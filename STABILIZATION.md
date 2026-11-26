# Stabilization log

Running record of the stabilization phase: what was found, what was changed,
what was measured before and after, and what is still broken.

Rules for this file: a number is only written down if it was measured; a
feature is only called working if it was run and seen working. Anything else
is marked **UNVERIFIED** with the command that would verify it.

Test machine: ASUS laptop, AMD Radeon 890M (internal panel, `eDP-1`) plus
NVIDIA RTX 5070 Mobile (external `DP` output), Arch Linux, GNOME on Wayland as
the host session. All measurements so far are from **nested mode** (Meridian
running as a window inside GNOME). The real session (DRM/KMS on a TTY) cannot
be started from the development session, so nothing about it is measured yet.

## Step 0: audit and baseline

Status: audit done, baseline measured in nested mode, no behavior changed.
The only code added is the statistics facility (off by default).

### 0.1 Release build

| Check | Result |
|---|---|
| `cargo build --release` | 0 warnings |
| `cargo clippy --release --all-targets` | 0 warnings |
| `cargo test` | 35 passed, 0 failed |
| `meridian-comp` release binary | 13.9 MB |

### 0.2 Processes the session starts

| Process | Started by | How | Restarted if it dies |
|---|---|---|---|
| `meridian-comp` | login manager (`/usr/share/wayland-sessions/meridian.desktop`, `Exec=meridian-comp`) or by hand on a TTY | directly, no session script | no |
| `systemctl --user import-environment`, `dbus-update-activation-environment` | compositor, real session only | `Command::spawn` at startup | n/a |
| `meridian-bar` | compositor (`autostart` in config) | `Command::spawn`, child process | **no** |
| `meridian-dock` | compositor (`autostart`) | same | **no** |
| `meridian-desktop` | compositor (`autostart`) | same | **no** |
| `meridian-notify` | compositor (`autostart`) | same | **no** |
| `meridian-launcher --background` | compositor (`autostart`) | same | **no** |
| `meridian-launcher` (second copy) | compositor, on Super / Mod+Space / logo button | spawn; passes "activate" to the first copy and exits | n/a |
| `meridian-lock` | compositor, on Mod+L or the Find action | spawn | yes, while locked (1 s watchdog) |
| `meridian-settings` | Find or dock | GIO app launch from the shell program | no |
| terminal and other apps | compositor keybinding, Find, dock | spawn / GIO launch | no |

Nothing is a systemd unit. `graphical-session.target` is never started, so
services that are `PartOf=`/`WantedBy=graphical-session.target` (portals'
backends, polkit agents, and so on) do not start with a Meridian session.

### 0.3 Session services

Checked on the installed system (`meridian-git 0.12.0`) and in the code.
"In a Meridian session" means what would be true there; items that could only
be checked in the GNOME host session say so.

| Item | Status | Evidence |
|---|---|---|
| systemd user session integration | **MISSING** | No unit files are shipped (`pacman -Ql meridian-git` lists none). `graphical-session.target` is not started. The compositor does run `systemctl --user import-environment WAYLAND_DISPLAY XDG_CURRENT_DESKTOP` and `dbus-update-activation-environment --systemd` (real session only), but `XDG_SESSION_TYPE` and others are not imported, and nothing waits for the import before starting shell programs. UNVERIFIED on a real session. |
| Session `.desktop` | **PRESENT**, login path **UNVERIFIED** | `/usr/share/wayland-sessions/meridian.desktop` is installed; GDM is the active display manager. Nobody has logged in through it: the journal has no `meridian` entries from any boot. Verify: pick "Terra (Meridian)" in GDM, then `journalctl --user -b \| grep meridian`. |
| Notification daemon | **PRESENT**, partial | `meridian-notify` owns `org.freedesktop.Notifications` when no other daemon does (seen on a private bus: two toasts shown, `GetServerInformation` answered). Missing: actions (`ActionInvoked`), urgency, capabilities beyond `body`. |
| xdg-desktop-portal + backends | **BROKEN** in a Meridian session | `xdg-desktop-portal`, `-gtk` and `-gnome` are installed, but there is no `meridian-portals.conf`, and `gtk.portal` / `gnome.portal` say `UseIn=gnome`. With `XDG_CURRENT_DESKTOP=Meridian` no backend is selected. No screenshot or screencast portal backend exists for this compositor at all (it implements no screencopy protocol). |
| PipeWire / WirePlumber | **PRESENT** (system), start in session **UNVERIFIED** | `pipewire`, `wireplumber`, `pipewire-pulse` user services are active in the host session; they are socket-activated and not tied to a desktop. The bar reads volume through `wpctl`. |
| polkit authentication agent | **MISSING** | `polkit-kde-agent` is installed but nothing starts an agent in a Meridian session. `pkexec` would fail to prompt. |
| Keyring / Secret portal | **MISSING** in a Meridian session | `gnome-keyring` is installed and owns `org.freedesktop.secrets` in the host session; nothing starts or unlocks it for Meridian, and no portals.conf routes the Secret portal. |
| Tray (StatusNotifierWatcher + host) | **MISSING** | No owner of `org.kde.StatusNotifierWatcher` in our code; the bar has no tray host. |
| Clipboard persistence | **MISSING** | The compositor does not implement `ext-data-control` or `wlr-data-control`, so no clipboard manager can work. A selection dies with its source app. |
| XWayland | **MISSING** | `xorg-xwayland` is installed, `xwayland-satellite` is not, and the compositor starts neither. `DISPLAY` is removed from child environments, so X11-only apps fail to start. |
| qt5-wayland / qt6-wayland | **PRESENT** | `qt5-wayland 5.15.19`, `qt6-wayland 6.12.0` installed. Not a dependency of our package. |

### 0.4 Statistics facility

Added in `meridian-comp/src/stats.rs`. Off by default: when off it holds
`None`, every hook is one branch, no clock is read and no timer exists.

- `MERIDIAN_STATS=1 meridian-comp` logs one line per second.
- `MERIDIAN_STATS=/path/stats.csv meridian-comp` appends CSV rows.

Per second it reports: redraws that showed a frame, redraws that found nothing
changed, frame time p50 / p95 / max, frame callbacks sent, number of surfaces
(windows + layer surfaces), resident memory, compositor CPU use, and the
reasons redraws were requested.

"Frame time" is the CPU time from starting to build the frame until it is
handed to the display (element list, off-screen window textures, render,
submit). It does not include the GPU finishing the frame or the wait for the
monitor.

Scripts: `scripts/cpu-sample.sh` (CPU and memory of named processes over a
period, read from `/proc`, independent of the facility) and
`scripts/baseline-nested.sh` (the nested baseline below).

### 0.5 Baseline (nested mode, release build, `main` at 05c20e6)

Run with `scripts/baseline-nested.sh`: one terminal open, default config,
host refresh rate 120 Hz. The host window was in use from second 36 of the
run, so "idle" numbers come from the 26 clean seconds before that; the
60-second `/proc` sample is given too but includes that activity.

| Measure | Value | Source |
|---|---|---|
| Redraws per second, idle | 119 | stats, seconds 10-35 |
| Frame callbacks sent per second, idle | 540 | stats |
| Frame time p50 / p95, idle | 0.23 ms / 0.42 ms | stats |
| Longest single frame in the run | 23.5 ms | stats (during use) |
| Compositor CPU, 60 s | 3.53% | `/proc`, includes some use |
| Compositor CPU, idle | 4.6% | stats, seconds 10-35 |
| Compositor CPU, locked, 15 s | 2.67% (119 redraws/s) | `/proc` + stats, separate run |
| Compositor CPU, host window hidden | 0.0% (0 redraws/s) | stats, 232 s |
| Compositor memory after 9 min | RSS 283 MB, unchanged from start | stats |
| Compositor memory, proportional | PSS 56 MB, anonymous 30 MB | `/proc/PID/smaps_rollup` |
| `meridian-bar` | CPU 1.72%, RSS 329 MB, PSS 64 MB | `/proc` |
| `meridian-dock` | CPU 0.28%, RSS 328 MB | `/proc` |
| `meridian-launcher` | CPU 0.60%, RSS 346 MB (73 MB while it has never been shown) | `/proc` |
| `meridian-desktop` | RSS 323 MB | `ps` |
| `meridian-settings` | RSS 330 MB | `ps` |

Reading the memory numbers: RSS counts shared GPU-driver and library
mappings in full for every process. PSS divides shared pages among their
users and is the fairer figure: about 56 to 64 MB per process. A shell
program that has not yet shown a window (no GL context) sits at 73 MB RSS.

The run was stopped by hand at 9 minutes instead of 10 (the script was
edited while it ran, which can derail a running shell script); the locked
measurement was taken in a separate short run.

Not measured, and why:

| Wanted | Why not | How to measure |
|---|---|---|
| CPU and frame time while moving a window | Nothing in the development session can inject pointer input into the nested window. | `MERIDIAN_STATS=$HOME/drag.csv meridian-comp alacritty`, drag the window for 10 s, quit, read the `frame_p50_ms` / `frame_p95_ms` columns. |
| Unlock part of a lock/unlock cycle | Typing the password cannot be scripted, and trying passwords against the real account is off limits. | Lock with Alt+L, unlock by hand, compare `cpu_percent` rows before and after in the CSV. |
| Anything in the real session | No seat or TTY in the development session. | Ctrl+Alt+F3, log in, `MERIDIAN_STATS=$HOME/tty.csv meridian-comp 2> ~/meridian.log`, leave it idle for 60 s, drag a window, quit with Super+Shift+E. |

### 0.6 Findings from reading the code

These are not yet confirmed by measurement unless noted.

1. **Real-session render loop polls.** `tty_render` falls back to a 16 ms
   timer whenever a frame has no changes, and frame callbacks are sent from
   that same path. After every shown frame the next draw finds nothing new,
   so an animating client gets roughly one frame per (refresh interval + 16 ms).
   On a 120 Hz panel that is about 40 frames per second, and the cursor can
   lag by up to 16 ms. UNVERIFIED on hardware; a rewrite exists on the
   unmerged branch `smooth-redraw` and is the candidate for Step 2.
2. **Nested render loop draws continuously.** Every host frame triggers a
   full redraw with buffer age 0 (no damage tracking) and requests the next
   one. Confirmed by the baseline: see the redraws-per-second row.
3. **Effects are always on.** Every window is rendered into an off-screen
   texture and drawn through the rounded-corner shader on each redraw; there
   is no configuration flag to turn shadows, corners or title bars off.
4. **Wallpaper is a full-screen shader**, evaluated for every damaged pixel
   on every redraw rather than drawn once into a texture.
5. **Blocking work on the main loop:** process spawning (`Command::spawn`),
   config and theme reload (file reads and TOML parsing), screenshots (GPU
   read-back, PNG encoding and file write), cursor theme loading at startup.
   Wallpaper image decoding is already on a worker thread.
6. **Panic sites reachable while running:** 10 `unwrap`/`expect` calls in the
   compositor outside tests (6 in `input.rs`, 1 each in `state.rs`,
   `handlers/layer_shell.rs`, `handlers/compositor.rs`, `backend/mod.rs`).
   There is no panic hook.
7. **Shell programs are plain child processes** with no restart; if the bar
   or dock crashes it stays gone until the session restarts.
8. **Popup grabs are not implemented**, so menus do not close when clicking
   outside them.

### 0.7 Protocols (for Step 6)

Present: `wl_compositor`/subcompositor, `wl_shm`, `linux-dmabuf`, `wl_seat`
(keyboard, pointer), `wl_data_device`, `wl_output` + `xdg-output`,
`xdg-shell` (no popup grabs), `xdg-decoration`, `wlr-layer-shell`,
`ext-session-lock`.

Missing: fractional-scale, viewporter, xdg-activation, text-input /
input-method, idle-notify, idle-inhibit, data-control, primary-selection,
cursor-shape, pointer-constraints, relative-pointer, foreign-toplevel (the
dock uses our own IPC socket instead), output-management, presentation-time,
single-pixel-buffer, xdg-foreign.

## Step 1: session and startup

Status: written, partly verified (see the table). The real login path is
**UNVERIFIED**; it needs a real session.

### What changed

- **`meridian-session`** (`/usr/bin`): the session script, used by the login
  manager entry and usable from a text console. It sets `XDG_CURRENT_DESKTOP`,
  `XDG_SESSION_DESKTOP`, `XDG_SESSION_TYPE` and `QT_QPA_PLATFORM`, runs
  `meridian-comp --session`, writes the compositor's log to
  `~/.local/state/meridian/session.log` (previous one kept as `.old`), and on
  exit stops the session target and removes our variables from systemd.
- **`meridian-comp --session`**: after its sockets exist it runs
  `dbus-update-activation-environment --systemd` with `WAYLAND_DISPLAY`,
  `XDG_CURRENT_DESKTOP`, `XDG_SESSION_TYPE`, `XDG_SESSION_DESKTOP`,
  `MERIDIAN_SOCKET` and `QT_QPA_PLATFORM`, then
  `systemctl --user start meridian-session.target`. Both run on a worker
  thread; the result returns through a calloop channel. If either fails, the
  compositor starts the shell programs itself, as before.
- **systemd user units** (`/usr/lib/systemd/user`): `meridian-session.target`
  (`BindsTo=graphical-session.target`) and one service each for the bar,
  dock, desktop icons, notification daemon and Find, with
  `PartOf=graphical-session.target`, `Restart=on-failure`, `RestartSec=1`.
- **Nested mode is labelled**: the host window is titled "Meridian (nested dev
  mode, not a full session)", the bar shows a "Nested dev mode" chip, and the
  log prints a warning saying what is missing. Started by hand on a TTY
  without the script, the compositor warns that nothing will be restarted.
- **`terra-doctor`** (`/usr/bin`): prints PASS / FAIL / SKIP with a fix hint
  for session units, environment, notifications, portals, audio, polkit,
  keyring, tray, clipboard, XWayland and Qt. Exit status is the number of FAILs.

There is no settings daemon and no wallpaper process to make into units: the
wallpaper is drawn by the compositor and settings are a config file.

### Verification

| Check | Result |
|---|---|
| Unit files are valid | PASS: `systemd-analyze --user verify` on the target and services reports nothing |
| Session script syntax | PASS: `sh -n` |
| A shell program is restarted after a crash | PASS: the bar ran as a transient unit with the same settings (`Restart=on-failure`, `RestartSec=1`) against a nested compositor; after `kill -9` it was active again within 3 s with a new PID, `NRestarts=1`, and the compositor kept answering IPC |
| The test left nothing in the host's systemd environment | PASS: no `MERIDIAN_SOCKET` or `wayland-1` in `systemctl --user show-environment` afterwards |
| Nested mode is labelled | PASS: IPC state reports `"nested":true`, the bar shows the chip (screenshot), the log prints the warning |
| `terra-doctor` runs | PASS: run in the GNOME host session, 10 expected FAILs, exit status 10 |
| Login through GDM | **UNVERIFIED**. Choose "Terra (Meridian)" in GDM, then run `terra-doctor` in a terminal and `cat ~/.local/state/meridian/session.log` |
| Start from a TTY | **UNVERIFIED**. Ctrl+Alt+F3, log in, run `meridian-session`, then `terra-doctor` |
| Environment reaches systemd and D-Bus | **UNVERIFIED**. In the session: `systemctl --user show-environment \| grep -E "WAYLAND_DISPLAY\|MERIDIAN_SOCKET"` |
| Shell crash in the real session | **UNVERIFIED**. In the session: `pkill -9 meridian-bar; sleep 2; systemctl --user is-active meridian-bar.service` |
| Logout stops everything | **UNVERIFIED**. Quit with Super+Shift+E, then on a TTY: `systemctl --user is-active meridian-session.target` should say `inactive` |

### Logout path

Super+Shift+E (or "Log out" in Find) stops the compositor's event loop; the
session script then stops `meridian-session.target` and with it
`graphical-session.target` and every unit that is `PartOf` it.
**UNVERIFIED** on a real session.

## Step 2: render loop

Status: done and measured in nested mode. The real-session loop is rewritten
on the same design but **UNVERIFIED** (no hardware run).

### What changed

- **Draw on demand.** Nothing draws on a schedule. `queue_redraw(reason)` is
  called when a client commits, on input, when the cursor image changes, and
  on focus, layout, mood, wallpaper, lock, layer and output changes. Requests
  that arrive while a frame is in flight are merged.
- **Real session state machine:** `Idle -> Queued -> WaitingForVblank`, back
  to `Idle` or `Queued`. When a redraw finds nothing changed, no frame is
  submitted and the state is `WaitingForTimer` for one refresh interval (the
  "estimated VBlank"). Frame callbacks are sent when a cycle ends, so a client
  gets at most one per refresh. The 16 ms polling timer is gone.
- **Nested:** a redraw is requested from the host only when queued. The damage
  tracker is asked whether anything changed; if not, nothing is drawn or
  submitted and frame callbacks follow 16 ms later. If so, the window is
  repainted in full. (Partial repaints by EGL buffer age were tried and
  dropped: the age can only be read while the window is the current GL
  target, which off-screen passes and buffer imports change between frames.
  The failed reads logged an EGL error per frame and forced full repaints
  anyway.)
- **Clients are flushed after every event-loop pass**, not only after a
  frame. Found by measuring: with on-demand drawing a starting client waited
  forever for replies, and the lock screen never came up.
- **Effect flags** in `[effects]`: `rounded-corners`, `shadows`,
  `direct-scanout`. With rounded corners off, windows are drawn straight from
  their surfaces: no off-screen texture, no shader. With shadows off, no
  shadow element exists. Direct scanout stays off by default. There is no
  blur.
- **Wallpaper:** the landscape is painted on a worker thread once per output
  size and theme, uploaded once, and reused as a plain texture. The shader
  draws it until the texture arrives.
- **Real session:** the fastest refresh rate at the preferred resolution is
  chosen; the lock watchdog moved to its own 1 s timer (a locked idle screen
  draws no frames).

### Measurements (nested, release build, same script)

| Measure | Before (`main`) | After |
|---|---|---|
| Redraws per second, idle | 119 | 0.04 |
| Redraws that found nothing changed, per second | 0 (never checked) | 0.16 |
| Frame callbacks per second, idle | 540 | 0.8 |
| Compositor CPU, idle 60 s (`/proc`) | 3.53% | 0.08% |
| Compositor CPU, locked 15 s | 2.67% | 0.00% |
| Redraws per second, locked | 119 | 0 |
| Frame time p50 / p95, idle | 0.23 / 0.42 ms | 0.04 / 0.04 ms |
| Frame time p95 / max while the pointer moves over the window | not separated | 2.06 / 18.5 ms |
| EGL errors in the log | 0 | 0 (156 with the buffer-age attempt) |
| Tests | 35 pass | 38 pass |

The idle redraws that remain are one client commit every few seconds (the
bar's clock and tray).

### Not done or not verified

- **Real session:** the whole state machine, the refresh-rate choice and the
  estimated-VBlank timer. Verify with
  `MERIDIAN_STATS=$HOME/tty.csv meridian-session`, 60 s idle, then drag a
  window for 10 s; idle rows should show about 0 redraws and `cpu_percent`
  near 0, drag rows a `frame_p95_ms` under the refresh interval (8.3 ms at
  120 Hz).
- **Frame hitches:** single frames of 18 to 23 ms happen while windows open
  or the pointer first moves in. Cause not yet found; candidates are the
  first title-bar paint (font loading in pango) and texture creation.
- **Which wallpaper path drew a frame** is not logged, so "texture, not
  shader" is unconfirmed beyond the picture looking right.
- **Rounded corners still use an off-screen pass per window when on.** Its
  cost is inside the 0.04 to 0.2 ms frame times above, so it was left alone.
- **Animations:** the compositor has none. The dock's slide honours reduced
  motion with a zero duration.
- **Moving a window:** still not measurable here.

## Step 3: required services

Status: written; most of it verified in nested mode or on a private D-Bus
session with the real helper programs. Nothing verified inside a real
session yet.

| Item | Before | After | How it was checked |
|---|---|---|---|
| Clipboard persistence | **MISSING**: `wl-paste` says "Nothing is copied" once the source app exits | **WORKS**: text still pastes after the source exits; primary selection works | Nested, `wl-copy` / `wl-paste`, `wl-clip-persist` from the Arch package running on the new `ext-data-control` / `wlr-data-control` protocols |
| XWayland | **MISSING**: X11 apps cannot start | **WORKS** nested: `glxgears` opened and ran at 120 FPS | `xwayland-satellite :12` from the Arch package against the nested compositor (needed the new `viewporter` protocol) |
| Portals | **BROKEN**: no config, no backend chosen | **WORKS** for file chooser and settings; screenshot and screencast **MISSING** | Private bus with `XDG_CURRENT_DESKTOP=Meridian` and our `meridian-portals.conf`: `FileChooser.OpenFile` opened the GTK dialog as a window in the compositor, `Settings.Read` answered, `Screenshot` is not offered |
| Notifications | Partial: body only | **WORKS**: capabilities `body`, `actions`, `icon-static`; expiry, replace-by-id, critical urgency, close-by-call | Private bus, `gdbus call` and `gdbus monitor`: `NotificationClosed(1, 1)` after expiry, `(2, 3)` after `CloseNotification`; screenshot shows the critical card with its buttons 4 s after a 2 s timeout |
| Tray | **MISSING** | **WORKS** for showing items | Private bus, scripted StatusNotifierItem: registered item listed by the watcher, icon in the bar (screenshot), removed when the app quit; 20 appear/vanish cycles left the bar running, no items, PSS 83,320 -> 83,352 kB |
| polkit agent | **MISSING** | Unit added, **UNVERIFIED** | `meridian-polkit-agent.service` starts the first installed agent. Verify in a session: `pkexec true` should ask for the password |
| PipeWire / WirePlumber | Present on the system | Wanted by the session target, **UNVERIFIED** | Verify in a session: `terra-doctor` (Audio section) |
| Keyring | **MISSING** in a session | Secret portal routed to gnome-keyring, **UNVERIFIED** | gnome-keyring is D-Bus activated and unlocked by the login manager's PAM setup. Verify: `terra-doctor` (Authentication section) |
| `terra-doctor` | n/a | **WORKS** | Run in the host session; exit status equals the number of FAILs |

### What changed

- **Compositor protocols:** `ext-data-control`, `wlr-data-control`,
  `primary-selection`, `viewporter`.
- **Session units** (each skipped when its program is not installed):
  `meridian-clipboard.service` (`wl-clip-persist`),
  `meridian-xwayland.service` (`xwayland-satellite`),
  `meridian-polkit-agent.service` (first installed agent). The target also
  wants `pipewire.socket`, `pipewire-pulse.socket`, `wireplumber.service`.
- **`DISPLAY`:** the session script picks a free X display number and exports
  it; the compositor passes it to systemd and D-Bus and only strips `DISPLAY`
  from child processes when nested (where it is the host's).
- **`meridian-portals.conf`:** GTK backend by default, gnome-keyring for
  secrets, screenshot / screencast / remote desktop switched off.
- **Notifications:** actions as buttons, `ActionInvoked`, the default action
  on a click, urgency (critical never expires).
- **Tray:** the bar owns `org.kde.StatusNotifierWatcher`, shows each item's
  icon (theme name or pixels) and title, forwards a left click as `Activate`,
  and draws the item's dbusmenu in its own small window on a right click. All
  D-Bus calls are asynchronous.
- **Package dependencies:** `wl-clip-persist`, `xdg-desktop-portal` and
  `xdg-desktop-portal-gtk` are now required; `xwayland-satellite`,
  `polkit-gnome`, `gnome-keyring`, `pipewire-pulse` and the Qt Wayland
  plugins are optional with a reason each.

### Not done or not verified

- **Screenshot and screencast portals:** no backend can capture this
  compositor. The compositor implements no screen-copy protocol, which
  `xdg-desktop-portal-wlr` would need. The Print key still saves a screenshot.
- **`ActionInvoked` and tray clicks/menus:** need a pointer click. The menu
  parser is unit-tested; the menu window has never been seen.
- **XWayland scale:** only checked at scale 1 nested. HiDPI is unverified.
- **Everything as real session units:** verify with `terra-doctor`.

## Step 4: crash resilience

Status: done for what can be exercised nested; all checks below were run.

### What changed

- **No `unwrap` or `expect` left in the compositor's non-test code**
  (10 before, 0 after; counted with `grep` over `meridian-comp/src`). Missing
  seat devices return early, poisoned locks fall back to a default, and a
  client without our per-client data gets a shared spare instead of a panic.
  Indexing was reviewed: the three remaining `[...]` sites index fixed-size
  arrays or values bounded on the line above.
- **Panic hook:** a panic writes
  `~/.local/state/meridian/panic-<unix time>.log` with the message, location
  and a backtrace, and the same text goes to the session log.
- **IPC state has `locked`**, so tests and tools can see the lock state.
- **`scripts/abuse-test.sh`:** starts a nested compositor and mistreats it.

### Verification

| Check | Result |
|---|---|
| Deliberate panic (debug build, `MERIDIAN_TEST_PANIC=1`) | PASS: `panic-*.log` written with message, `main.rs:148` and backtrace; exit status 101 |
| 30 clients sending 4 kB of random bytes on the Wayland socket | PASS: compositor alive |
| Random bytes, invalid JSON, an unknown action and a 2 MB line on the IPC socket | PASS: compositor alive, a normal client is still answered |
| 5 windows whose client is killed with SIGKILL | PASS |
| Bar, dock, desktop icons and Find killed with SIGKILL (as plain children) | PASS: compositor and the open app survive each |
| Dock, desktop icons and Find as transient systemd units with the shipped settings, killed with SIGKILL | PASS: each `active` again within 3.5 s, `NRestarts=1` (the bar was checked in step 1) |
| Lock program killed while locked | PASS: `locked` stays true immediately after, the program is restarted, still locked |
| Focus request over IPC while locked | PASS: no window gets focus |
| Crash reports after the whole abuse run | PASS: none written |

`scripts/abuse-test.sh` exits 0 with 14 PASS lines.

### Not done or not verified

- **Panics inside Smithay or drivers** cannot be audited from here; the hook
  will at least record them.
- **The notification daemon as a unit** was not restart-tested: in the host
  session another daemon owns the bus name, so ours exits at once.
- **Malformed but well-typed protocol use** (for example absurd sizes or
  thousands of surfaces) is not covered by the random-bytes test. That
  belongs with the client-server tests in step 7.
- **Restart in a real session** with the installed units: see step 1.

## Still broken (running list)

Ranked by expected user impact; re-ranked as measurements come in.

1. The real session has never run on hardware. Its render loop, the systemd
   session, every session unit and the logout path are unverified.
2. No screenshot or screencast portal (step 3).
3. Occasional 18 to 23 ms frames when windows open (step 2).
4. Lock screen unlock path never tested with a real password.
5. Popups do not close on an outside click; several protocols missing (0.7).
6. Tray menus, notification action clicks and the polkit agent are untested.
7. No idle handling, sleep inhibitors or lock-before-suspend yet (step 5).
