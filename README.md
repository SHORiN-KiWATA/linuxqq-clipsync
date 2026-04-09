# Clipsync

Clipboard synchronization sctipts for Wayland Compositors ( X11 <--> Wayland ).

WARN: Only tested on Niri and Hyprland.

- Dependencies

    `xclip` `wl-clipboard` `clipnotify`

- Installation

    ```
    yay -S clipsync-git
    ```

- Usage

    ```
    systemctl enable --user clipsync
    ```

    It's also recommend to use my clipboard tui with these scrtips:[shorinclip](https://github.com/SHORiN-KiWATA/shorinclip)

- Features

    This project contains three scripts and one service.

    `clipsync-x2w` for X11 to Wayland synchronization.

    `clipsync-w2x` for Wayland to X11.

    `clipsync` manage these two sctips.

    `clipsync.service` which will run `clipsync` automatically.

