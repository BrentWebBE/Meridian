//! How Meridian's programs talk to the compositor.
//!
//! The compositor listens on a Unix socket. Messages are JSON, one per line:
//!
//! * compositor -> client: a [`State`] snapshot, sent on connect and again
//!   whenever something in it changes;
//! * client -> compositor: a [`Request`].
//!
//! (Wayland itself has no standard way for a dock to list windows or focus
//! one, so compositors add their own channel; sway and niri do the same.)

use std::{
    io::{self, Read, Write},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd},
        unix::net::UnixStream,
    },
    path::PathBuf,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::config::{Action, LayoutKind};

/// Environment variable holding the socket path, set by the compositor for
/// every program it starts.
pub const SOCKET_ENV: &str = "MERIDIAN_SOCKET";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Stable for the lifetime of the window.
    pub id: u32,
    /// Index of the space (virtual desktop) the window is on, from 0.
    pub space: u32,
    /// The application's identifier, normally its desktop file id.
    pub app_id: String,
    pub title: String,
    pub focused: bool,
}

/// Everything shell programs need to know about the desktop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// In the order the windows were opened.
    pub windows: Vec<WindowInfo>,
    pub layout: LayoutKind,
    /// Index of the space being shown, from 0.
    pub active_space: u32,
    pub space_count: u32,
    /// True while the overview is open.
    pub overview: bool,
    /// Name of the theme in use: "morning", "dusk" or "night".
    pub theme: String,
    /// Goes up every time the compositor re-reads the config file, so
    /// shell programs know to re-read it too.
    pub config_generation: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Request {
    FocusWindow { id: u32 },
    /// Run any keybinding action, e.g. `"toggle-layout"`.
    Action(Action),
    /// The config file changed: read it again and apply it.
    ReloadConfig,
    /// A shell program is done with the keyboard: give it back to the top window.
    ReturnFocus,
}

/// Where the compositor for Wayland display `wayland_display` listens.
pub fn socket_path(wayland_display: &str) -> Option<PathBuf> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(runtime_dir).join(format!("meridian-{wayland_display}.sock")))
}

/// One end of the socket, on either side. It never blocks: reads return
/// whatever complete lines have arrived so far.
pub struct Connection {
    stream: UnixStream,
    /// Bytes received that do not form a complete line yet.
    pending: Vec<u8>,
}

impl Connection {
    /// Connect to the compositor that started this program.
    pub fn connect() -> io::Result<Self> {
        let path = std::env::var_os(SOCKET_ENV)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{SOCKET_ENV} is not set")))?;
        Self::from_stream(UnixStream::connect(path)?)
    }

    pub fn from_stream(stream: UnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            pending: Vec::new(),
        })
    }

    /// Send one message.
    pub fn send<T: Serialize>(&mut self, message: &T) -> io::Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        self.stream.write_all(&line)
    }

    /// All complete messages received since the last call.
    /// An `Err` means the other side is gone (or sent garbage): drop the connection.
    pub fn receive<T: DeserializeOwned>(&mut self) -> io::Result<Vec<T>> {
        let mut chunk = [0u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => self.pending.extend_from_slice(&chunk[..n]),
                // Nothing more to read right now.
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
            // A peer must not make us buffer without limit.
            if self.pending.len() > 1 << 20 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "message too long"));
            }
        }

        let mut messages = Vec::new();
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=newline).collect();
            messages.push(serde_json::from_slice(&line[..newline])?);
        }
        Ok(messages)
    }

    pub fn raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// A second handle to the same socket, for an event loop to watch.
    pub fn as_fd_owned(&self) -> io::Result<OwnedFd> {
        Ok(self.stream.try_clone()?.into())
    }
}

// Lets event loops (calloop, GLib) watch the socket for incoming data.
impl AsFd for Connection {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_survive_the_socket_in_pieces() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut sender = Connection::from_stream(a).unwrap();
        let mut receiver = Connection::from_stream(b).unwrap();

        assert!(receiver.receive::<Request>().unwrap().is_empty());

        sender.send(&Request::FocusWindow { id: 7 }).unwrap();
        sender.send(&Request::Action(Action::ToggleLayout)).unwrap();
        // Half a message: must be kept until the rest arrives.
        sender.stream.write_all(b"{\"focus-window\":").unwrap();

        let got: Vec<Request> = receiver.receive().unwrap();
        assert_eq!(got, vec![Request::FocusWindow { id: 7 }, Request::Action(Action::ToggleLayout)]);

        sender.stream.write_all(b"{\"id\":9}}\n").unwrap();
        let got: Vec<Request> = receiver.receive().unwrap();
        assert_eq!(got, vec![Request::FocusWindow { id: 9 }]);
    }

    #[test]
    fn closed_peer_is_an_error() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut receiver = Connection::from_stream(b).unwrap();
        drop(a);
        assert!(receiver.receive::<Request>().is_err());
    }
}
