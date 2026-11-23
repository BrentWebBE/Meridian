//! Input: decide for every key/pointer event whether Meridian handles it
//! itself (shortcuts, moving windows) or forwards it to the focused client.

use meridian_common::config::Action;
use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, KeyState,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent, InputTime,
    },
    input::{
        keyboard::{FilterResult, Keysym, keysyms},
        pointer::{AxisFrame, ButtonEvent, GrabStartData, MotionEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{
    grabs::Edges,
    keys::{self, Modifiers},
    state::Meridian,
};

// Linux input codes of the mouse buttons (linux/input-event-codes.h).
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

/// What the keyboard filter decided to do with a key press.
enum KeyAction {
    Run(Action),
    ChangeVt(i32),
    /// Swallow the key without doing anything.
    Nothing,
}

impl Meridian {
    // `I: InputBackend` makes this generic: the same code handles events from
    // the host window (winit) and from real devices (libinput).
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => self.on_key::<I>(event),
            // Real mice report movement ("3 px right"), the nested window
            // reports positions ("at 40%, 60% of the window").
            InputEvent::PointerMotion { event, .. } => {
                let pointer = self.seat.get_pointer().expect("seat has a pointer");
                let pos = self.clamp_to_outputs(pointer.current_location() + event.delta());
                self.move_pointer(pos, event.time());
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                let Some(area) = self.output_area() else { return };
                let pos = event.position_transformed(area.size) + area.loc.to_f64();
                self.move_pointer(pos, event.time());
            }
            InputEvent::PointerButton { event, .. } => self.on_pointer_button::<I>(event),
            InputEvent::PointerAxis { event, .. } => self.on_pointer_axis::<I>(event),
            _ => {}
        }
    }

    fn on_key<I: InputBackend>(&mut self, event: I::KeyboardKeyEvent) {
        let serial = SERIAL_COUNTER.next_serial();
        let time = Event::time(&event);
        let keycode = event.key_code();
        let key_state = event.state();
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");

        // `input` updates the keyboard state (modifiers, layout) and then calls
        // our filter. `Forward` sends the key to the focused client;
        // `Intercept(x)` keeps it from the client and returns `Some(x)` to us.
        let action = keyboard.input(self, keycode, key_state, serial, time, |state, modifiers, handle| {
            // Tapping Super on its own (press and release with nothing in
            // between) opens Find. Only in a real session: nested, the host
            // desktop owns the Super key.
            let is_super = matches!(handle.modified_sym(), Keysym::Super_L | Keysym::Super_R);
            if key_state == KeyState::Pressed {
                state.super_tap = is_super && state.backend.mod_is_logo();
            } else if is_super && state.super_tap {
                state.super_tap = false;
                state.open_find_after_key = true;
            }

            if key_state == KeyState::Released {
                // Swallow the release of a key whose press we intercepted.
                return match state.suppressed_keys.iter().position(|k| *k == keycode) {
                    Some(index) => {
                        state.suppressed_keys.remove(index);
                        FilterResult::Intercept(KeyAction::Nothing)
                    }
                    None => FilterResult::Forward,
                };
            }

            // Ctrl+Alt+F1..F12 arrive as special "switch VT" keysyms.
            let sym = handle.modified_sym().raw();
            let action = if (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12).contains(&sym) {
                Some(KeyAction::ChangeVt((sym - keysyms::KEY_XF86Switch_VT_1 + 1) as i32))
            } else {
                // `raw_syms` is the key without Shift applied, so "Mod+Shift+E"
                // matches on `e` plus the Shift modifier.
                keys::find_action(&state.bindings, modifiers, &handle.raw_syms()).map(KeyAction::Run)
            };

            match action {
                Some(action) => {
                    state.suppressed_keys.push(keycode);
                    FilterResult::Intercept(action)
                }
                None => FilterResult::Forward,
            }
        });

        // Set inside the filter above; acted on here, where `self` is free again.
        if std::mem::take(&mut self.open_find_after_key) {
            self.run_action(Action::Spawn("meridian-launcher".into()));
        }

        match action {
            Some(KeyAction::Run(action)) => self.run_action(action),
            Some(KeyAction::ChangeVt(vt)) => self.change_vt(vt),
            Some(KeyAction::Nothing) | None => {}
        }
    }

    /// Keep the pointer on a monitor. Monitors sit side by side in a row.
    fn clamp_to_outputs(&self, pos: Point<f64, Logical>) -> Point<f64, Logical> {
        let geometries: Vec<_> = self
            .space
            .outputs()
            .filter_map(|output| self.space.output_geometry(output))
            .collect();
        let (Some(first), Some(last)) = (geometries.first(), geometries.last()) else {
            return pos;
        };

        let x = pos.x.clamp(first.loc.x as f64, (last.loc.x + last.size.w - 1) as f64);
        // The monitor at this x decides how far up and down the pointer can go.
        let column = geometries
            .iter()
            .find(|geo| x >= geo.loc.x as f64 && x < (geo.loc.x + geo.size.w) as f64)
            .unwrap_or(first);
        let y = pos.y.clamp(column.loc.y as f64, (column.loc.y + column.size.h - 1) as f64);
        (x, y).into()
    }

    fn move_pointer(&mut self, pos: Point<f64, Logical>, time: InputTime) {
        let under = self.surface_under(pos);
        let pointer = self.seat.get_pointer().expect("seat has a pointer");

        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        // Wayland groups pointer events into frames; this ends the group.
        pointer.frame(self);
    }

    fn on_pointer_button<I: InputBackend>(&mut self, event: I::PointerButtonEvent) {
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        let serial = SERIAL_COUNTER.next_serial();
        let button = event.button_code();

        let location = pointer.current_location();
        let on_layer = self.layer_surface_under(location);
        // A mouse button between Super press and release means "Super+drag", not a tap.
        self.super_tap = false;

        // In the overview, clicks pick a thumbnail or a space instead of
        // going to a client. Bar, dock and the search field still work.
        if self.overview && on_layer.is_none() {
            if event.state() == ButtonState::Pressed {
                self.overview_click(location);
            }
            return;
        }

        if event.state() == ButtonState::Pressed
            && !pointer.is_grabbed()
            && let Some(layer) = &on_layer
        {
            // Clicking the bar or dock leaves window focus alone, unless the
            // layer surface wants the keyboard (a launcher's search field).
            if layer.can_receive_keyboard_focus() {
                keyboard.set_focus(self, Some(layer.wl_surface().clone()), serial);
            }
        } else if event.state() == ButtonState::Pressed && !pointer.is_grabbed() {
            let window = self.space.element_under(location).map(|(w, _)| w.clone());

            // Click-to-focus: clicking a window focuses and raises it,
            // clicking the empty desktop clears the focus.
            self.focus_window(window.as_ref());

            // Mod + left-drag moves, Mod + right-drag resizes, wherever you grab.
            let held = Modifiers::from(&keyboard.modifier_state());
            let mod_held = if self.backend.mod_is_logo() { held.logo } else { held.alt };

            if let Some(window) = window
                && mod_held
            {
                // A "grab" redirects all pointer events to the compositor until released.
                let start_data = GrabStartData {
                    focus: None,
                    button,
                    location,
                };
                match button {
                    BTN_LEFT => self.start_move(window, start_data, serial),
                    BTN_RIGHT => {
                        // Resize from the corner nearest to the pointer.
                        let geo = self.space.element_geometry(&window).unwrap_or_default();
                        let center = geo.loc.to_f64() + geo.size.to_f64().to_point().downscale(2.0);
                        let edges = Edges {
                            left: location.x < center.x,
                            right: location.x >= center.x,
                            top: location.y < center.y,
                            bottom: location.y >= center.y,
                        };
                        self.start_resize(window, edges, start_data, serial);
                    }
                    _ => {}
                }
            }
        }

        // With a grab active this goes to the grab, otherwise to the client.
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state: event.state(),
                serial,
                time: event.time(),
            },
        );
        pointer.frame(self);
    }

    fn on_pointer_axis<I: InputBackend>(&mut self, event: I::PointerAxisEvent) {
        // Scrolling. Wheels report steps of 120 ("v120"), touchpads report pixels.
        let mut frame = AxisFrame::new(event.time()).source(event.source());

        for axis in [Axis::Horizontal, Axis::Vertical] {
            let v120 = event.amount_v120(axis);
            let amount = event
                .amount(axis)
                .unwrap_or_else(|| v120.unwrap_or(0.0) * 15.0 / 120.0);

            if amount != 0.0 {
                frame = frame.value(axis, amount);
                if let Some(v120) = v120 {
                    frame = frame.v120(axis, v120 as i32);
                }
            } else if event.source() == AxisSource::Finger && event.amount(axis) == Some(0.0) {
                // Fingers lifted from the touchpad: tells clients to stop kinetic scrolling.
                frame = frame.stop(axis);
            }
        }

        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        pointer.axis(self, frame);
        pointer.frame(self);
    }
}
