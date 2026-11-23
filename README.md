# Meridian

A Wayland desktop environment and window manager for Arch Linux, written in Rust
on top of [Smithay](https://github.com/Smithay/smithay). The goal: the structure
of macOS (global menu bar, dock, overview) with the friendliness of Zorin OS.

**Status: phase 5 of 6.** The compositor runs nested in a window or as a real
session from a TTY. It has rounded windows with shadows, four spaces, an
overview, a top bar, an optional dock, the Find launcher and notifications.
The settings app, lock screen and desktop icons are still to come. The
real-session backend is new and has had little hardware testing.

![Meridian running nested](docs/design/phase-5-desktop.png)

The look follows the "Terra" mockups in `docs/design/`.

## Install (Arch Linux)

```sh
git clone https://github.com/BrentWebBE/Meridian.git meridian && cd meridian/packaging/arch
makepkg -si          # builds the package and installs it with pacman
```

Remove it again with `sudo pacman -R meridian-git`.

## Run

Nested, inside your current desktop (safe for trying things out):

```sh
meridian-comp                      # or: cargo run -p meridian-comp
meridian-comp alacritty            # start with a client
```

As a real session: switch to a text console (Ctrl+Alt+F3), log in, and run

```sh
meridian-comp 2> ~/meridian.log
```

or pick "Meridian" in your login manager. On a hybrid-graphics laptop you can
choose the GPU with `MERIDIAN_DRM_DEVICE=/dev/dri/cardN`.

| Keys                 | Action                              |
|----------------------|-------------------------------------|
| Mod+Return           | open a terminal                     |
| Mod+Q                | close the focused window            |
| Super (tap) or Mod+Space | open or close Find              |
| Mod+O                | overview: thumbnails and spaces     |
| Mod+D                | keyboard to the dock (arrows, Enter, Esc) |
| Mod+L                | lock the screen                     |
| Mod+1 .. Mod+4       | switch space (add Shift: move window) |
| Mod+T                | toggle floating / tiled             |
| Mod+Tab              | focus next window (Shift: previous) |
| Mod+left-drag        | move a window (or drag its title bar) |
| Mod+right-drag       | resize a window (or drag its border) |
| Print                | screenshot to ~/Pictures/Screenshots |
| Mod+Shift+E          | quit                                |
| Ctrl+Alt+F1..F12     | switch virtual terminal             |

`Mod` is **Super** in a real session and **Alt** when nested.

## Find

Tap Super (or Mod+Space, or the logo button) and type. Find shows, in groups:

- **Apps** from `.desktop` files, matched fuzzily: `ffx` finds Firefox.
- **Actions**: overview, terminal, screenshot, layout, spaces, log out,
  sleep, restart, shut down.
- **Settings**: the three moods and "Follow the time of day".
- **Files** by name from your home folder. Hidden folders and build folders
  (`target`, `node_modules`, ...) are skipped; the index holds at most
  60,000 files and is refreshed at most every two minutes.

Up and Down move the highlight, Enter opens it, Tab completes the field to
it, Esc closes. The strip at the bottom lists running apps.

## Lock screen

Mod+L (or "Lock the screen" in Find) starts `meridian-lock`. The compositor
then shows only the lock screen and sends input nowhere else until the
password is accepted. If the lock program crashes, the session stays locked
and the program is started again.

Passwords are checked with PAM using `/etc/pam.d/meridian-lock`, which the
package installs. Without that file `meridian-lock` refuses to lock, because
no password could ever be accepted.

If you cannot get back in (for example after changing PAM rules), switch to
another console with Ctrl+Alt+F3, log in there and run `pkill meridian-comp`.
That ends the Meridian session.

## Configure

Copy `/usr/share/doc/meridian-git/config.toml` (or
`meridian-common/resources/default-config.toml`) to
`~/.config/meridian/config.toml`. It covers the mood, wallpaper, start layout,
terminal, autostart programs, keyboard layout, output scale, tiling gaps,
pinned dock apps and keybindings.

### Moods

Terra has three moods: Morning, Dusk and Night. By default the mood follows
the clock (morning from 6:00, dusk from 17:00, night from 21:00). To pin one:

```toml
[appearance]
theme = "dusk"
follow-time-of-day = false
wallpaper = "~/Pictures/photo.jpg"   # optional; empty = the drawn landscape
```

To change colors, copy `/usr/share/doc/meridian-git/themes.toml` to
`~/.config/meridian/themes.toml` and edit the tokens you want.

Changes apply as soon as the file is saved: the compositor and the shell
programs watch it (inotify). The Settings app writes the same file. Keyboard
layout and output scale still need a restart.

### Launching apps

```toml
[dock]
mode = "dock"          # "dock", "auto-hide" or "off" (Find only)
position = "bottom"    # "bottom", "left" or "right"
running-dots = true
```

## Layout

| Crate             | Purpose                                              |
|-------------------|------------------------------------------------------|
| `meridian-comp`   | the compositor                                       |
| `meridian-bar`    | top bar (GTK 4, layer-shell)                         |
| `meridian-dock`   | dock (GTK 4, layer-shell)                            |
| `meridian-launcher` | Find: search apps and recent files, running apps, power |
| `meridian-notify` | notification daemon (`org.freedesktop.Notifications`) |
| `meridian-settings` | Settings app: Appearance and Launching apps         |
| `meridian-desktop` | desktop icons: Home, Documents, Projects, Trash      |
| `meridian-lock`   | lock screen (ext-session-lock-v1, PAM)               |
| `meridian-common` | shared config, theme tokens and the IPC protocol     |

The shell programs are ordinary Wayland clients. They learn about windows from
the compositor over a small JSON socket (`$MERIDIAN_SOCKET`, see
`meridian-common/src/ipc.rs`). The settings app is added in a later phase.

Try a notification with `notify-send "Hello" "from Meridian"`. Only one
notification daemon can run per login session, so inside GNOME or KDE (nested
mode) `meridian-notify` exits and the host desktop keeps showing them.

Building needs `gtk4` and `gtk4-layer-shell` installed (`makepkg -s` does that).

## License

MIT
