//! Pointer grabs. While a grab is active the compositor, not a client, receives
//! all pointer events. `MoveGrab` drags a window around, `ResizeGrab` resizes it.

use smithay::{
    desktop::Window,
    input::pointer::{
        AxisFrame, ButtonEvent, Focus, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
        GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
        GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
        RelativeMotionEvent,
    },
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point, Serial, Size},
    wayland::{compositor::with_states, shell::xdg::SurfaceCachedState},
};

use crate::{layout::Rect, state::Meridian};

// Smithay's `PointerGrab` trait has a method for every kind of pointer event.
// Our grabs only care about motion and buttons; everything else is passed
// through unchanged. This macro writes those pass-through methods so both
// grabs can share them. (`macro_rules!` is compile-time copy and paste.)
macro_rules! forward_other_pointer_events {
    () => {

        fn relative_motion(
            &mut self,
            data: &mut Meridian,
            handle: &mut PointerInnerHandle<'_, Meridian>,
            focus: Option<(WlSurface, Point<f64, Logical>)>,
            event: &RelativeMotionEvent,
        ) {
            handle.relative_motion(data, focus, event);
        }

        fn axis(&mut self, data: &mut Meridian, handle: &mut PointerInnerHandle<'_, Meridian>, details: AxisFrame) {
            handle.axis(data, details)
        }

        fn frame(&mut self, data: &mut Meridian, handle: &mut PointerInnerHandle<'_, Meridian>) {
            handle.frame(data);
        }

        fn gesture_swipe_begin(
            &mut self,
            data: &mut Meridian,
            handle: &mut PointerInnerHandle<'_, Meridian>,
            event: &GestureSwipeBeginEvent,
        ) {
            handle.gesture_swipe_begin(data, event)
        }

        fn gesture_swipe_update(
            &mut self,
            data: &mut Meridian,
            handle: &mut PointerInnerHandle<'_, Meridian>,
            event: &GestureSwipeUpdateEvent,
        ) {
            handle.gesture_swipe_update(data, event)
        }

        fn gesture_swipe_end(
            &mut self,
            data: &mut Meridian,
            handle: &mut PointerInnerHandle<'_, Meridian>,
            event: &GestureSwipeEndEvent,
        ) {
            handle.gesture_swipe_end(data, event)
        }

        fn gesture_pinch_begin(
            &mut self,
            data: &mut Meridian,
            handle: &mut PointerInnerHandle<'_, Meridian>,
            event: &GesturePinchBeginEvent,
        ) {
            handle.gesture_pinch_begin(data, event)
        }

        fn gesture_pinch_update(
            &mut self,
            data: &mut Meridian,
            handle: &mut PointerInnerHandle<'_, Meridian>,
            event: &GesturePinchUpdateEvent,
        ) {
            handle.gesture_pinch_update(data, event)
        }

        fn gesture_pinch_end(
            &mut self,
            data: &mut Meridian,
            handle: &mut PointerInnerHandle<'_, Meridian>,
            event: &GesturePinchEndEvent,
        ) {
            handle.gesture_pinch_end(data, event)
        }

        fn gesture_hold_begin(
            &mut self,
            data: &mut Meridian,
            handle: &mut PointerInnerHandle<'_, Meridian>,
            event: &GestureHoldBeginEvent,
        ) {
            handle.gesture_hold_begin(data, event)
        }

        fn gesture_hold_end(
            &mut self,
            data: &mut Meridian,
            handle: &mut PointerInnerHandle<'_, Meridian>,
            event: &GestureHoldEndEvent,
        ) {
            handle.gesture_hold_end(data, event)
        }
    };
}


pub struct MoveGrab {
    /// Where the pointer was when the drag started.
    pub start_data: GrabStartData<Meridian>,
    pub window: Window,
    pub initial_window_location: Point<i32, Logical>,
}

// `PointerGrab` is a trait (an interface). Smithay calls these methods instead
// of delivering events to clients. Only `motion` and `button` do real work;
// the rest pass the event through unchanged.
impl PointerGrab<Meridian> for MoveGrab {
    fn motion(
        &mut self,
        data: &mut Meridian,
        handle: &mut PointerInnerHandle<'_, Meridian>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // Move the pointer, but with no client focused while dragging.
        handle.motion(data, None, event);

        let delta = event.location - self.start_data.location;
        let new_location = self.initial_window_location.to_f64() + delta;
        data.space
            .map_element(self.window.clone(), new_location.to_i32_round(), true);
    }

    fn button(
        &mut self,
        data: &mut Meridian,
        handle: &mut PointerInnerHandle<'_, Meridian>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);

        // All buttons released: the drag is over.
        if handle.current_pressed().is_empty() {
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn start_data(&self) -> &GrabStartData<Meridian> {
        &self.start_data
    }

    fn unset(&mut self, _data: &mut Meridian) {}

    forward_other_pointer_events!();
}

/// Which edges of a window are being dragged in a resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl From<xdg_toplevel::ResizeEdge> for Edges {
    fn from(edge: xdg_toplevel::ResizeEdge) -> Self {
        use xdg_toplevel::ResizeEdge as E;
        Self {
            left: matches!(edge, E::Left | E::TopLeft | E::BottomLeft),
            right: matches!(edge, E::Right | E::TopRight | E::BottomRight),
            top: matches!(edge, E::Top | E::TopLeft | E::TopRight),
            bottom: matches!(edge, E::Bottom | E::BottomLeft | E::BottomRight),
        }
    }
}

/// Bookkeeping for a resize, kept in `Meridian::resize`.
///
/// Resizing is a conversation: we propose a size, the client redraws and
/// commits. When the left or top edge is dragged, the window's position must
/// change too, and that can only happen once the client's new size arrives.
pub struct ResizeState {
    pub window: Window,
    pub edges: Edges,
    /// Position and size when the resize started.
    pub initial: Rect,
    /// The button was released; one more commit from the client is expected.
    pub finishing: bool,
}

pub struct ResizeGrab {
    pub start_data: GrabStartData<Meridian>,
    pub window: Window,
    pub edges: Edges,
    pub initial: Rect,
}

impl PointerGrab<Meridian> for ResizeGrab {
    fn motion(
        &mut self,
        data: &mut Meridian,
        handle: &mut PointerInnerHandle<'_, Meridian>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);

        let Some(toplevel) = self.window.toplevel() else { return };
        let delta = event.location - self.start_data.location;

        // Dragging the left/top edge outward makes the window bigger.
        let mut width = self.initial.size.w;
        let mut height = self.initial.size.h;
        if self.edges.left {
            width -= delta.x as i32;
        } else if self.edges.right {
            width += delta.x as i32;
        }
        if self.edges.top {
            height -= delta.y as i32;
        } else if self.edges.bottom {
            height += delta.y as i32;
        }

        // Respect the limits the client announced (0 means "no limit").
        let (min, max) = with_states(toplevel.wl_surface(), |states| {
            let mut cached = states.cached_state.get::<SurfaceCachedState>();
            let current = cached.current();
            (current.min_size, current.max_size)
        });
        let max_w = if max.w == 0 { i32::MAX } else { max.w };
        let max_h = if max.h == 0 { i32::MAX } else { max.h };
        let size = Size::from((
            width.clamp(min.w.max(1), max_w.max(min.w.max(1))),
            height.clamp(min.h.max(1), max_h.max(min.h.max(1))),
        ));

        toplevel.with_pending_state(|state| state.size = Some(size));
        toplevel.send_pending_configure();
    }

    fn button(
        &mut self,
        data: &mut Meridian,
        handle: &mut PointerInnerHandle<'_, Meridian>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if handle.current_pressed().is_empty() {
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn start_data(&self) -> &GrabStartData<Meridian> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut Meridian) {
        if let Some(toplevel) = self.window.toplevel() {
            toplevel.with_pending_state(|state| {
                state.states.unset(xdg_toplevel::State::Resizing);
            });
            toplevel.send_pending_configure();
        }
        if let Some(resize) = &mut data.resize {
            resize.finishing = true;
        }
    }

    forward_other_pointer_events!();
}

impl Meridian {
    /// Begin dragging `window`. Ignored when the layout manages positions.
    pub fn start_move(&mut self, window: Window, start_data: GrabStartData<Meridian>, serial: Serial) {
        if self.layout_is_managed() {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else { return };
        let Some(initial_window_location) = self.space.element_location(&window) else { return };

        let grab = MoveGrab {
            start_data,
            window,
            initial_window_location,
        };
        // `Focus::Clear`: no client receives pointer events during the drag.
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    /// Begin resizing `window` by the given edges.
    pub fn start_resize(
        &mut self,
        window: Window,
        edges: Edges,
        start_data: GrabStartData<Meridian>,
        serial: Serial,
    ) {
        if self.layout_is_managed() {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else { return };
        let Some(location) = self.space.element_location(&window) else { return };
        let initial = Rect::new(location, window.geometry().size);

        if let Some(toplevel) = window.toplevel() {
            // Tells the client a resize is in progress (it may skip animations).
            toplevel.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Resizing);
            });
            toplevel.send_pending_configure();
        }

        self.resize = Some(ResizeState {
            window: window.clone(),
            edges,
            initial,
            finishing: false,
        });
        let grab = ResizeGrab {
            start_data,
            window,
            edges,
            initial,
        };
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    /// Called when a window commits: keep the opposite edge in place while
    /// its left or top edge is being dragged.
    pub fn resize_commit(&mut self, surface: &WlSurface) {
        let Some(resize) = &self.resize else { return };
        if resize.window.toplevel().map(|t| t.wl_surface()) != Some(surface) {
            return;
        }

        if resize.edges.left || resize.edges.top {
            let size = resize.window.geometry().size;
            let mut location = self.space.element_location(&resize.window).unwrap_or(resize.initial.loc);
            if resize.edges.left {
                location.x = resize.initial.loc.x + resize.initial.size.w - size.w;
            }
            if resize.edges.top {
                location.y = resize.initial.loc.y + resize.initial.size.h - size.h;
            }
            self.space.map_element(resize.window.clone(), location, false);
        }

        if resize.finishing {
            self.resize = None;
        }
    }
}
