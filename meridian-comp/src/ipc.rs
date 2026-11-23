//! The compositor side of Meridian's IPC socket (protocol: meridian_common::ipc).
//!
//! Shell programs connect here to learn about windows and to send requests.

use std::os::unix::net::UnixListener;

use anyhow::Context;
use meridian_common::ipc::{self, Connection, Request, State, WindowInfo};
use smithay::{
    desktop::Window,
    reexports::calloop::{Interest, Mode, PostAction, generic::Generic},
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};

use crate::state::Meridian;

#[derive(Default)]
pub struct IpcServer {
    /// Connected clients, each with an id so its event source can find it.
    clients: Vec<(u32, Connection)>,
    next_client_id: u32,
    /// The last snapshot we sent, to avoid repeating identical ones.
    last_state: Option<State>,
}

/// Stored in each window's user data.
struct WindowId(u32);

impl Meridian {
    /// Create the socket and start accepting clients. Returns its path.
    pub fn init_ipc(&mut self) -> anyhow::Result<std::path::PathBuf> {
        let path = ipc::socket_path(&self.socket_name.to_string_lossy())
            .context("XDG_RUNTIME_DIR is not set")?;
        // A stale socket file from a crashed run would block `bind`.
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).with_context(|| format!("cannot create {}", path.display()))?;
        listener.set_nonblocking(true)?;

        // `Generic` turns any file descriptor into an event source: the
        // callback runs whenever the listener has a pending connection.
        self.loop_handle
            .insert_source(Generic::new(listener, Interest::READ, Mode::Level), |_, listener, state| {
                while let Ok((stream, _)) = listener.accept() {
                    if let Ok(connection) = Connection::from_stream(stream) {
                        state.ipc_add_client(connection);
                    }
                }
                Ok(PostAction::Continue)
            })
            .map_err(|err| anyhow::anyhow!("cannot watch the IPC socket: {err}"))?;

        Ok(path)
    }

    fn ipc_add_client(&mut self, mut connection: Connection) {
        let id = self.ipc.next_client_id;
        self.ipc.next_client_id += 1;

        // Greet the client with the current state.
        if connection.send(&self.ipc_state()).is_err() {
            return;
        }

        // Watch a duplicate of the socket for incoming requests. The
        // `Connection` itself stays in `self.ipc.clients` so we can write to it.
        let Ok(fd) = connection.as_fd_owned() else { return };
        let watching = self
            .loop_handle
            .insert_source(Generic::new(fd, Interest::READ, Mode::Level), move |_, _, state| {
                Ok(state.ipc_client_readable(id))
            });
        if watching.is_ok() {
            self.ipc.clients.push((id, connection));
        }
    }

    /// Read and run a client's requests. Returns `Remove` once it disconnects.
    fn ipc_client_readable(&mut self, id: u32) -> PostAction {
        let Some(index) = self.ipc.clients.iter().position(|(client_id, _)| *client_id == id) else {
            return PostAction::Remove;
        };

        match self.ipc.clients[index].1.receive::<Request>() {
            Ok(requests) => {
                for request in requests {
                    self.ipc_handle(request);
                }
                PostAction::Continue
            }
            Err(_) => {
                self.ipc.clients.remove(index);
                PostAction::Remove
            }
        }
    }

    fn ipc_handle(&mut self, request: Request) {
        match request {
            Request::FocusWindow { id } => {
                let window = self.windows.iter().find(|w| window_id(w) == Some(id)).cloned();
                if let Some(window) = window {
                    self.focus_window(Some(&window));
                }
            }
            Request::Action(action) => self.run_action(action),
        }
    }

    /// Give a new window its id.
    pub fn ipc_register_window(&mut self, window: &Window) {
        let id = self.next_window_id;
        self.next_window_id += 1;
        window.user_data().insert_if_missing(|| WindowId(id));
    }

    fn ipc_state(&self) -> State {
        let focused = self.focused_window();
        let windows = self
            .windows
            .iter()
            .filter_map(|window| {
                let toplevel = window.toplevel()?;
                // Title and app id are stored by Smithay on the surface.
                let (app_id, title) = with_states(toplevel.wl_surface(), |states| {
                    let data = states.data_map.get::<XdgToplevelSurfaceData>()?.lock().ok()?;
                    Some((data.app_id.clone().unwrap_or_default(), data.title.clone().unwrap_or_default()))
                })?;
                Some(WindowInfo {
                    id: window_id(window)?,
                    app_id,
                    title,
                    focused: focused.as_ref() == Some(window),
                })
            })
            .collect();

        State {
            windows,
            layout: self.layout.kind(),
        }
    }

    /// Tell all clients about the current state, if it changed.
    /// Call this after anything that affects windows, focus or layout.
    pub fn ipc_broadcast(&mut self) {
        let state = self.ipc_state();
        if self.ipc.last_state.as_ref() == Some(&state) {
            return;
        }
        // A client that cannot be written to is dropped; its event source
        // notices on the next read and removes itself.
        self.ipc.clients.retain_mut(|(_, connection)| connection.send(&state).is_ok());
        self.ipc.last_state = Some(state);
    }
}

fn window_id(window: &Window) -> Option<u32> {
    window.user_data().get::<WindowId>().map(|id| id.0)
}
