//! The mouse cursor image. On a real session nobody draws the cursor for us,
//! so we load one from the installed cursor themes (XCursor format).

use std::io::Read;

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::{Logical, Point, Transform},
};

/// We load a double-size image and tell the renderer it is "scale 2", so the
/// cursor is sharp on HiDPI screens and simply scaled down on normal ones.
const BUFFER_SCALE: i32 = 2;
const NOMINAL_SIZE: u32 = 24 * BUFFER_SCALE as u32;

pub struct Cursor {
    /// Pixels in CPU memory; the renderer uploads them to the GPU when needed.
    pub buffer: MemoryRenderBuffer,
    /// The pixel inside the image that is the actual pointer position (the arrow's tip).
    pub hotspot: Point<i32, Logical>,
}

impl Cursor {
    pub fn load() -> Self {
        let image = load_themed().unwrap_or_else(|| {
            tracing::warn!("no cursor theme found, using a built-in arrow");
            fallback_arrow()
        });

        let buffer = MemoryRenderBuffer::from_slice(
            &image.pixels_rgba,
            Fourcc::Abgr8888, // byte order R, G, B, A
            (image.width as i32, image.height as i32),
            BUFFER_SCALE,
            Transform::Normal,
            None,
        );
        let hotspot = Point::from((image.xhot as i32 / BUFFER_SCALE, image.yhot as i32 / BUFFER_SCALE));

        Self { buffer, hotspot }
    }
}

fn load_themed() -> Option<xcursor::parser::Image> {
    let theme_name = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into());
    let theme = xcursor::CursorTheme::load(&theme_name);
    let path = theme.load_icon("default").or_else(|| theme.load_icon("left_ptr"))?;

    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.read_to_end(&mut bytes).ok()?;
    let images = xcursor::parser::parse_xcursor(&bytes)?;

    // A cursor file holds several sizes; take the one closest to what we want.
    images
        .into_iter()
        .min_by_key(|image| (image.size as i64 - NOMINAL_SIZE as i64).abs())
}

/// A plain dark triangle with a light outline, drawn pixel by pixel.
fn fallback_arrow() -> xcursor::parser::Image {
    const SIZE: u32 = 32;
    let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let inside = x <= y && x + y / 2 < SIZE - 6;
            let edge = inside && (x <= 1 || x + 1 >= y || x + y / 2 + 2 >= SIZE - 6);
            let rgba: [u8; 4] = match (inside, edge) {
                (true, true) => [255, 255, 255, 255],
                (true, false) => [20, 24, 32, 255],
                _ => [0, 0, 0, 0],
            };
            pixels.extend_from_slice(&rgba);
        }
    }
    xcursor::parser::Image {
        size: SIZE,
        width: SIZE,
        height: SIZE,
        xhot: 0,
        yhot: 0,
        delay: 0,
        pixels_rgba: pixels,
        pixels_argb: Vec::new(),
    }
}
