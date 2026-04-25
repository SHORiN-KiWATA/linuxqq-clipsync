use chrono::Utc;
use memfd::MemfdOptions;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use xxhash_rust::xxh3::xxh3_128;

struct SyncState {
    last_dir: String,
    last_time: i64,
    last_sync_hash: u128,
}

const EMPTY_HASH: u128 = 0;

fn log(level: &str, msg: &str) {
    let now = Utc::now().format("%H:%M:%S").to_string();
    println!("[{}] [{}] {}", now, level, msg);
}

fn get_ms() -> i64 {
    Utc::now().timestamp_millis()
}

// 优化 1：零拷贝 Hash，拒绝为大图片分配多余内存
fn calc_hash(data: &[u8], process_mode: &str) -> u128 {
    if data.is_empty() {
        return EMPTY_HASH;
    }

    match process_mode {
        "uri-list" => {
            let s = String::from_utf8_lossy(data);
            let mut result = String::new();
            for line in s.lines() {
                let trimmed = line.trim();
                if trimmed == "copy" || trimmed == "cut" {
                    continue;
                }
                result.push_str(trimmed.trim_start_matches("file://"));
            }
            let processed = result.replace("\n", "").replace("\r", "").into_bytes();
            if processed.is_empty() {
                EMPTY_HASH
            } else {
                xxh3_128(&processed)
            }
        }
        "text" => {
            let s = String::from_utf8_lossy(data);
            let result: String = s
                .chars()
                .filter(|c| *c != '\0' && *c != '\n' && *c != '\r' && *c != ' ' && *c != '\t')
                .collect();
            let processed = result.into_bytes();
            if processed.is_empty() {
                EMPTY_HASH
            } else {
                xxh3_128(&processed)
            }
        }
        _ => xxh3_128(data), // 对于图片直接 Hash 原数组，绝对不 clone！
    }
}

// ==========================================
// 核心机制：无管道读取与写入 (基于 memfd)
// ==========================================

fn read_clipboard(cmd: &str, args: &[&str]) -> Vec<u8> {
    let Ok(mfd) = MemfdOptions::default().create("clip_read") else {
        return vec![];
    };
    let file = mfd.into_file();
    let Ok(file_out) = file.try_clone() else {
        return vec![];
    };

    if let Ok(mut child) = Command::new(cmd)
        .args(args)
        .stdout(Stdio::from(file_out))
        .stderr(Stdio::null())
        .spawn()
    {
        let _ = child.wait();
    }

    let mut data = Vec::new();
    let mut file_read = file;
    let _ = file_read.seek(SeekFrom::Start(0));
    let _ = file_read.read_to_end(&mut data);
    data
}

fn write_clipboard(cmd: &str, args: &[&str], data: &[u8]) -> bool {
    let Ok(mfd) = MemfdOptions::default().create("clip_write") else {
        return false;
    };
    let mut file = mfd.into_file();
    if file.write_all(data).is_err() {
        return false;
    }
    if file.seek(SeekFrom::Start(0)).is_err() {
        return false;
    }

    if let Ok(mut child) = Command::new(cmd)
        .args(args)
        .stdin(Stdio::from(file))
        .stderr(Stdio::null())
        .spawn()
    {
        return child.wait().map(|s| s.success()).unwrap_or(false);
    }
    false
}

fn get_xdg_runtime_dir() -> String {
    if let Ok(dir) = env::var("XDG_RUNTIME_DIR") {
        return dir;
    }
    format!("/run/user/{}", unsafe { libc::getuid() })
}

fn main() {
    let xdg_runtime_dir = get_xdg_runtime_dir();
    env::set_var("XDG_RUNTIME_DIR", &xdg_runtime_dir);

    if let Ok(home) = env::var("HOME") {
        env::set_var("XAUTHORITY", format!("{}/.Xauthority", home));
    }

    let mut wayland_display = env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let mut display = env::var("DISPLAY").unwrap_or_default();

    if wayland_display.is_empty() {
        if let Ok(entries) = fs::read_dir(&xdg_runtime_dir) {
            for entry in entries.flatten() {
                let file_name = entry.file_name().into_string().unwrap_or_default();
                if file_name.starts_with("wayland-") && !file_name.contains('.') {
                    if Command::new("wl-paste")
                        .env("WAYLAND_DISPLAY", &file_name)
                        .arg("--list-types")
                        .output()
                        .is_ok()
                    {
                        wayland_display = file_name;
                        break;
                    }
                }
            }
        }
    }

    if display.is_empty() {
        if let Ok(entries) = fs::read_dir("/tmp/.X11-unix") {
            for entry in entries.flatten() {
                let file_name = entry.file_name().into_string().unwrap_or_default();
                if file_name.starts_with('X') {
                    let test_d = format!(":{}", &file_name[1..]);
                    if Command::new("xclip")
                        .env("DISPLAY", &test_d)
                        .args(["-selection", "clipboard", "-t", "TARGETS", "-o"])
                        .output()
                        .is_ok()
                    {
                        display = test_d;
                        break;
                    }
                }
            }
        }
    }

    env::set_var("WAYLAND_DISPLAY", &wayland_display);
    env::set_var("DISPLAY", &display);

    log(
        "INIT",
        &format!(
            "探测结果: DISPLAY={}, WAYLAND_DISPLAY={}",
            display, wayland_display
        ),
    );

    if wayland_display.is_empty() || display.is_empty() {
        log("FATAL", "找不到存活的图形界面，退出...");
        std::process::exit(1);
    }

    let shared_state = Arc::new(Mutex::new(SyncState {
        last_dir: String::new(),
        last_time: 0,
        last_sync_hash: EMPTY_HASH,
    }));

    // ==========================================
    // X2W 线程
    // ==========================================
    let state_x2w = Arc::clone(&shared_state);
    thread::spawn(move || {
        log("INFO", "=== [X2W] 线程启动 ===");
        loop {
            let _ = Command::new("clipnotify").status();
            thread::sleep(Duration::from_millis(30));

            // 优化 2：【前置全局排他锁】在此处锁死状态，彻底杜绝并发竞争，利用短路求值拦截回音！
            let mut state = state_x2w.lock().unwrap();
            let now = get_ms();
            if state.last_dir == "W2X" && (now - state.last_time < 1000) {
                continue;
            }

            let types_raw =
                read_clipboard("xclip", &["-selection", "clipboard", "-t", "TARGETS", "-o"]);
            let types_str = String::from_utf8_lossy(&types_raw);

            let (source_mime, sync_mime, process_mode) =
                if types_str.contains("x-special/gnome-copied-files") {
                    ("x-special/gnome-copied-files", "text/uri-list", "uri-list")
                } else if types_str.contains("application/x-qt-image")
                    || types_str.contains("text/uri-list")
                {
                    ("text/uri-list", "text/uri-list", "uri-list")
                } else if types_str.contains("image/png") {
                    ("image/png", "image/png", "raw")
                } else if types_str.contains("image/jpeg") {
                    ("image/jpeg", "image/jpeg", "raw")
                } else if types_str.contains("text/plain;charset=utf-8") {
                    ("text/plain;charset=utf-8", "text/plain", "text") 
                } else if types_str.contains("UTF8_STRING") {
                    ("UTF8_STRING", "text/plain", "text")
                } else if types_str.contains("text/plain") {
                    ("text/plain", "text/plain", "text")
                } else if types_str.contains("text/html") {
                    ("text/html", "text/html", "raw")
                } else {
                    continue;
                };

            let x_data = read_clipboard("xclip", &["-sel", "clip", "-o", "-t", source_mime]);
            let current_hash = calc_hash(&x_data, process_mode);
            if current_hash == EMPTY_HASH {
                continue;
            }

            // 优化 3：移除极其冗余的二次目标查壳（w_check_data），直接依靠记录的 hash 防环
            if current_hash == state.last_sync_hash {
                continue;
            }

            log(
                "X2W",
                &format!(
                    "写入 Wayland... (Hash: {:08x})",
                    (current_hash >> 96) as u32
                ),
            );
            state.last_dir = "X2W".to_string();
            state.last_time = get_ms();

            let write_data = if process_mode == "uri-list" {
                let s = String::from_utf8_lossy(&x_data);
                let mut res = String::new();
                for line in s.lines() {
                    if line == "copy" || line == "cut" {
                        continue;
                    }
                    if line.starts_with('/') {
                        res.push_str("file:///");
                        res.push_str(&line[1..]);
                    } else {
                        res.push_str(line);
                    }
                    res.push('\n');
                }
                res.into_bytes()
            } else {
                x_data
            };

            if write_clipboard("wl-copy", &["-t", sync_mime], &write_data) {
                state.last_sync_hash = current_hash;
            }
        }
    });

    log("INFO", "=== [W2X] 线程启动 ===");
    log("SYS", "双向剪贴板同步服务已准备就绪！");

    // ==========================================
    // W2X 主线程
    // ==========================================
    let mut wl_watch = Command::new("wl-paste")
        .args(["--watch", "echo"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to start wl-paste --watch");

    let stdout = wl_watch.stdout.take().unwrap();
    let reader = BufReader::new(stdout);

    for _line in reader.lines() {
        thread::sleep(Duration::from_millis(30));

        // 优化 2：【前置全局排他锁】同样提到最前面，防止 XWayland 带来的回音击穿
        let mut state = shared_state.lock().unwrap();
        let now = get_ms();
        if state.last_dir == "X2W" && (now - state.last_time < 1000) {
            continue;
        }

        let types_raw = read_clipboard("wl-paste", &["--list-types"]);
        let types_str = String::from_utf8_lossy(&types_raw);

        let (sync_mime, process_mode) = if types_str.contains("application/x-qt-image")
            || types_str.contains("text/uri-list")
        {
            ("text/uri-list", "uri-list")
        } else if types_str.contains("image/png") {
            ("image/png", "raw")
        } else if types_str.contains("image/jpeg") {
            ("image/jpeg", "raw")
        } else if types_str.contains("text/plain") {
            ("text/plain", "text")
        } else if types_str.contains("text/html") {
            ("text/html", "raw")
        } else {
            continue;
        };

        let w_data = read_clipboard("wl-paste", &["-t", sync_mime]);
        let current_hash = calc_hash(&w_data, process_mode);
        if current_hash == EMPTY_HASH {
            continue;
        }

        // 优化 3：移除 x_check_data 的大量多余 IO。
        if current_hash == state.last_sync_hash {
            continue;
        }

        log(
            "W2X",
            &format!("写入 X11... (Hash: {:08x})", (current_hash >> 96) as u32),
        );
        state.last_dir = "W2X".to_string();
        state.last_time = get_ms();

        let write_data = if process_mode == "uri-list" {
            let s = String::from_utf8_lossy(&w_data);
            let mut res = String::new();
            for line in s.lines() {
                if line == "copy" || line == "cut" {
                    continue;
                }
                if line.starts_with('/') {
                    res.push_str("file:///");
                    res.push_str(&line[1..]);
                } else {
                    res.push_str(line);
                }
                res.push('\n');
            }
            res.into_bytes()
        } else {
            w_data
        };

        let target_t = if process_mode == "text" {
            "UTF8_STRING"
        } else {
            sync_mime
        };

        if write_clipboard(
            "xclip",
            &["-sel", "clip", "-i", "-t", target_t],
            &write_data,
        ) {
            state.last_sync_hash = current_hash;
        }
    }

    log("ERROR", "W2X 监听意外终止，触发退出...");
    std::process::exit(1);
}
