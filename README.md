# Linuxqq-Clipsync

> [!IMPORTANT]
> **本项目已停止维护，由 [linuxqq-wayland-fix](https://github.com/SHORiN-KiWATA/linuxqq-wayland-fix) 接替**，它同时修复了 QQ 在 Wayland 下的剪贴板、屏幕共享和共享电脑声音。
>
> 迁移：
>
> ```
> systemctl --user disable --now linuxqq-clipsync
> yay -S linuxqq-wayland-fix-git
> ```
>
> AUR 上的 `linuxqq-clipsync-git` 已改为过渡包，正常更新也会自动装上 `linuxqq-wayland-fix-git`。之后从应用菜单打开「QQ（Wayland修复版）」即可。

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

