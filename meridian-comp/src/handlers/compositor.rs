//! wl_compositor, wl_shm: surfaces and their pixel buffers.

use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    reexports::wayland_server::{
        Client,
        protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface},
    },
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface},
        shm::{ShmHandler, ShmState},
    },
};

use super::xdg_shell;
use crate::state::{ClientState, Meridian};

impl CompositorHandler for Meridian {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // Every client was inserted with a `ClientState` (see state.rs).
        &client
            .get_data::<ClientState>()
            .expect("client was created with ClientState")
            .compositor_state
    }

    /// A client "commits" a surface to apply everything it changed since the
    /// last commit (new buffer, size, ...) in one atomic step.
    fn commit(&mut self, surface: &WlSurface) {
        // Takes the newly attached buffer so the renderer can draw it.
        on_commit_buffer_handler::<Self>(surface);

        if !is_sync_subsurface(surface) {
            // Walk up to the window's main surface.
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.window_for_surface(&root) {
                window.on_commit();
                self.resize_commit(&root);
            }
        }

        xdg_shell::handle_commit(self, surface);
        self.layer_commit(surface);
    }
}

impl BufferHandler for Meridian {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for Meridian {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
