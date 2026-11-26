//! Rounded corners and shadows.
//!
//! Clients draw rectangles. To round a window's corners the compositor has
//! to cut them off itself, and that takes two steps:
//!
//! 1. Draw the window (all its surfaces) into an off-screen texture.
//! 2. Draw that texture to the screen with a small GPU program (a "shader")
//!    that makes the pixels outside a rounded rectangle transparent.
//!
//! Shadows need no texture at all: a second shader computes, for every pixel
//! around the window, how far it is from the window's edge and fades with it.

use std::cell::{Cell, RefCell};

use meridian_common::theme::{Mood, Theme};

use anyhow::Context;
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, ImportMem, Offscreen,
            damage::OutputDamageTracker,
            element::{
                Element, Id, Kind, RenderElement,
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
                texture::{TextureBuffer, TextureRenderBuffer, TextureRenderElement},
            },
            gles::{
                GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, GlesTexture, Uniform,
                UniformName, UniformType, element::PixelShaderElement,
            },
            utils::{CommitCounter, DamageSet, OpaqueRegions},
        },
    },
    desktop::Window,
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform, user_data::UserDataMap},
};

use crate::layout::Rect;

/// Corner radius of windows, in logical pixels.
pub const RADIUS: f32 = meridian_common::theme::WINDOW_RADIUS as f32;

/// How far a shadow reaches beyond its window.
const SHADOW_MARGIN: i32 = 48;

// GLSL, the OpenGL shading language. `main` runs once per pixel on the GPU.
// `v_coords` goes from (0,0) at the top-left of what we draw to (1,1) at the
// bottom-right.

/// The distance from point `p` to a rounded rectangle centered on the origin.
/// Negative inside, positive outside. Shared by both shaders.
const ROUNDED_BOX: &str = r#"
float rounded_box(vec2 p, vec2 half_size, float radius) {
    vec2 q = abs(p) - (half_size - vec2(radius));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}
"#;

/// Draws a texture, transparent outside a rounded rectangle.
const ROUNDED_TEXTURE: &str = r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec2 box_size;   // size on screen, in pixels
uniform float radius;
uniform float top_radius; // 0.0 below a title bar, which has the rounded corners

ROUNDED_BOX

void main() {
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif

    vec2 p = (v_coords - 0.5) * box_size;
    float distance = rounded_box(p, box_size / 2.0, p.y < 0.0 ? top_radius : radius);
    // One pixel of smooth transition avoids jagged corners.
    float inside = 1.0 - smoothstep(-0.5, 0.5, distance);
    gl_FragColor = color * alpha * inside;
}
"#;

/// Draws a rounded rectangle with a soft edge. With a large `softness` it is
/// a shadow; with a tiny one it is a crisp filled shape.
const SOFT_BOX: &str = r#"
precision highp float;
uniform float alpha;
uniform vec2 size;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec2 area_size;  // the area we draw into, in pixels
uniform vec4 box;        // the rectangle inside it: x, y, width, height
uniform float radius;
uniform float softness;  // width of the fade, in pixels
uniform vec4 color;      // straight (not premultiplied) RGBA

ROUNDED_BOX

void main() {
    vec2 center = box.xy + box.zw / 2.0;
    float distance = rounded_box(v_coords * area_size - center, box.zw / 2.0, radius);
    // Fully covered up to the edge, then fading out over `softness` pixels.
    float coverage = 1.0 - smoothstep(0.0, softness, distance);
    // A squared falloff looks closer to a real (gaussian) shadow.
    coverage = softness > 1.0 ? coverage * coverage : coverage;
    float a = color.a * coverage * alpha;
    gl_FragColor = vec4(color.rgb * a, a);   // premultiplied alpha
}
"#;

/// The wallpaper: a sky, a sun and three hills, each hill the top of a large
/// ellipse. Everything is positioned relative to the screen size.
const WALLPAPER: &str = r#"
precision highp float;
uniform float alpha;
uniform vec2 size;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec2 area_size;
uniform vec3 sky;
uniform vec3 sun;
uniform vec3 hill_far;
uniform vec3 hill_mid;
uniform vec3 hill_near;

// 1.0 inside the ellipse, 0.0 outside, with a soft one-pixel edge.
float ellipse(vec2 p, vec2 center, vec2 radius) {
    float d = (length((p - center) / radius) - 1.0) * min(radius.x, radius.y);
    return 1.0 - smoothstep(-0.75, 0.75, d);
}

void main() {
    vec2 p = v_coords * area_size;
    float w = area_size.x;
    float h = area_size.y;

    vec3 color = sky;
    color = mix(color, sun, ellipse(p, vec2(0.76 * w, 0.205 * h), vec2(0.083 * h)));
    // Each hill: where its top is (x, y) and how wide and tall its ellipse is.
    color = mix(color, hill_far,  ellipse(p, vec2(0.30 * w, 0.668 * h + 0.60 * h), vec2(0.70 * w, 0.60 * h)));
    color = mix(color, hill_mid,  ellipse(p, vec2(0.83 * w, 0.757 * h + 0.55 * h), vec2(0.62 * w, 0.55 * h)));
    color = mix(color, hill_near, ellipse(p, vec2(0.28 * w, 0.866 * h + 0.46 * h), vec2(0.62 * w, 0.46 * h)));

    gl_FragColor = vec4(color, 1.0) * alpha;
}
"#;

/// The compiled shader programs. Created once at startup.
pub struct Effects {
    rounded_texture: GlesTexProgram,
    soft_box: GlesPixelProgram,
    wallpaper: GlesPixelProgram,
    /// The colors in use. `Cell` lets `set_theme` work through `&self`.
    theme: Cell<Theme>,
    /// The user's wallpaper image, if one is set and has been loaded.
    wallpaper_image: RefCell<Option<WallpaperTexture>>,
    /// Which effects are switched on (from the config).
    flags: Cell<meridian_common::config::Effects>,
    /// The drawn landscape as ready-made textures, one per output size.
    landscapes: RefCell<Vec<Landscape>>,
    /// A landscape a render pass found missing; picked up after the frame.
    landscape_wanted: Cell<Option<(Size<i32, Physical>, Theme)>>,
    /// The landscape a worker thread is painting right now.
    landscape_pending: Cell<Option<(Size<i32, Physical>, Theme)>>,
}

/// The landscape painted for one output size and theme.
struct Landscape {
    size: Size<i32, Physical>,
    theme: Theme,
    buffer: TextureBuffer<GlesTexture>,
}

impl Effects {
    pub fn theme(&self) -> Theme {
        self.theme.get()
    }

    /// Switch colors. Cached elements notice on their next use.
    pub fn set_theme(&self, theme: Theme) {
        self.theme.set(theme);
    }

    pub fn flags(&self) -> meridian_common::config::Effects {
        self.flags.get()
    }

    pub fn set_flags(&self, flags: meridian_common::config::Effects) {
        self.flags.set(flags);
    }

    pub fn compile(
        renderer: &mut GlesRenderer,
        theme: Theme,
        flags: meridian_common::config::Effects,
    ) -> anyhow::Result<Self> {
        let rounded_texture = renderer
            .compile_custom_texture_shader(
                ROUNDED_TEXTURE.replace("ROUNDED_BOX", ROUNDED_BOX),
                &[
                    UniformName::new("box_size", UniformType::_2f),
                    UniformName::new("radius", UniformType::_1f),
                    UniformName::new("top_radius", UniformType::_1f),
                ],
            )
            .context("could not compile the rounded-corner shader")?;

        let soft_box = renderer
            .compile_custom_pixel_shader(
                SOFT_BOX.replace("ROUNDED_BOX", ROUNDED_BOX),
                &[
                    UniformName::new("area_size", UniformType::_2f),
                    UniformName::new("box", UniformType::_4f),
                    UniformName::new("radius", UniformType::_1f),
                    UniformName::new("softness", UniformType::_1f),
                    UniformName::new("color", UniformType::_4f),
                ],
            )
            .context("could not compile the shadow shader")?;

        let color = |name| UniformName::new(name, UniformType::_3f);
        let wallpaper = renderer
            .compile_custom_pixel_shader(
                WALLPAPER,
                &[
                    UniformName::new("area_size", UniformType::_2f),
                    color("sky"),
                    color("sun"),
                    color("hill_far"),
                    color("hill_mid"),
                    color("hill_near"),
                ],
            )
            .context("could not compile the wallpaper shader")?;

        Ok(Self {
            rounded_texture,
            soft_box,
            wallpaper,
            theme: Cell::new(theme),
            wallpaper_image: RefCell::new(None),
            flags: Cell::new(flags),
            landscapes: RefCell::new(Vec::new()),
            landscape_wanted: Cell::new(None),
            landscape_pending: Cell::new(None),
        })
    }

    /// Use an image as wallpaper. `pixels` is RGBA, 4 bytes per pixel.
    pub fn set_wallpaper_image(&self, renderer: &mut GlesRenderer, pixels: &[u8], width: u32, height: u32) {
        let size: Size<i32, Buffer> = (width as i32, height as i32).into();
        match renderer.import_memory(pixels, Fourcc::Abgr8888, size, false) {
            Ok(texture) => {
                let buffer = TextureBuffer::from_texture(renderer, texture, 1, Transform::Normal, None);
                *self.wallpaper_image.borrow_mut() = Some((buffer, size));
            }
            Err(err) => tracing::warn!("could not upload the wallpaper image: {err}"),
        }
    }

    /// Go back to the drawn landscape.
    pub fn clear_wallpaper_image(&self) {
        *self.wallpaper_image.borrow_mut() = None;
    }

    /// The wallpaper image scaled to cover an output of the given size,
    /// cropping whatever does not fit. `None` if no image is set.
    pub fn wallpaper_image(&self, size: Size<i32, Logical>) -> Option<TextureRenderElement<GlesTexture>> {
        let image = self.wallpaper_image.borrow();
        let (buffer, image_size) = image.as_ref()?;
        let (iw, ih) = (image_size.w as f64, image_size.h as f64);
        let (ow, oh) = (size.w.max(1) as f64, size.h.max(1) as f64);

        // The largest centered part of the image with the screen's proportions.
        let (crop_w, crop_h) = if iw / ih > ow / oh {
            (ih * ow / oh, ih) // image is wider: cut the sides
        } else {
            (iw, iw * oh / ow) // image is taller: cut top and bottom
        };
        let src = Rectangle::new(((iw - crop_w) / 2.0, (ih - crop_h) / 2.0).into(), (crop_w, crop_h).into());

        Some(TextureRenderElement::from_texture_buffer(
            (0.0, 0.0),
            buffer,
            None,
            Some(src),
            Some(size),
            Kind::Unspecified,
        ))
    }

    /// The drawn landscape as a plain texture, if it has been painted for
    /// this output size and the current theme. Drawing a ready texture is
    /// as cheap as it gets; the shader version computes every pixel anew.
    ///
    /// When the texture is missing this returns `None` (the caller falls
    /// back to the shader for now) and notes that it is wanted.
    pub fn landscape(
        &self,
        physical: Size<i32, Physical>,
        logical: Size<i32, Logical>,
    ) -> Option<TextureRenderElement<GlesTexture>> {
        let theme = self.theme.get();
        let landscapes = self.landscapes.borrow();
        match landscapes.iter().find(|l| l.size == physical && l.theme == theme) {
            Some(landscape) => Some(TextureRenderElement::from_texture_buffer(
                (0.0, 0.0),
                &landscape.buffer,
                None,
                None,
                Some(logical),
                Kind::Unspecified,
            )),
            None => {
                if self.landscape_pending.get() != Some((physical, theme)) {
                    self.landscape_wanted.set(Some((physical, theme)));
                }
                None
            }
        }
    }

    /// The landscape that should be painted next, if any. The caller hands
    /// it to a worker thread and later calls `set_landscape`.
    pub fn take_landscape_request(&self) -> Option<(Size<i32, Physical>, Theme)> {
        let wanted = self.landscape_wanted.take()?;
        self.landscape_pending.set(Some(wanted));
        Some(wanted)
    }

    /// Store a landscape painted by `paint_landscape` (RGBA pixels).
    pub fn set_landscape(&self, renderer: &mut GlesRenderer, pixels: &[u8], size: Size<i32, Physical>, theme: Theme) {
        self.landscape_pending.set(None);
        let buffer_size: Size<i32, Buffer> = (size.w, size.h).into();
        match renderer.import_memory(pixels, Fourcc::Abgr8888, buffer_size, false) {
            Ok(texture) => {
                let buffer = TextureBuffer::from_texture(renderer, texture, 1, Transform::Normal, None);
                let mut landscapes = self.landscapes.borrow_mut();
                // One per output size; older themes for that size are dropped.
                landscapes.retain(|l| l.size != size);
                landscapes.push(Landscape { size, theme, buffer });
                // A handful of monitors at most; do not grow without bound.
                if landscapes.len() > 4 {
                    landscapes.remove(0);
                }
            }
            Err(err) => tracing::warn!("could not upload the wallpaper: {err}"),
        }
    }

    /// The wallpaper for an output of the given size. Kept in the output's
    /// user data so that it counts as "unchanged" from frame to frame.
    pub fn wallpaper(&self, output: &smithay::output::Output, size: Size<i32, Logical>) -> PixelShaderElement {
        let cache = output
            .user_data()
            .get_or_insert(|| WallpaperCache(RefCell::new(None)));
        let mut cached = cache.0.borrow_mut();
        let theme = self.theme.get();
        if let Some((element, cached_size, cached_theme)) = cached.as_ref()
            && *cached_size == size
            && *cached_theme == theme.mood
        {
            return element.clone();
        }

        let rgb = |color: meridian_common::theme::Color| (color.r, color.g, color.b);
        let element = PixelShaderElement::new(
            self.wallpaper.clone(),
            Rect::new((0, 0).into(), size),
            None,
            1.0,
            vec![
                Uniform::new("area_size", (size.w as f32, size.h as f32)),
                Uniform::new("sky", rgb(theme.sky)),
                Uniform::new("sun", rgb(theme.sun)),
                Uniform::new("hill_far", rgb(theme.hills[0])),
                Uniform::new("hill_mid", rgb(theme.hills[1])),
                Uniform::new("hill_near", rgb(theme.hills[2])),
            ],
            Kind::Unspecified,
        );
        *cached = Some((element.clone(), size, theme.mood));
        element
    }

    /// A filled rounded rectangle (used by the overview).
    pub fn rounded_rect(&self, rect: Rect, radius: f32, color: [f32; 4]) -> PixelShaderElement {
        PixelShaderElement::new(
            self.soft_box.clone(),
            rect,
            None,
            1.0,
            rounded_rect_uniforms(rect, radius, color),
            Kind::Unspecified,
        )
    }

    /// The shadow of a window that occupies `rect`.
    pub fn shadow(&self, rect: Rect, radius: f32, strength: Shadow) -> PixelShaderElement {
        PixelShaderElement::new(
            self.soft_box.clone(),
            shadow_area(rect),
            None,
            1.0,
            shadow_uniforms(rect, radius, strength, self.theme.get()),
            Kind::Unspecified,
        )
    }

    /// What is drawn behind `window`: a thin accent outline when it is
    /// focused, and its shadow. Both are kept between frames: a render
    /// element that keeps its identity lets the damage tracker see that
    /// nothing changed, so an idle desktop is not redrawn.
    pub fn window_backdrop(&self, window: &Window, rect: Rect, focused: bool) -> Vec<PixelShaderElement> {
        let cache = window
            .user_data()
            .get_or_insert(|| BackdropCache(RefCell::new(None)));
        let mut cached = cache.0.borrow_mut();
        let theme = self.theme.get();

        let up_to_date = cached
            .as_ref()
            .is_some_and(|backdrop| backdrop.rect == rect && backdrop.focused == focused && backdrop.theme == theme.mood);
        if !up_to_date {
            let strength = if focused { Shadow::Focused } else { Shadow::Normal };
            // The outline is a slightly larger rounded rectangle peeking out
            // from behind the window.
            let outline = Rect::new((rect.loc.x - 1, rect.loc.y - 1).into(), (rect.size.w + 2, rect.size.h + 2).into());
            let outline_color = if focused { theme.accent.with_alpha(0.5) } else { theme.border() };

            match cached.as_mut() {
                // Reuse the existing elements (and their ids), only moving them.
                Some(backdrop) => {
                    backdrop.shadow.resize(shadow_area(rect), None);
                    backdrop.shadow.update_uniforms(shadow_uniforms(rect, RADIUS, strength, theme));
                    backdrop.outline.resize(outline, None);
                    backdrop
                        .outline
                        .update_uniforms(rounded_rect_uniforms(outline, RADIUS + 1.0, outline_color.to_array()));
                    backdrop.rect = rect;
                    backdrop.focused = focused;
                    backdrop.theme = theme.mood;
                }
                None => {
                    *cached = Some(Backdrop {
                        shadow: self.shadow(rect, RADIUS, strength),
                        outline: self.rounded_rect(outline, RADIUS + 1.0, outline_color.to_array()),
                        rect,
                        focused,
                        theme: theme.mood,
                    });
                }
            }
        }

        match cached.as_ref() {
            Some(backdrop) if self.flags.get().shadows => vec![backdrop.outline.clone(), backdrop.shadow.clone()],
            // Shadows off: only the thin outline that marks the focus.
            Some(backdrop) => vec![backdrop.outline.clone()],
            None => Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)] // each one is a distinct aspect of how to draw
    /// Draw `texture` at `location` with rounded corners.
    /// `size` scales it (thumbnails); `None` draws it at its natural size.
    pub fn rounded(
        &self,
        texture: &WindowTexture,
        location: Point<f64, Physical>,
        size: Option<Size<i32, Logical>>,
        radius: f32,
        // False for windows with a title bar above them.
        round_top: bool,
        output_scale: f64,
        fresh_id: bool,
    ) -> RoundedTextureElement {
        // `src` is the part of the texture to draw: all of it. Without it a
        // smaller `size` would crop the texture instead of scaling it.
        let src = Rectangle::from_size((texture.physical_size.w as f64, texture.physical_size.h as f64).into());
        let inner = TextureRenderElement::from_texture_render_buffer(
            location,
            &texture.buffer,
            None,
            Some(src),
            Some(size.unwrap_or(texture.logical_size)),
            Kind::Unspecified,
        );
        // The shader works in screen pixels.
        let box_size = size.unwrap_or(texture.logical_size).to_f64().to_physical(output_scale);

        RoundedTextureElement {
            // The same texture may appear twice on screen (overview). Elements
            // must have distinct ids, so copies get a new one.
            id: if fresh_id { Id::new() } else { inner.id().clone() },
            texture: texture.texture.clone(),
            program: self.rounded_texture.clone(),
            uniforms: vec![
                Uniform::new("box_size", (box_size.w as f32, box_size.h as f32)),
                Uniform::new("radius", radius * output_scale as f32),
                Uniform::new("top_radius", if round_top { radius * output_scale as f32 } else { 0.0 }),
            ],
            inner,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shadow {
    Normal,
    /// The focused window floats "higher": a larger, darker shadow.
    Focused,
}

struct Backdrop {
    shadow: PixelShaderElement,
    outline: PixelShaderElement,
    rect: Rect,
    focused: bool,
    theme: Mood,
}

struct BackdropCache(RefCell<Option<Backdrop>>);
/// The uploaded wallpaper image and its size in pixels.
type WallpaperTexture = (TextureBuffer<GlesTexture>, Size<i32, Buffer>);
/// The drawn landscape, with the output size and mood it was made for.
type DrawnWallpaper = (PixelShaderElement, Size<i32, Logical>, Mood);
struct WallpaperCache(RefCell<Option<DrawnWallpaper>>);

fn rounded_rect_uniforms(rect: Rect, radius: f32, color: [f32; 4]) -> Vec<Uniform<'static>> {
    let size = (rect.size.w as f32, rect.size.h as f32);
    vec![
        Uniform::new("area_size", size),
        Uniform::new("box", (0.0, 0.0, size.0, size.1)),
        Uniform::new("radius", radius),
        Uniform::new("softness", 0.5f32),
        Uniform::new("color", color),
    ]
}

/// The area a shadow is drawn into: the window's rectangle plus a margin.
fn shadow_area(rect: Rect) -> Rect {
    Rect::new(
        (rect.loc.x - SHADOW_MARGIN, rect.loc.y - SHADOW_MARGIN).into(),
        (rect.size.w + 2 * SHADOW_MARGIN, rect.size.h + 2 * SHADOW_MARGIN).into(),
    )
}

fn shadow_uniforms(rect: Rect, radius: f32, strength: Shadow, theme: Theme) -> Vec<Uniform<'static>> {
    let shadow = theme.shadow();
    // (blur width, how far the shadow drops below the window, opacity factor)
    let (softness, drop, opacity) = match strength {
        Shadow::Normal => (22.0f32, 4.0f32, 1.0f32),
        Shadow::Focused => (40.0, 10.0, 1.8),
    };
    let margin = SHADOW_MARGIN as f32;
    let (width, height) = (rect.size.w as f32, rect.size.h as f32);

    vec![
        Uniform::new("area_size", (width + 2.0 * margin, height + 2.0 * margin)),
        Uniform::new("box", (margin, margin + drop, width, height)),
        Uniform::new("radius", radius),
        Uniform::new("softness", softness),
        Uniform::new("color", [shadow.r, shadow.g, shadow.b, (shadow.a * opacity).min(1.0)]),
    ]
}

/// A window drawn into an off-screen texture, kept in the window's user data.
pub struct WindowTexture {
    texture: GlesTexture,
    /// Wraps the texture and records which parts changed (for damage tracking).
    buffer: TextureRenderBuffer<GlesTexture>,
    /// Only redraws the parts of the window that changed since last frame.
    tracker: OutputDamageTracker,
    physical_size: Size<i32, Physical>,
    pub logical_size: Size<i32, Logical>,
    scale: f64,
    /// False until the texture has been drawn once.
    drawn: bool,
}

struct WindowTextureSlot(RefCell<Option<WindowTexture>>);

/// Bring `window`'s off-screen texture up to date and hand it to `f`.
///
/// Returns `None` if the window has no size yet or the GPU refuses.
pub fn with_window_texture<T>(
    renderer: &mut GlesRenderer,
    window: &Window,
    scale: f64,
    f: impl FnOnce(&WindowTexture) -> T,
) -> Option<T> {
    let geometry = window.geometry();
    let physical_size: Size<i32, Physical> = geometry.size.to_f64().to_physical(scale).to_i32_round();
    if physical_size.w <= 0 || physical_size.h <= 0 {
        return None;
    }

    let slot = window
        .user_data()
        .get_or_insert(|| WindowTextureSlot(RefCell::new(None)));
    let mut slot = slot.0.borrow_mut();

    // (Re)create the texture when the window appears or changes size.
    let stale = slot
        .as_ref()
        .is_none_or(|texture| texture.physical_size != physical_size || texture.scale != scale);
    if stale {
        let buffer_size: Size<i32, Buffer> = (physical_size.w, physical_size.h).into();
        let texture: GlesTexture = match renderer.create_buffer(Fourcc::Abgr8888, buffer_size) {
            Ok(texture) => texture,
            Err(err) => {
                tracing::warn!("could not create a window texture: {err}");
                return None;
            }
        };
        *slot = Some(WindowTexture {
            buffer: TextureRenderBuffer::from_texture(renderer, texture.clone(), 1, Transform::Normal, None),
            texture,
            tracker: OutputDamageTracker::new(physical_size, scale, Transform::Normal),
            physical_size,
            logical_size: geometry.size,
            scale,
            drawn: false,
        });
    }
    let window_texture = slot.as_mut()?;

    // The window's surfaces, positioned so that the visible part (the
    // "geometry", without client-drawn shadows) starts at the texture's origin.
    let toplevel = window.toplevel()?;
    let origin: Point<i32, Physical> = (Point::from((0, 0)) - geometry.loc)
        .to_f64()
        .to_physical(scale)
        .to_i32_round();
    let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = render_elements_from_surface_tree(
        renderer,
        toplevel.wl_surface(),
        origin,
        Scale::from(scale),
        1.0,
        Kind::Unspecified,
    );

    let WindowTexture {
        texture,
        buffer,
        tracker,
        drawn,
        ..
    } = window_texture;
    // Age 1 = "the texture still holds last frame": only changes are redrawn.
    let age = if *drawn { 1 } else { 0 };
    let full = Rectangle::from_size((physical_size.w, physical_size.h).into());

    let result = buffer.render().draw(|_| -> Result<Vec<Rectangle<i32, Buffer>>, String> {
        let mut framebuffer = renderer.bind(texture).map_err(|err| err.to_string())?;
        let rendered = tracker
            .render_output(renderer, &mut framebuffer, age, &elements, [0.0, 0.0, 0.0, 0.0])
            .map_err(|err| err.to_string())?;
        // Report what changed, so the screen is only redrawn when needed.
        let changed = rendered.damage.is_some_and(|damage| !damage.is_empty());
        Ok(if changed { vec![full] } else { Vec::new() })
    });

    match result {
        Ok(()) => {
            *drawn = true;
            Some(f(window_texture))
        }
        Err(err) => {
            tracing::warn!("could not draw a window off-screen: {err}");
            None
        }
    }
}

/// A custom render element: a texture drawn through the rounded-corner shader.
///
/// `Element` describes it to the damage tracker (where it is, whether it
/// changed); `RenderElement` actually draws it.
pub struct RoundedTextureElement {
    id: Id,
    inner: TextureRenderElement<GlesTexture>,
    texture: GlesTexture,
    program: GlesTexProgram,
    uniforms: Vec<Uniform<'static>>,
}

impl Element for RoundedTextureElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    // The corners are transparent, so we never claim to hide what is behind us.
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for RoundedTextureElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            self.transform(),
            self.alpha(),
            Some(&self.program),
            &self.uniforms,
        )
    }
}

/// Paint the landscape (sky, sun, three hills) into RGBA pixels on the CPU.
///
/// The same composition as the `WALLPAPER` shader. This takes tens of
/// milliseconds for a large screen, so it is meant for a worker thread;
/// the result is uploaded once and reused for every frame after.
pub fn paint_landscape(width: i32, height: i32, theme: &Theme) -> Vec<u8> {
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    // Center x, center y, radius x, radius y of each shape, and its color.
    let shapes = [
        (0.76 * w, 0.205 * h, 0.083 * h, 0.083 * h, theme.sun),
        (0.30 * w, 0.668 * h + 0.60 * h, 0.70 * w, 0.60 * h, theme.hills[0]),
        (0.83 * w, 0.757 * h + 0.55 * h, 0.62 * w, 0.55 * h, theme.hills[1]),
        (0.28 * w, 0.866 * h + 0.46 * h, 0.62 * w, 0.46 * h, theme.hills[2]),
    ];
    let mut pixels = Vec::with_capacity((width.max(1) * height.max(1) * 4) as usize);

    for y in 0..height.max(1) {
        for x in 0..width.max(1) {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let mut color = [theme.sky.r, theme.sky.g, theme.sky.b];
            for (cx, cy, rx, ry, shape) in shapes {
                // Distance to the ellipse edge in pixels (approximate),
                // turned into a coverage between 0 and 1 for a smooth edge.
                let (dx, dy) = ((px - cx) / rx, (py - cy) / ry);
                let distance = ((dx * dx + dy * dy).sqrt() - 1.0) * rx.min(ry);
                let coverage = (0.5 - distance / 1.5).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    color[0] += (shape.r - color[0]) * coverage;
                    color[1] += (shape.g - color[1]) * coverage;
                    color[2] += (shape.b - color[2]) * coverage;
                }
            }
            pixels.extend_from_slice(&[
                (color[0] * 255.0).round() as u8,
                (color[1] * 255.0).round() as u8,
                (color[2] * 255.0).round() as u8,
                255,
            ]);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landscape_has_sky_on_top_and_the_near_hill_at_the_bottom() {
        let theme = meridian_common::theme::Themes::builtin().morning;
        let (width, height) = (320, 200);
        let pixels = paint_landscape(width, height, &theme);
        assert_eq!(pixels.len(), (width * height * 4) as usize);

        let pixel = |x: i32, y: i32| {
            let i = ((y * width + x) * 4) as usize;
            [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
        };
        let rgba = |c: meridian_common::theme::Color| {
            [(c.r * 255.0).round() as u8, (c.g * 255.0).round() as u8, (c.b * 255.0).round() as u8, 255]
        };
        assert_eq!(pixel(2, 2), rgba(theme.sky));
        assert_eq!(pixel(width / 4, height - 2), rgba(theme.hills[2]));
        // The middle of the sun.
        assert_eq!(pixel((0.76 * width as f32) as i32, (0.205 * height as f32) as i32), rgba(theme.sun));
    }
}
