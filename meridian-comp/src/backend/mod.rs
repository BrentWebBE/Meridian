//! Backends: where Meridian's pixels go and where its input comes from.
//!
//! * `winit`: nested, a window on your current desktop (for development).
//! * `tty`: a real session that owns the monitors and input devices.

pub mod tty;
pub mod winit;

use smithay::{
    backend::renderer::{
        element::{
            AsRenderElements, Kind,
            memory::MemoryRenderBufferRenderElement,
            render_elements,
            surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
        },
        gles::{GlesRenderer, element::PixelShaderElement},
    },
    desktop::{PopupManager, Space, Window, layer_map_for_output},
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    output::Output,
    utils::{Logical, Physical, Point, Scale},
    wayland::{compositor::with_states, shell::wlr_layer::Layer},
};

use crate::{
    effects::{self, Effects, RoundedTextureElement, Shadow},
    layout::Rect,
    overview::OverviewScene,
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
    // Client surfaces drawn as they are: bar, dock, popups, cursor surfaces.
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    CursorImage=MemoryRenderBufferRenderElement<GlesRenderer>,
    // Windows, drawn through the rounded-corner shader (effects.rs).
    Rounded=RoundedTextureElement,
    // Shadows and other shader-drawn shapes.
    Shader=PixelShaderElement,
}

/// What to draw for the pointer, and where.
pub struct CursorFrame<'a> {
    pub status: &'a CursorImageStatus,
    pub image: &'a Cursor,
    /// Pointer position in the global space.
    pub location: Point<f64, Logical>,
}

/// Everything `output_elements` needs to know about the desktop.
pub struct Scene<'a> {
    pub space: &'a Space<Window>,
    pub effects: &'a Effects,
    /// Gets the stronger shadow.
    pub focused: Option<Window>,
    /// Present while the overview is open; replaces the normal windows.
    pub overview: Option<OverviewScene>,
    pub cursor: Option<CursorFrame<'a>>,
}

/// Build the list of things to draw on `output`, front to back.
pub fn output_elements(renderer: &mut GlesRenderer, output: &Output, scene: &Scene<'_>) -> Vec<OutputElement> {
    let mut elements = Vec::new();
    let scale = output.current_scale().fractional_scale();
    let Some(output_geo) = scene.space.output_geometry(output) else {
        return elements;
    };

    // 1. The cursor is on top of everything.
    if let Some(cursor) = &scene.cursor {
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
                let location: Point<i32, Physical> = (position - hotspot.to_f64()).to_physical(scale).to_i32_round();
                elements.extend(
                    render_elements_from_surface_tree(renderer, surface, location, Scale::from(scale), 1.0, Kind::Cursor)
                        .into_iter()
                        .map(OutputElement::Surface),
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

    // 2. Bar, dock, launcher, notifications.
    layer_elements(renderer, output, scale, &[Layer::Overlay, Layer::Top], &mut elements);

    // 3. Windows, or their thumbnails while the overview is open.
    match &scene.overview {
        Some(overview) => overview_elements(renderer, scene, overview, output_geo, scale, &mut elements),
        None => {
            // `elements()` is ordered bottom to top; we need the topmost first.
            for window in scene.space.elements().rev() {
                let Some(location) = scene.space.element_location(window) else { continue };
                let rect = Rect::new(location - output_geo.loc, window.geometry().size);
                let focused = scene.focused.as_ref() == Some(window);
                window_elements(renderer, scene.effects, window, rect, focused, scale, &mut elements);
            }
        }
    }

    // 4. Desktop layers from clients, then our own wallpaper at the very bottom.
    layer_elements(renderer, output, scale, &[Layer::Bottom, Layer::Background], &mut elements);
    elements.push(OutputElement::Shader(scene.effects.wallpaper(output, output_geo.size)));

    elements
}

/// The surfaces of the given layers, topmost first.
fn layer_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    scale: f64,
    layers: &[Layer],
    elements: &mut Vec<OutputElement>,
) {
    let map = layer_map_for_output(output);
    for layer in layers {
        // Within one layer, surfaces mapped later are on top.
        for surface in map.layers_on(*layer).rev() {
            let Some(geometry) = map.layer_geometry(surface) else { continue };
            let location: Point<i32, Physical> = geometry.loc.to_physical_precise_round(scale);
            elements.extend(
                surface
                    .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                        renderer,
                        location,
                        Scale::from(scale),
                        1.0,
                    )
                    .into_iter()
                    .map(OutputElement::Surface),
            );
        }
    }
}

/// One window at full size: its popups, its rounded body, its shadow.
/// `rect` is where the window is on this output.
fn window_elements(
    renderer: &mut GlesRenderer,
    effects: &Effects,
    window: &Window,
    rect: Rect,
    focused: bool,
    scale: f64,
    elements: &mut Vec<OutputElement>,
) {
    let Some(toplevel) = window.toplevel() else { return };

    // Popups (menus, tooltips) may reach beyond the window, so they are drawn
    // directly and not through the window's clipped texture.
    for (popup, popup_offset) in PopupManager::popups_for_surface(toplevel.wl_surface()) {
        let location: Point<i32, Physical> =
            (rect.loc + popup_offset - popup.geometry().loc).to_physical_precise_round(scale);
        elements.extend(
            render_elements_from_surface_tree(
                renderer,
                popup.wl_surface(),
                location,
                Scale::from(scale),
                1.0,
                Kind::Unspecified,
            )
            .into_iter()
            .map(OutputElement::Surface),
        );
    }

    let body = effects::with_window_texture(renderer, window, scale, |texture| {
        effects.rounded(
            texture,
            rect.loc.to_f64().to_physical(scale),
            None,
            effects::RADIUS,
            scale,
            false,
        )
    });
    if let Some(body) = body {
        elements.push(OutputElement::Rounded(body));
        elements.extend(
            effects
                .window_backdrop(window, rect, focused)
                .into_iter()
                .map(OutputElement::Shader),
        );
    }
}

/// The overview: window thumbnails, the spaces strip, and a dimmed backdrop.
fn overview_elements(
    renderer: &mut GlesRenderer,
    scene: &Scene<'_>,
    overview: &OverviewScene,
    output_geo: Rect,
    scale: f64,
    elements: &mut Vec<OutputElement>,
) {
    let effects = scene.effects;
    let theme = meridian_common::theme::LIGHT;
    // Overview rectangles are in global coordinates; make them output-local.
    let local = |rect: Rect| Rect::new(rect.loc - output_geo.loc, rect.size);

    // A scaled-down copy of a window, with corners scaled to match.
    let thumbnail = |renderer: &mut GlesRenderer, window: &Window, rect: Rect, elements: &mut Vec<OutputElement>| {
        let full_width = window.geometry().size.w.max(1) as f32;
        let radius = (effects::RADIUS * rect.size.w as f32 / full_width).max(2.0);
        let body = effects::with_window_texture(renderer, window, scale, |texture| {
            effects.rounded(
                texture,
                rect.loc.to_f64().to_physical(scale),
                Some(rect.size),
                radius,
                scale,
                true,
            )
        });
        if let Some(body) = body {
            elements.push(OutputElement::Rounded(body));
        }
        radius
    };

    // Spaces strip: one tile per space with miniature windows inside.
    for tile in &overview.spaces {
        let rect = local(tile.rect);
        for (window, mini) in &tile.windows {
            thumbnail(renderer, window, local(*mini), elements);
        }
        elements.push(OutputElement::Shader(effects.rounded_rect(
            rect,
            8.0,
            theme.wallpaper.sky.to_array(),
        )));
        // A ring around the tile: accent color for the active space.
        let ring = Rect::new((rect.loc.x - 2, rect.loc.y - 2).into(), (rect.size.w + 4, rect.size.h + 4).into());
        let ring_color = if tile.active { theme.accent } else { theme.surface_raised };
        elements.push(OutputElement::Shader(effects.rounded_rect(ring, 10.0, ring_color.to_array())));
    }

    // Window thumbnails with shadows.
    for (window, rect) in &overview.thumbnails {
        let rect = local(*rect);
        let radius = thumbnail(renderer, window, rect, elements);
        elements.push(OutputElement::Shader(effects.shadow(rect, radius, Shadow::Focused)));
    }

    // Dim the desktop behind everything.
    let dim = theme.text.with_alpha(0.28).to_array();
    elements.push(OutputElement::Shader(effects.rounded_rect(
        Rect::new((0, 0).into(), output_geo.size),
        0.0,
        dim,
    )));
}

/// Draw `output` into an off-screen texture and save it as a PNG.
pub fn screenshot(
    renderer: &mut GlesRenderer,
    output: &Output,
    scene: &Scene<'_>,
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
    let background = meridian_common::theme::LIGHT.wallpaper.sky.to_array();

    let elements = output_elements(renderer, output, scene);

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
