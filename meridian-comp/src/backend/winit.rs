//! Nested backend: Meridian's "monitor" is a window on the host desktop.
//! (winit is the Rust windowing library that creates that window.)


use std::time::Duration;

use anyhow::Context;
use smithay::{
    backend::{
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent, WinitEventLoop, WinitGraphicsBackend},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::{
        EventLoop,
        timer::{TimeoutAction, Timer},
    },
    utils::{Rectangle, Transform},
};

use super::{Backend, Scene, output_elements};
use crate::state::Meridian;

pub struct Winit {
    /// The host window plus its OpenGL renderer.
    pub graphics: WinitGraphicsBackend<GlesRenderer>,
    /// We asked the host for a redraw and it has not happened yet.
    redraw_requested: bool,
    /// Frames to draw in full regardless of damage (startup, resize).
    full_redraws: u8,
    /// A timer is pending that will send frame callbacks after a redraw
    /// in which nothing changed.
    callback_timer_armed: bool,
}

impl Winit {
    /// Ask the host to let us draw. Requests made before that happens are
    /// merged into one, so this is cheap to call for every change.
    pub fn queue_redraw(&mut self) {
        if !self.redraw_requested {
            self.redraw_requested = true;
            self.graphics.window().request_redraw();
        }
    }
}

/// Open the host window. Returns the backend and the window's event source,
/// which `attach` registers once the compositor state exists.
pub fn init() -> anyhow::Result<(Winit, WinitEventLoop)> {
    let (graphics, events) = winit::init::<GlesRenderer>()
        .map_err(|err| anyhow::anyhow!("failed to open the host window: {err}"))?;
    // Nobody should mistake this window for the real session.
    graphics
        .window()
        .set_title("Meridian (nested dev mode, not a full session)");
    Ok((
        Winit {
            graphics,
            redraw_requested: false,
            full_redraws: 4,
            callback_timer_armed: false,
        },
        events,
    ))
}

pub fn attach(
    event_loop: &mut EventLoop<Meridian>,
    state: &mut Meridian,
    winit_events: WinitEventLoop,
) -> anyhow::Result<()> {
    let Backend::Winit(winit) = &mut state.backend else {
        anyhow::bail!("winit::attach called without the winit backend");
    };

    // An Output is what Wayland calls a monitor. Ours has the size of the host window.
    let mode = Mode {
        size: winit.graphics.window_size(),
        refresh: 60_000, // millihertz
    };
    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Meridian".into(),
            model: "Nested".into(),
            serial_number: "Unknown".into(),
        },
    );
    // Advertise it to clients as a wl_output global.
    let _global = output.create_global::<Meridian>(&state.display_handle);
    // OpenGL's origin is bottom-left, so the nested output is drawn flipped.
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));

    // Remembers what was drawn last frame so unchanged regions can be skipped.
    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    // `move` transfers ownership of `output` and `damage_tracker` into the
    // closure, which then lives inside the event loop for the program's lifetime.
    event_loop
        .handle()
        .insert_source(winit_events, move |event, _, state| match event {
            WinitEvent::Resized { size, .. } => {
                let mode = Mode { size, refresh: 60_000 };
                output.change_current_state(Some(mode), None, None, None);
                output.set_preferred(mode);
                // Tiled windows must follow the new size.
                state.apply_layout();
                if let Backend::Winit(winit) = &mut state.backend {
                    winit.full_redraws = 4; // the buffers are new
                }
                state.queue_redraw("resize");
            }
            WinitEvent::Input(event) => state.process_input_event(event),
            // The host is ready for a frame we asked for (`queue_redraw`).
            // Nothing is drawn unless something asked.
            WinitEvent::Redraw => {
                // Computed first: these borrow all of `state`, which is not
                // possible once `state.backend` is borrowed mutably below.
                let frame_started = state.stats.start_frame();
                let focused = state.focused_window();
                let overview = state.overview_scene();
                let Backend::Winit(winit) = &mut state.backend else { return };
                winit.redraw_requested = false;
                let size = winit.graphics.window_size();
                let background = state.effects.theme().sky.to_array();

                let force = winit.full_redraws > 0;
                winit.full_redraws = winit.full_redraws.saturating_sub(1);

                // `Ok(true)`: something was drawn. `Ok(false)`: nothing changed.
                let rendered = match winit.graphics.bind() {
                    Ok((renderer, mut framebuffer)) => {
                        // No cursor: the host desktop draws it over our window.
                        let scene = Scene {
                            space: &state.space,
                            effects: &state.effects,
                            focused,
                            overview,
                            lock: state.lock.locked.then_some(state.lock.surfaces.as_slice()),
                            cursor: None,
                        };
                        let elements = output_elements(renderer, &output, &scene);

                        // First ask the damage tracker whether anything on
                        // screen differs from the last frame ("age 1"). If
                        // not, draw nothing. If so, repaint the whole
                        // window ("age 0").
                        //
                        // Repainting only the changed parts would need the
                        // age of the host's buffer. EGL only answers that
                        // while the window is the current drawing target,
                        // which our off-screen passes and client buffer
                        // imports change between frames. A dev-mode window
                        // is not worth that bookkeeping; the real session
                        // gets partial repaints from Smithay's DRM compositor.
                        let changed = match damage_tracker.damage_output(1, &elements) {
                            Ok((damage, _)) => damage.is_some(),
                            Err(_) => true,
                        };
                        if changed || force {
                            damage_tracker
                                .render_output(renderer, &mut framebuffer, 0, &elements, background)
                                .map(|_| true)
                                .map_err(|err| err.to_string())
                        } else {
                            Ok(false)
                        }
                    }
                    Err(err) => Err(err.to_string()),
                };

                let shown = match rendered {
                    Ok(true) => {
                        // Tell the host the whole window may have changed.
                        // It is only a hint, and always correct.
                        if let Err(err) = winit.graphics.submit(Some(&[Rectangle::from_size(size)])) {
                            tracing::warn!("failed to present the frame: {err}");
                        }
                        true
                    }
                    Ok(false) => false,
                    Err(err) => {
                        tracing::warn!("failed to render the frame: {err}");
                        false
                    }
                };
                state.stats.end_frame(frame_started, !shown);

                if shown {
                    // Clients may draw their next frame.
                    state.after_frame(&output);
                } else if let Backend::Winit(winit) = &mut state.backend
                    && !winit.callback_timer_armed
                {
                    // Nothing was shown. Clients still need their "draw the
                    // next frame" signal, but not instantly, or a client
                    // that keeps committing unchanged content would spin.
                    // Send it one frame interval later.
                    winit.callback_timer_armed = true;
                    let output = output.clone();
                    let armed = state.loop_handle.insert_source(
                        Timer::from_duration(Duration::from_millis(16)),
                        move |_, _, state| {
                            if let Backend::Winit(winit) = &mut state.backend {
                                winit.callback_timer_armed = false;
                            }
                            state.after_frame(&output);
                            TimeoutAction::Drop
                        },
                    );
                    if armed.is_err()
                        && let Backend::Winit(winit) = &mut state.backend
                    {
                        winit.callback_timer_armed = false;
                    }
                }
            }
            WinitEvent::CloseRequested => state.loop_signal.stop(),
            _ => {}
        })
        .map_err(|err| anyhow::anyhow!("{err}"))
        .context("failed to watch the host window")?;

    // Draw the first frame; after that only on request.
    state.queue_redraw("startup");

    Ok(())
}
