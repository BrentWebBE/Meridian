//! Theme tokens: named colors that all Meridian programs agree on.

/// An RGBA color with each channel in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    /// Build an opaque color from a `0xRRGGBB` hex value.
    ///
    /// `const fn` means this can run at compile time, so themes can be constants.
    pub const fn from_hex(hex: u32) -> Self {
        Self {
            r: ((hex >> 16) & 0xff) as f32 / 255.0,
            g: ((hex >> 8) & 0xff) as f32 / 255.0,
            b: (hex & 0xff) as f32 / 255.0,
            a: 1.0,
        }
    }

    /// The `[r, g, b, a]` array form that renderers expect.
    pub const fn to_array(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub name: &'static str,
    /// Shown behind all windows until a wallpaper exists.
    pub desktop_background: Color,
}

pub const LIGHT: Theme = Theme {
    name: "light",
    desktop_background: Color::from_hex(0xdfe5ee),
};

pub const DARK: Theme = Theme {
    name: "dark",
    desktop_background: Color::from_hex(0x1b1e24),
};
