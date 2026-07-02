use chrono::Utc;
use memfd::MemfdOptions;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use xxhash_rust::xxh3::xxh3_128;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    X2W,
    W2X,
}

impl Direction {
    fn tag(self) -> &'static str {
        match self {
            Direction::X2W => "X2W",
            Direction::W2X => "W2X",
        }
    }

    // 目标侧名称，仅用于日志
    fn target(self) -> &'static str {
        match self {
            Direction::X2W => "Wayland",
            Direction::W2X => "X11",
        }
    }

    fn opposite(self) -> Direction {
        match self {
            Direction::X2W => Direction::W2X,
            Direction::W2X => Direction::X2W,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProcessMode {
    UriList,
    Text,
    Raw,
}

struct SyncState {
    last_dir: Option<Direction>,
    last_time: Option<Instant>,
    last_sync_hash: Option<u128>,
}

// 一个同步方向的注入配置：触发差异全部收敛到函数指针，循环体共用 handle_change。
// pick 返回 (读取类型, 写入类型, 处理模式)；两个方向各自的 MIME 表保留，因为源/目标
// 表示本就不对称（X 侧特有 gnome-copied-files / UTF8_STRING 源类型等）。
#[derive(Clone, Copy)]
struct SyncConfig {
    dir: Direction,
    list_types: fn() -> Vec<u8>,
    pick: fn(&str) -> Option<(&'static str, &'static str, ProcessMode)>,
    read_data: fn(&str) -> Vec<u8>,
    write_data: fn(&str, &[u8]) -> bool,
}

fn log(level: &str, msg: &str) {
    let now = Utc::now().format("%H:%M:%S").to_string();
    println!("[{}] [{}] {}", now, level, msg);
}

// 回音窗口阈值：刚被对向写入 1 秒内的变化视为回音，忽略
const ECHO_WINDOW: Duration = Duration::from_secs(1);

// 优化 1：零拷贝 Hash，拒绝为大图片分配多余内存
fn calc_hash(data: &[u8], process_mode: ProcessMode) -> Option<u128> {
    if data.is_empty() {
        return None;
    }

    match process_mode {
        ProcessMode::UriList => {
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
                None
            } else {
                Some(xxh3_128(&processed))
            }
        }
        ProcessMode::Text => {
            let s = String::from_utf8_lossy(data);
            let result: String = s
                .chars()
                .filter(|c| *c != '\0' && *c != '\n' && *c != '\r' && *c != ' ' && *c != '\t')
                .collect();
            let processed = result.into_bytes();
            if processed.is_empty() {
                None
            } else {
                Some(xxh3_128(&processed))
            }
        }
        ProcessMode::Raw => Some(xxh3_128(data)), // 对于图片直接 Hash 原数组，绝对不 clone！
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

// uri-list 规范化：丢弃 copy/cut 行，裸路径补成 file:/// 形式
fn rewrite_uri_list(data: &[u8]) -> Vec<u8> {
    let s = String::from_utf8_lossy(data);
    let mut res = String::new();
    for line in s.lines() {
        if line == "copy" || line == "cut" {
            continue;
        }
        if let Some(rest) = line.strip_prefix('/') {
            res.push_str("file:///");
            res.push_str(rest);
        } else {
            res.push_str(line);
        }
        res.push('\n');
    }
    res.into_bytes()
}

// X → Wayland 的类型选择。读取类型是 X 侧的源表示，写入类型是 Wayland 侧目标。
fn pick_x2w(types: &str) -> Option<(&'static str, &'static str, ProcessMode)> {
    let plan = if types.contains("x-special/gnome-copied-files") {
        (
            "x-special/gnome-copied-files",
            "text/uri-list",
            ProcessMode::UriList,
        )
    } else if types.contains("application/x-qt-image") || types.contains("text/uri-list") {
        ("text/uri-list", "text/uri-list", ProcessMode::UriList)
    } else if types.contains("image/png") {
        ("image/png", "image/png", ProcessMode::Raw)
    } else if types.contains("image/jpeg") {
        ("image/jpeg", "image/jpeg", ProcessMode::Raw)
    } else if types.contains("text/plain;charset=utf-8") {
        ("text/plain;charset=utf-8", "text/plain", ProcessMode::Text)
    } else if types.contains("UTF8_STRING") {
        ("UTF8_STRING", "text/plain", ProcessMode::Text)
    } else if types.contains("text/plain") {
        ("text/plain", "text/plain", ProcessMode::Text)
    } else if types.contains("text/html") {
        ("text/html", "text/html", ProcessMode::Raw)
    } else {
        return None;
    };
    Some(plan)
}

// Wayland → X 的类型选择。读取类型即 Wayland 源类型；写入 xclip 时文本统一用 UTF8_STRING。
fn pick_w2x(types: &str) -> Option<(&'static str, &'static str, ProcessMode)> {
    let (read_type, mode) =
        if types.contains("application/x-qt-image") || types.contains("text/uri-list") {
            ("text/uri-list", ProcessMode::UriList)
        } else if types.contains("image/png") {
            ("image/png", ProcessMode::Raw)
        } else if types.contains("image/jpeg") {
            ("image/jpeg", ProcessMode::Raw)
        } else if types.contains("text/plain;charset=utf-8") {
            ("text/plain;charset=utf-8", ProcessMode::Text)
        } else if types.contains("text/plain") {
            ("text/plain", ProcessMode::Text)
        } else if types.contains("text/html") {
            ("text/html", ProcessMode::Raw)
        } else {
            return None;
        };
    let write_type = match read_type {
        "text/plain;charset=utf-8" | "text/plain" => "UTF8_STRING",
        other => other,
    };
    Some((read_type, write_type, mode))
}

// 单次剪贴板变化的共用处理：回音抑制 → 选型 → 读取 → 去重 → 写入 → 记录。
//
// 注意：锁全程持有（含读写剪贴板的子进程阻塞）是**有意**的，不是性能疏忽。
// 它保证「置 last_sync_hash 先于任何回音被处理」这一步原子完成，hash 防环
// 因而绝对可靠。剪贴板是人手速操作，两方向几乎不争锁，宽锁无吞吐代价。
fn handle_change(state: &Mutex<SyncState>, cfg: &SyncConfig) {
    // 前置排他锁，利用短路求值拦截回音
    let mut st = state.lock().unwrap();
    if st.last_dir == Some(cfg.dir.opposite())
        && st.last_time.is_some_and(|t| t.elapsed() < ECHO_WINDOW)
    {
        return;
    }

    let types_raw = (cfg.list_types)();
    let types_str = String::from_utf8_lossy(&types_raw);
    let Some((read_type, write_type, mode)) = (cfg.pick)(&types_str) else {
        return;
    };

    let data = (cfg.read_data)(read_type);
    let Some(current_hash) = calc_hash(&data, mode) else {
        return;
    };

    // 优化 3：直接依靠记录的 hash 防环，不做二次目标查壳
    if st.last_sync_hash == Some(current_hash) {
        return;
    }

    log(
        cfg.dir.tag(),
        &format!(
            "写入 {}... (Hash: {:08x})",
            cfg.dir.target(),
            (current_hash >> 96) as u32
        ),
    );
    let payload = if mode == ProcessMode::UriList {
        rewrite_uri_list(&data)
    } else {
        data
    };

    // 锁全程持有，对向的回音事件在写入期间阻塞在锁外，因此状态在写成功后
    // 置位依然先于任何回音被处理；写失败则不占用回音窗，不再无谓压制对向 1 秒。
    if (cfg.write_data)(write_type, &payload) {
        st.last_dir = Some(cfg.dir);
        st.last_time = Some(Instant::now());
        st.last_sync_hash = Some(current_hash);
    }
}

// 扫描 dir 下的候选套接字文件，返回第一个能真正建立连接的 display 名。
// 直接 connect 探活：此前用子进程 output().is_ok() 只能证明命令启动成功，
// 连不上服务器与剪贴板为空同样非零退出，无法区分，探测形同虚设。
fn probe_display(dir: &str, to_display: impl Fn(&str) -> Option<String>) -> Option<String> {
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let file_name = entry.file_name().into_string().unwrap_or_default();
        let Some(display) = to_display(&file_name) else {
            continue;
        };
        if UnixStream::connect(entry.path()).is_ok() {
            return Some(display);
        }
    }
    None
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

    if env::var("XAUTHORITY").is_err() {
        if let Ok(home) = env::var("HOME") {
            let candidate = format!("{}/.Xauthority", home);
            if std::path::Path::new(&candidate).exists() {
                env::set_var("XAUTHORITY", candidate);
            }
        }
    }

    let mut wayland_display = env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let mut display = env::var("DISPLAY").unwrap_or_default();

    if wayland_display.is_empty() {
        wayland_display = probe_display(&xdg_runtime_dir, |f| {
            (f.starts_with("wayland-") && !f.contains('.')).then(|| f.to_string())
        })
        .unwrap_or_default();
    }

    if display.is_empty() {
        display = probe_display("/tmp/.X11-unix", |f| {
            f.strip_prefix('X').map(|n| format!(":{n}"))
        })
        .unwrap_or_default();
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
        last_dir: None,
        last_time: None,
        last_sync_hash: None,
    }));

    // ==========================================
    // X2W 线程
    // ==========================================
    let x2w_cfg = SyncConfig {
        dir: Direction::X2W,
        list_types: || read_clipboard("xclip", &["-selection", "clipboard", "-t", "TARGETS", "-o"]),
        pick: pick_x2w,
        read_data: |t| read_clipboard("xclip", &["-sel", "clip", "-o", "-t", t]),
        write_data: |t, d| write_clipboard("wl-copy", &["-t", t], d),
    };
    let state_x2w = Arc::clone(&shared_state);
    thread::spawn(move || {
        log("INFO", "=== [X2W] 线程启动 ===");
        loop {
            let _ = Command::new("clipnotify").status();
            thread::sleep(Duration::from_millis(30));
            handle_change(&state_x2w, &x2w_cfg);
        }
    });

    log("INFO", "=== [W2X] 线程启动 ===");
    log("SYS", "双向剪贴板同步服务已准备就绪！");

    // ==========================================
    // W2X 主线程
    // ==========================================
    let w2x_cfg = SyncConfig {
        dir: Direction::W2X,
        list_types: || read_clipboard("wl-paste", &["--list-types"]),
        pick: pick_w2x,
        read_data: |t| read_clipboard("wl-paste", &["-n", "-t", t]),
        write_data: |t, d| write_clipboard("xclip", &["-sel", "clip", "-i", "-t", t], d),
    };

    let mut wl_watch = Command::new("wl-paste")
        .args(["--watch", "echo"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to start wl-paste --watch");

    let stdout = wl_watch.stdout.take().unwrap();
    let reader = BufReader::new(stdout);

    for _line in reader.lines() {
        thread::sleep(Duration::from_millis(30));
        handle_change(&shared_state, &w2x_cfg);
    }

    log("ERROR", "W2X 监听意外终止，触发退出...");
    std::process::exit(1);
}
