# Linuxqq-Clipsync

> [!IMPORTANT]
> **本项目已停止维护，由 [linuxqq-wayland-clipboard-fix](https://github.com/SHORiN-KiWATA/linuxqq-wayland-clipboard-fix) 接替。**
>
> 新项目在 QQ 进程内直接桥接 X11 与 Wayland 剪贴板：按需传输、没有同步空窗期、保留全部格式，也不再依赖 xclip / wl-clipboard / clipnotify。
>
> 迁移：
>
> ```
> systemctl --user disable --now linuxqq-clipsync
> paru -S linuxqq-wayland-clipboard-fix-git   # 会提示替换 linuxqq-clipsync-git
> ```
>
> 之后从应用菜单打开「QQ（剪贴板修复）」即可。

通过同步 X11 和 Wayland 剪贴板的方式修复 Linuxqq 以 Wayland 运行时的剪贴板异常。

## 依赖

`xclip` `wl-clipboard` `clipnotify`

## 安装

- Arch Linux

    ```
    yay -S linuxqq-clipsync-git
    ```

- 其他发行版

    ```
    You'll figure it out.
    ```

## 使用方法

运行`linuxqq-clipsync`命令即可，也可以使用systemd服务。 

```
systemctl enable --user linuxqq-clipsync
```

