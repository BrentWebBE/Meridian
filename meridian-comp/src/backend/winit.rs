//! Nested backend: Meridian's "monitor" is a window on the host desktop.
//! (winit is the Rust windowing library that creates that window.)


use anyhow::Context;
use meridian_common::theme;
use smithay::{
    backend::{
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent, WinitEventLoop, WinitGraphicsBackend},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::{Rectangle, Transform},
};

use super::{Backend, Scene, output_elements};
use crate::state::Meridian;

pub struct Winit {
    /// The host window plus its OpenGL renderer.
    pub graphics: WinitGraphicsBackend<GlesRenderer>,
}

/// Open the host window. Returns the backend and the window's event source,
/// which `attach` registers once the compositor state exists.
pub fn init() -> anyhow::Result<(Winit, WinitEventLoop)> {
    let (graphics, events) = winit::init::<GlesRenderer>()
        .map_err(|err| anyhow::anyhow!("failed to open the host window: {err}"))?;
    Ok((Winit { graphics }, events))
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
    let background = theme::LIGHT.wallpaper.sky.to_array();

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
            }
            WinitEvent::Input(event) => state.process_input_event(event),
            WinitEvent::Redraw => {
                // Computed first: these borrow all of `state`, which is not
                // possible once `state.backend` is borrowed mutably below.
                let focused = state.focused_window();
                let overview = state.overview_scene();
                let Backend::Winit(winit) = &mut state.backend else { return };
                let size = winit.graphics.window_size();

                let rendered = match winit.graphics.bind() {
                    Ok((renderer, mut framebuffer)) => {
                        // No cursor: the host desktop draws it over our window.
                        let scene = Scene {
                            space: &state.space,
                            effects: &state.effects,
                            focused,
                            overview,
                            cursor: None,
                        };
                        let elements = output_elements(renderer, &output, &scene);
                        damage_tracker
                            // Buffer age 0 = redraw everything.
                            .render_output(renderer, &mut framebuffer, 0, &elements, background)
                            .map(|_| ())
                            .map_err(|err| err.to_string())
                    }
                    Err(err) => Err(err.to_string()),
                };

                match rendered {
                    Ok(()) => {
                        if let Err(err) = winit.graphics.submit(Some(&[Rectangle::from_size(size)])) {
                            tracing::warn!("failed to present the frame: {err}");
                        }
                    }
                    Err(err) => tracing::warn!("failed to render the frame: {err}"),
                }
                winit.graphics.window().request_redraw();

                state.after_frame(&output);
            }
            WinitEvent::CloseRequested => state.loop_signal.stop(),
            _ => {}
        })
        .map_err(|err| anyhow::anyhow!("{err}"))
        .context("failed to watch the host window")?;

    Ok(())
}
