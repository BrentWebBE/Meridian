//! Meridian compositor.
//!
//! How the pieces fit together:
//!
//! * `calloop` is the event loop. Everything that can wake us up (the Wayland
//!   socket, client messages, input devices, monitors) is registered as an
//!   "event source" with a callback.
//! * Every callback receives `&mut Meridian`, our single state struct. Rust
//!   allows only one mutable reference to a value at a time, so instead of each
//!   callback owning a piece of state, the loop owns nothing and lends the whole
//!   state to one callback at a time. That is why `state` is passed to
//!   `event_loop.run` rather than captured by the closures.

mod backend;
mod cursor;
mod decorations;
mod effects;
mod grabs;
mod handlers;
mod input;
mod ipc;
mod keys;
mod layout;
mod lock;
mod overview;
mod state;
mod stats;

use anyhow::Context;
use meridian_common::config::Config;
use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

use crate::{backend::Backend, state::Meridian};

const HELP: &str = "\
Usage: meridian-comp [--winit | --tty] [COMMAND [ARGS...]]

Runs the Meridian compositor with the Terra desktop. Inside another desktop
it opens as a window (nested); from a text console or login manager it takes
over the screen.

  --winit     force nested dev mode (NOT a full session)
  --tty       force a real session on this console
  --session   real session under systemd; use the `meridian-session` script
  COMMAND     program to start as the first client, e.g. `meridian-comp foot`

Default keys (Mod = Alt when nested, Super in a real session):
  Super tap, Mod+Space  Find: apps, files, settings, actions
  Mod+Return            open a terminal
  Mod+Q                 close the focused window
  Mod+Tab               focus the next window (add Shift for previous)
  Mod+1..4              go to a space (add Shift to move the window there)
  Mod+O                 overview
  Mod+T                 switch between floating and tiled layout
  Mod+D                 keyboard to the dock
  Mod+L                 lock the screen
  Mod+left-drag         move a window
  Mod+right-drag        resize a window
  Print                 screenshot to ~/Pictures/Screenshots
  Mod+Shift+E           quit Meridian
  Ctrl+Alt+F1..F12      switch virtual terminal (real session only)

Configuration: ~/.config/meridian/config.toml (or the Settings app)
Example file:  /usr/share/doc/meridian-git/config.toml";

// `anyhow::Result` is a Result whose error type can hold any error. The `?`
// operator returns early with the error, and `.context()` adds a message to it.
fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // Nested if we were started from inside a graphical session.
    let mut nested = std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
    // Set by the `meridian-session` script: we are a login session and
    // systemd runs the shell programs.
    let mut managed = false;
    // Leading flags; everything after them is the command to start.
    while let Some(flag) = args.first().map(String::as_str) {
        match flag {
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("meridian-comp {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--winit" => nested = true,
            "--tty" => nested = false,
            "--session" => {
                managed = true;
                nested = false;
            }
            _ => break,
        }
        args.remove(0);
    }

    init_logging();
    install_panic_hook();

    // A broken config must not lock you out of your session: report and continue.
    let config = Config::load().unwrap_or_else(|err| {
        tracing::error!("{err}\nusing the default configuration instead");
        Config::default()
    });

    // The type parameter says which state type the callbacks will receive.
    let mut event_loop: EventLoop<'static, Meridian> =
        EventLoop::try_new().context("failed to create the event loop")?;

    // The Wayland display: owns the protocol state of all connected clients.
    let display: Display<Meridian> = Display::new().context("failed to create the Wayland display")?;

    let mut state = if nested {
        let (winit, winit_events) = backend::winit::init()?;
        let mut state = Meridian::new(&mut event_loop, display, Backend::Winit(Box::new(winit)), config)?;
        backend::winit::attach(&mut event_loop, &mut state, winit_events)?;
        state
    } else {
        let tty = backend::tty::init(&event_loop)?;
        let mut state = Meridian::new(&mut event_loop, display, Backend::Tty(Box::new(tty)), config)?;
        state.tty_scan_connectors();
        state
    };

    tracing::info!(
        "Meridian is running. Start clients with WAYLAND_DISPLAY={}",
        state.socket_name.to_string_lossy()
    );

    match state.init_ipc() {
        Ok(path) => state.ipc_socket = Some(path),
        Err(err) => tracing::error!("IPC is unavailable, bar and dock will be limited: {err:#}"),
    }
    state.start_theme_clock();
    state.start_stats();
    state.init_wallpaper();
    state.watch_config_files();

    // Test aid, debug builds only: panic on purpose shortly after startup,
    // to check that the crash report is written.
    #[cfg(debug_assertions)]
    if std::env::var_os("MERIDIAN_TEST_PANIC").is_some() {
        use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
        let _ = event_loop
            .handle()
            .insert_source(Timer::from_duration(std::time::Duration::from_secs(2)), |_, _, _| {
                panic!("MERIDIAN_TEST_PANIC is set: this panic is deliberate");
                #[allow(unreachable_code)]
                TimeoutAction::Drop
            });
    }

    if nested {
        // Say it loudly: this is a development aid, not the desktop.
        tracing::warn!(
            "NESTED DEV MODE: running as a window inside another desktop. This is not a full \
             session: no systemd units, no portals, no polkit agent, and speed is limited by \
             the host. Log in through `meridian-session` to judge the real thing."
        );
        state.autostart();
    } else if managed {
        state.start_systemd_session();
    } else {
        tracing::warn!(
            "started without the `meridian-session` script: shell programs run as plain child \
             processes and are not restarted if they crash"
        );
        state.autostart();
    }

    if let Some((program, rest)) = args.split_first() {
        state.spawn(program, rest);
    }

    // Blocks until `loop_signal.stop()` is called.
    // The closure runs after every pass of the loop. Sending queued
    // messages to clients here, and not only after a frame, matters now
    // that frames are drawn on demand: a client waiting for an answer
    // (for example while starting up) must get it even if nothing is drawn.
    event_loop
        .run(None, &mut state, |state| {
            let _ = state.display_handle.flush_clients();
        })
        .context("event loop failed")?;

    Ok(())
}

/// Where crash reports go: `~/.local/state/meridian`.
fn state_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/state")))?;
    Some(base.join("meridian"))
}

/// If the compositor ever panics (a bug: it should not), write what
/// happened and where to a file before the process dies, so there is
/// something to send with a bug report. The session script's log gets the
/// same text through stderr.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // `force_capture` records the call stack even without RUST_BACKTRACE.
        let backtrace = std::backtrace::Backtrace::force_capture();
        let report = format!(
            "meridian-comp {} panicked\n{info}\n\nbacktrace:\n{backtrace}\n",
            env!("CARGO_PKG_VERSION")
        );
        if let Some(dir) = state_dir()
            && std::fs::create_dir_all(&dir).is_ok()
        {
            let seconds = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs());
            let path = dir.join(format!("panic-{seconds}.log"));
            if std::fs::write(&path, &report).is_ok() {
                eprintln!("meridian-comp: crash report written to {}", path.display());
            }
        }
        default_hook(info);
    }));
}

/// Log to stderr. Set `RUST_LOG=debug` (or `RUST_LOG=smithay=trace`) for more.
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,meridian_comp=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
