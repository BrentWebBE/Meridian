//! Protocol handlers. Smithay implements the Wayland protocols; for each one it
//! asks us to implement a small trait so it can reach our state and tell us
//! what clients requested.

mod compositor;
mod xdg_shell;

use smithay::{
    backend::{allocator::dmabuf::Dmabuf, renderer::ImportDma},
    input::{
        Seat, SeatHandler, SeatState,
        dnd::{DnDGrab, DndGrabHandler, GrabType, Source},
        pointer::{CursorImageStatus, Focus},
    },
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface},
    utils::Serial,
    wayland::{
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        output::OutputHandler,
        pointer_constraints::PointerConstraintsHandler,
        selection::{
            SelectionHandler,
            data_device::{DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler, set_data_device_focus},
        },
    },
};

use crate::state::Meridian;

impl SeatHandler for Meridian {
    // What can hold focus. For now only plain surfaces; later also our own UI.
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Meridian> {
        &mut self.seat_state
    }

    // Clients ask for a cursor shape here. The real-session backend draws it;
    // nested, the host desktop draws the cursor and this is unused.
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        // Only the focused client may read the clipboard.
        let dh = &self.display_handle;
        let client = focused.and_then(|surface| dh.get_client(surface.id()).ok());
        set_data_device_focus(dh, seat, client);
    }
}

// Required by Smithay for surfaces to receive pointer focus. Pointer locking
// (used by games) is not advertised yet, so the defaults are enough.
impl PointerConstraintsHandler for Meridian {}

// Clipboard and drag-and-drop.
impl SelectionHandler for Meridian {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Meridian {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl DndGrabHandler for Meridian {}

impl WaylandDndGrabHandler for Meridian {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        let pointer = seat.get_pointer();
        let start_data = pointer.as_ref().and_then(|p| p.grab_start_data());

        match (type_, pointer, start_data) {
            (GrabType::Pointer, Some(pointer), Some(start_data)) => {
                let grab = DnDGrab::new_pointer(&self.display_handle, start_data, source, seat);
                pointer.set_grab(self, grab, serial, Focus::Keep);
            }
            _ => source.cancel(), // no touch support yet
        }
    }
}

impl OutputHandler for Meridian {}

// GPU buffers: a client hands us a dmabuf, we check that our renderer can use it.
impl DmabufHandler for Meridian {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        if self.backend.renderer().import_dmabuf(&dmabuf, None).is_ok() {
            let _ = notifier.successful::<Meridian>();
        } else {
            notifier.failed();
        }
    }
}

// Generates the glue that routes incoming Wayland requests for every protocol
// above to Smithay's implementation, with `Meridian` as the state type.
smithay::delegate_dispatch2!(Meridian);
