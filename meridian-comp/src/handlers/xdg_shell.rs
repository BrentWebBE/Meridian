//! xdg_shell: application windows ("toplevels") and popups (menus, tooltips).

use smithay::{
    desktop::{PopupKind, Window, find_popup_root_surface, get_popup_toplevel_coords},
    input::{Seat, pointer::GrabStartData},
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            Resource,
            protocol::{wl_seat::WlSeat, wl_surface::WlSurface},
        },
    },
    utils::Serial,
    wayland::shell::xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState},
};

use crate::state::Meridian;

impl XdgShellHandler for Meridian {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    /// A client created a window.
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface);

        // Cascade new windows so they do not stack exactly on top of each other.
        // (A managed layout overrides this position right away.)
        let step = (self.windows.len() % 8) as i32;
        let origin = self.usable_area().map(|area| area.loc).unwrap_or_default();
        let location = (origin.x + 48 + step * 32, origin.y + 48 + step * 32);

        self.space.map_element(window.clone(), location, false);
        self.windows.push(window.clone());
        self.apply_layout();
        self.focus_window(Some(&window));
    }

    /// A window was closed by its client.
    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // `retain` keeps only the elements for which the closure returns true.
        self.windows.retain(|w| w.toplevel().is_some_and(|t| *t != surface));
        self.apply_layout();

        // Hand focus to the topmost remaining window. (`elements()` is ordered
        // bottom to top; the closed window is still listed until the next refresh.)
        let next = self
            .space
            .elements()
            .rev()
            .find(|w| w.toplevel().is_some_and(|t| *t != surface))
            .cloned();
        self.focus_window(next.as_ref());
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        if let Err(err) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!("failed to track popup: {err}");
        }
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    /// The client asks us to start moving its window: this is what happens when
    /// you drag a title bar the client drew itself (client-side decorations).
    /// The client asks us to start moving its window: this is what happens when
    /// you drag a title bar the client drew itself (client-side decorations).
    fn move_request(&mut self, surface: ToplevelSurface, seat: WlSeat, serial: Serial) {
        let Some(seat) = Seat::<Meridian>::from_resource(&seat) else { return };
        let wl_surface = surface.wl_surface();

        // Security check: only honor the request if it comes from the client
        // that was actually clicked, with the serial of that click.
        let Some(start_data) = click_start_data(&seat, wl_surface, serial) else { return };
        let Some(window) = self.window_for_surface(wl_surface) else { return };
        self.start_move(window, start_data, serial);
    }

    /// Same, for dragging a window border drawn by the client.
    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let Some(seat) = Seat::<Meridian>::from_resource(&seat) else { return };
        let wl_surface = surface.wl_surface();

        let Some(start_data) = click_start_data(&seat, wl_surface, serial) else { return };
        let Some(window) = self.window_for_surface(wl_surface) else { return };
        self.start_resize(window, edges.into(), start_data, serial);
    }

    // Popup grabs (menus that close when you click elsewhere) come later.
    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {}
}

/// Returns the data of the click identified by `serial`, if that click landed
/// on a surface of the same client as `surface`.
fn click_start_data(seat: &Seat<Meridian>, surface: &WlSurface, serial: Serial) -> Option<GrabStartData<Meridian>> {
    let pointer = seat.get_pointer()?;
    if !pointer.has_grab(serial) {
        return None;
    }
    let start_data = pointer.grab_start_data()?;
    let (focus, _) = start_data.focus.as_ref()?;
    focus.id().same_client_as(&surface.id()).then_some(start_data)
}

/// Called on every surface commit (see handlers/compositor.rs).
pub fn handle_commit(state: &mut Meridian, surface: &WlSurface) {
    // xdg_shell handshake: after a client's first (empty) commit we must send
    // a "configure" event; only then may it attach a buffer and become visible.
    if let Some(toplevel) = state.window_for_surface(surface).as_ref().and_then(|w| w.toplevel())
        && !toplevel.is_initial_configure_sent()
    {
        toplevel.send_configure();
    }

    state.popups.commit(surface);
    if let Some(PopupKind::Xdg(popup)) = state.popups.find_popup(surface)
        && !popup.is_initial_configure_sent()
    {
        // The initial configure is always allowed, so this cannot fail.
        let _ = popup.send_configure();
    }
}

impl Meridian {
    /// Keep a popup on screen, e.g. flip a menu that would run off the edge.
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let kind = PopupKind::Xdg(popup.clone());
        let Ok(root) = find_popup_root_surface(&kind) else { return };
        let Some(window) = self.window_for_surface(&root) else { return };
        let Some(output) = self.space.outputs().next() else { return };
        let Some(output_geo) = self.space.output_geometry(output) else { return };
        let Some(window_geo) = self.space.element_geometry(&window) else { return };

        // The allowed area, expressed relative to the popup's parent.
        let mut target = output_geo;
        target.loc -= get_popup_toplevel_coords(&kind);
        target.loc -= window_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}
