//! Theme tokens: named colors and sizes that all Meridian programs agree on.
//!
//! The compositor reads them as Rust values. The GTK programs (bar, dock)
//! get them as CSS variables through [`Theme::css_variables`].

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

    /// The same color with a different opacity.
    pub const fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    /// The `[r, g, b, a]` array form that renderers expect.
    pub const fn to_array(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// CSS notation, e.g. `rgba(47, 143, 255, 1)`.
    pub fn css(self) -> String {
        let byte = |channel: f32| (channel * 255.0).round() as u8;
        format!("rgba({}, {}, {}, {})", byte(self.r), byte(self.g), byte(self.b), self.a)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub name: &'static str,
    /// Shown behind all windows until a wallpaper exists.
    pub desktop_background: Color,
    /// Background of panels: bar, dock, menus.
    pub surface: Color,
    /// Background of tiles and buttons that sit on a surface.
    pub surface_raised: Color,
    /// Hover highlight on a surface.
    pub surface_hover: Color,
    pub text: Color,
    pub text_muted: Color,
    /// The one strong color: active toggles, the menu button, focus.
    pub accent: Color,
    pub on_accent: Color,
    pub border: Color,
    pub shadow: Color,
}

pub const LIGHT: Theme = Theme {
    name: "light",
    desktop_background: Color::from_hex(0xd4e3f8),
    surface: Color::from_hex(0xf7f9fd).with_alpha(0.92),
    surface_raised: Color::from_hex(0xffffff),
    surface_hover: Color::from_hex(0xe6edf7),
    text: Color::from_hex(0x1a2433),
    text_muted: Color::from_hex(0x5b6677),
    accent: Color::from_hex(0x2f8fff),
    on_accent: Color::from_hex(0xffffff),
    border: Color::from_hex(0x1a2433).with_alpha(0.08),
    shadow: Color::from_hex(0x1f3a66).with_alpha(0.18),
};

pub const DARK: Theme = Theme {
    name: "dark",
    desktop_background: Color::from_hex(0x1b1e24),
    surface: Color::from_hex(0x23272f).with_alpha(0.92),
    surface_raised: Color::from_hex(0x2e333d),
    surface_hover: Color::from_hex(0x3a404c),
    text: Color::from_hex(0xeef1f6),
    text_muted: Color::from_hex(0x9aa4b4),
    accent: Color::from_hex(0x4a9dff),
    on_accent: Color::from_hex(0xffffff),
    border: Color::from_hex(0xffffff).with_alpha(0.08),
    shadow: Color::from_hex(0x000000).with_alpha(0.45),
};

/// Sizes in logical pixels.
pub const BAR_HEIGHT: i32 = 28;
pub const DOCK_ICON_SIZE: i32 = 32;
pub const DOCK_TILE_SIZE: i32 = 48;
pub const WINDOW_RADIUS: i32 = 12;

impl Theme {
    /// The tokens as CSS custom properties, to prepend to an app's stylesheet.
    /// Stylesheets then write `color: var(--m-text);`.
    pub fn css_variables(&self) -> String {
        let colors = [
            ("surface", self.surface),
            ("surface-raised", self.surface_raised),
            ("surface-hover", self.surface_hover),
            ("text", self.text),
            ("text-muted", self.text_muted),
            ("accent", self.accent),
            ("on-accent", self.on_accent),
            ("border", self.border),
            ("shadow", self.shadow),
        ];

        let mut css = String::from(":root {\n");
        for (name, color) in colors {
            css.push_str(&format!("  --m-{name}: {};\n", color.css()));
        }
        css.push_str(&format!("  --m-radius: {WINDOW_RADIUS}px;\n}}\n"));
        css
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_css_round_trip() {
        let color = Color::from_hex(0x2f8fff);
        assert_eq!(color.css(), "rgba(47, 143, 255, 1)");
        assert_eq!(color.with_alpha(0.5).css(), "rgba(47, 143, 255, 0.5)");
    }

    #[test]
    fn css_variables_cover_the_tokens() {
        let css = LIGHT.css_variables();
        assert!(css.contains("--m-accent: rgba(47, 143, 255, 1);"));
        assert!(css.contains("--m-radius: 12px;"));
    }
}
