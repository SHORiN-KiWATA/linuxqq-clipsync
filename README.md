# Linuxqq-Clipsync

通过同步 X11 和 Wayland 剪贴板的方式修复 Linuxqq 以 Wayland 运行时的剪贴板异常。

- 依赖

    `xclip` `wl-clipboard` `clipnotify`

- 安装

    ```
    yay -S linuxqq-clipsync-git
    ```

- 使用

    运行`linuxqq-clipsync`命令即可，但是更推荐使用systemd服务。 
    
    ```
    systemctl enable --user linuxqq-clipsync
    ```

