//! Backends: where Meridian's pixels go and where its input comes from.
//!
//! * `winit`: nested, a window on your current desktop (for development).
//! * `tty`: a real session that owns the monitors and input devices.

pub mod tty;
pub mod winit;

use smithay::{
    backend::renderer::{
        element::{
            Kind,
            memory::MemoryRenderBufferRenderElement,
            render_elements,
            surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
        },
        gles::GlesRenderer,
    },
    desktop::{
        Space, Window,
        space::{SpaceRenderElements, space_render_elements},
    },
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    output::Output,
    utils::{Logical, Point, Scale},
    wayland::compositor::with_states,
};

use crate::cursor::Cursor;

/// An `enum` holds exactly one of its variants. Code that needs a specific
/// backend uses `match` / `if let` to find out which one is active.
pub enum Backend {
    // `Box` stores the (large) backend structs on the heap so the enum stays small.
    Winit(Box<winit::Winit>),
    Tty(Box<tty::Tty>),
}

impl Backend {
    /// The OpenGL renderer that draws the desktop.
    pub fn renderer(&mut self) -> &mut GlesRenderer {
        match self {
            Backend::Winit(winit) => winit.graphics.renderer(),
            Backend::Tty(tty) => &mut tty.renderer,
        }
    }

    /// Name of the seat (set of input devices) we run on.
    pub fn seat_name(&self) -> String {
        match self {
            Backend::Winit(_) => "seat0".into(),
            Backend::Tty(tty) => tty.seat_name(),
        }
    }

    /// "Mod" is Super on a real session. Nested we use Alt, because the host
    /// desktop keeps Super for itself.
    pub fn mod_is_logo(&self) -> bool {
        matches!(self, Backend::Tty(_))
    }
}

// Everything we draw on an output is a "render element". The renderer wants
// one list with a single element type, so this macro generates an enum that
// wraps the kinds we use.
render_elements! {
    pub OutputElement<=GlesRenderer>;
    Space=SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
    CursorImage=MemoryRenderBufferRenderElement<GlesRenderer>,
    CursorSurface=WaylandSurfaceRenderElement<GlesRenderer>,
}

/// What to draw for the pointer, and where.
pub struct CursorFrame<'a> {
    pub status: &'a CursorImageStatus,
    pub image: &'a Cursor,
    /// Pointer position in the global space.
    pub location: Point<f64, Logical>,
}

/// Build the list of things to draw on `output`, front to back.
pub fn output_elements(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    output: &Output,
    cursor: Option<CursorFrame<'_>>,
) -> Vec<OutputElement> {
    let mut elements = Vec::new();
    let scale = Scale::from(output.current_scale().fractional_scale());

    // The cursor comes first because it is on top of everything.
    if let Some(cursor) = cursor
        && let Some(output_geo) = space.output_geometry(output)
    {
        // Position relative to this output's top-left corner.
        let position = cursor.location - output_geo.loc.to_f64();

        match cursor.status {
            CursorImageStatus::Hidden => {}
            // The client supplied its own cursor surface (e.g. a text I-beam).
            CursorImageStatus::Surface(surface) => {
                let hotspot = with_states(surface, |states| {
                    states
                        .data_map
                        .get::<CursorImageSurfaceData>()
                        .map(|data| data.lock().unwrap().hotspot)
                        .unwrap_or_default()
                });
                let location = (position - hotspot.to_f64()).to_physical(scale).to_i32_round();
                elements.extend(
                    render_elements_from_surface_tree(renderer, surface, location, scale, 1.0, Kind::Cursor)
                        .into_iter()
                        .map(OutputElement::CursorSurface),
                );
            }
            // A named shape ("pointer", "text", ...). We only have the arrow for now.
            CursorImageStatus::Named(_) => {
                let location = (position - cursor.image.hotspot.to_f64()).to_physical(scale);
                match MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    location,
                    &cursor.image.buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                ) {
                    Ok(element) => elements.push(OutputElement::CursorImage(element)),
                    Err(err) => tracing::warn!("failed to draw the cursor: {err}"),
                }
            }
        }
    }

    // Then the windows, already sorted top to bottom by the space.
    match space_render_elements(renderer, [space], output, 1.0) {
        Ok(space_elements) => elements.extend(space_elements.into_iter().map(OutputElement::Space)),
        Err(err) => tracing::warn!("output has no mode yet: {err}"),
    }

    elements
}

/// Draw `output` into an off-screen texture and save it as a PNG.
pub fn screenshot(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    output: &Output,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    use anyhow::Context;
    use smithay::{
        backend::{
            allocator::Fourcc,
            renderer::{Bind, ExportMem, Offscreen, damage::OutputDamageTracker, gles::GlesTexture},
        },
        utils::{Rectangle, Transform},
    };

    let mode = output.current_mode().context("output has no mode")?;
    let (width, height) = (mode.size.w, mode.size.h);
    let scale = output.current_scale().fractional_scale();
    let background = meridian_common::theme::LIGHT.desktop_background.to_array();

    let elements = output_elements(renderer, space, output, None);

    let mut texture: GlesTexture = renderer
        .create_buffer(Fourcc::Abgr8888, (width, height).into())
        .context("cannot create the off-screen buffer")?;

    let pixels = {
        let mut framebuffer = renderer.bind(&mut texture).context("cannot bind the buffer")?;
        // A fresh damage tracker with a normal transform: draw everything, upright.
        let mut tracker = OutputDamageTracker::new((width, height), scale, Transform::Normal);
        tracker
            .render_output(renderer, &mut framebuffer, 0, &elements, background)
            .map_err(|err| anyhow::anyhow!("render failed: {err}"))?;

        let region = Rectangle::from_size((width, height).into());
        let mapping = renderer
            .copy_framebuffer(&framebuffer, region, Fourcc::Abgr8888)
            .context("cannot read the pixels back")?;
        renderer.map_texture(&mapping).context("cannot map the pixels")?.to_vec()
    };

    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&pixels)?;
    Ok(())
}
