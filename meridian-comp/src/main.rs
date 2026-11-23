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
mod grabs;
mod handlers;
mod input;
mod keys;
mod layout;
mod state;

use anyhow::Context;
use meridian_common::config::Config;
use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

use crate::{backend::Backend, state::Meridian};

const HELP: &str = "\
Usage: meridian-comp [--winit | --tty] [COMMAND [ARGS...]]

Runs the Meridian compositor. Inside another desktop it opens as a window
(nested); from a text console or login manager it takes over the screen.

  --winit     force nested mode
  --tty       force a real session
  COMMAND     program to start as the first client, e.g. `meridian-comp foot`

Default keys (Mod = Alt when nested, Super in a real session):
  Mod+Return            open a terminal
  Mod+Q                 close the focused window
  Mod+T                 switch between floating and tiled layout
  Mod+Tab               focus the next window (add Shift for previous)
  Mod+left-drag         move a window
  Mod+right-drag        resize a window
  Mod+Shift+E           quit Meridian
  Ctrl+Alt+F1..F12      switch virtual terminal (real session only)

Configuration: ~/.config/meridian/config.toml
Example file:  /usr/share/doc/meridian-git/config.toml";

// `anyhow::Result` is a Result whose error type can hold any error. The `?`
// operator returns early with the error, and `.context()` adds a message to it.
fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // Nested if we were started from inside a graphical session.
    let mut nested = std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            println!("{HELP}");
            return Ok(());
        }
        Some("-V" | "--version") => {
            println!("meridian-comp {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--winit") => {
            nested = true;
            args.remove(0);
        }
        Some("--tty") => {
            nested = false;
            args.remove(0);
        }
        _ => {}
    }

    init_logging();

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

    if let Some((program, rest)) = args.split_first() {
        state.spawn(program, rest);
    }

    // Blocks until `loop_signal.stop()` is called.
    event_loop
        .run(None, &mut state, |_| {})
        .context("event loop failed")?;

    Ok(())
}

/// Log to stderr. Set `RUST_LOG=debug` (or `RUST_LOG=smithay=trace`) for more.
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,meridian_comp=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
