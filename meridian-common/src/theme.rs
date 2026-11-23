//! Theme tokens: the named colors and sizes that all Meridian programs
//! agree on.
//!
//! Terra has three moods (Morning, Dusk, Night). Their colors live in a TOML
//! file. The compositor reads tokens as Rust values; the GTK programs get
//! them as CSS variables through [`Theme::css_variables`].

use std::{fmt, path::PathBuf};

use serde::Deserialize;

/// The built-in moods; also the template for a user's own themes file.
pub const DEFAULT_THEMES: &str = include_str!("../resources/themes.toml");

/// The name shown to people: on the logo button and in Settings.
pub const BRAND: &str = "Terra";

// Sizes in logical pixels.
pub const BAR_HEIGHT: i32 = 34;
pub const DOCK_ICON_SIZE: i32 = 32;
pub const WINDOW_RADIUS: i32 = 18;
pub const PANEL_RADIUS: i32 = 26;

/// How see-through panels (bar, dock) are, and how strong their border is.
const PANEL_OPACITY: f32 = 0.92;
const BORDER_OPACITY: f32 = 0.14;

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
    pub const fn from_hex(hex: u32) -> Self {
        Self {
            r: ((hex >> 16) & 0xff) as f32 / 255.0,
            g: ((hex >> 8) & 0xff) as f32 / 255.0,
            b: (hex & 0xff) as f32 / 255.0,
            a: 1.0,
        }
    }

    /// Parse `"#rrggbb"` as written in the themes file.
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('#')?;
        if digits.len() != 6 {
            return None;
        }
        u32::from_str_radix(digits, 16).ok().map(Self::from_hex)
    }

    /// The same color with a different opacity.
    pub const fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    /// The `[r, g, b, a]` array form that renderers expect.
    pub const fn to_array(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// CSS notation, e.g. `rgba(184, 90, 50, 1)`.
    pub fn css(self) -> String {
        let byte = |channel: f32| (channel * 255.0).round() as u8;
        format!("rgba({}, {}, {}, {})", byte(self.r), byte(self.g), byte(self.b), self.a)
    }

    /// How bright the color looks, 0 (black) to 1 (white), as defined by WCAG.
    fn luminance(self) -> f32 {
        let linear = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }

    /// WCAG contrast ratio between two opaque colors, from 1 to 21.
    /// Body text needs at least 4.5.
    pub fn contrast(self, other: Color) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
}

// Lets serde read a color straight from a "#rrggbb" string in TOML.
impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Color::parse(&text).ok_or_else(|| serde::de::Error::custom(format!("\"{text}\" is not a #rrggbb color")))
    }
}

/// Which of the three moods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    Morning,
    Dusk,
    Night,
}

impl Mood {
    pub const ALL: [Mood; 3] = [Mood::Morning, Mood::Dusk, Mood::Night];

    /// The name used in config files and CSS.
    pub fn name(self) -> &'static str {
        match self {
            Mood::Morning => "morning",
            Mood::Dusk => "dusk",
            Mood::Night => "night",
        }
    }

    /// Unknown names give Morning.
    pub fn from_name(name: &str) -> Mood {
        match name {
            "dusk" => Mood::Dusk,
            "night" => Mood::Night,
            _ => Mood::Morning,
        }
    }

    /// The mood that fits an hour of the day (0-23):
    /// morning from 6:00, dusk from 17:00, night from 21:00.
    pub fn for_hour(hour: u32) -> Mood {
        match hour {
            6..=16 => Mood::Morning,
            17..=20 => Mood::Dusk,
            _ => Mood::Night,
        }
    }
}

/// One mood's tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub mood: Mood,
    // Wallpaper: a sky, a sun and three hills.
    pub sky: Color,
    pub sun: Color,
    /// Far, middle, near.
    pub hills: [Color; 3],
    /// Background of windows, cards and panels.
    pub surface: Color,
    /// Title bars, hover and selection.
    pub surface2: Color,
    /// Text.
    pub ink: Color,
    /// Secondary text.
    pub muted: Color,
    /// The one strong color: logo, active states, focus.
    pub accent: Color,
    /// Text on top of the accent color.
    pub on_accent: Color,
}

impl Theme {
    /// Panels are the surface color, slightly see-through.
    pub fn panel(&self) -> Color {
        self.surface.with_alpha(PANEL_OPACITY)
    }

    /// Hairline borders: the ink color, faint.
    pub fn border(&self) -> Color {
        self.ink.with_alpha(BORDER_OPACITY)
    }

    /// Shadows are darker on dark moods, where they are harder to see.
    pub fn shadow(&self) -> Color {
        let dark_mood = self.surface.luminance() < 0.2;
        Color::from_hex(0x1a1410).with_alpha(if dark_mood { 0.50 } else { 0.22 })
    }

    /// The tokens as CSS custom properties, to prepend to an app's stylesheet.
    /// Stylesheets then write `color: var(--m-text);`.
    pub fn css_variables(&self) -> String {
        let colors = [
            ("surface", self.panel()),
            ("surface-raised", self.surface),
            ("surface-hover", self.surface2),
            ("text", self.ink),
            ("text-muted", self.muted),
            ("accent", self.accent),
            ("on-accent", self.on_accent),
            ("border", self.border()),
            ("shadow", self.shadow()),
        ];

        let mut css = String::from(":root {\n");
        for (name, color) in colors {
            css.push_str(&format!("  --m-{name}: {};\n", color.css()));
        }
        css.push_str(&format!("  --m-radius-window: {WINDOW_RADIUS}px;\n"));
        css.push_str(&format!("  --m-radius-panel: {PANEL_RADIUS}px;\n"));
        // The named fonts are used when installed; otherwise the system's.
        css.push_str("  --m-font-ui: \"DM Sans\", system-ui, sans-serif;\n");
        css.push_str("  --m-font-display: \"Fraunces\", serif;\n}\n");
        css
    }
}

/// All three moods.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Themes {
    pub morning: Theme,
    pub dusk: Theme,
    pub night: Theme,
}

/// A mood as written in the TOML file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct ThemeFile {
    sky: Color,
    sun: Color,
    hills: [Color; 3],
    surface: Color,
    surface2: Color,
    ink: Color,
    muted: Color,
    accent: Color,
    on_accent: Color,
}

#[derive(Debug)]
pub struct ThemeError(String);

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ThemeError {}

impl Themes {
    pub fn get(&self, mood: Mood) -> Theme {
        match mood {
            Mood::Morning => self.morning,
            Mood::Dusk => self.dusk,
            Mood::Night => self.night,
        }
    }

    /// The moods that ship with Terra.
    pub fn builtin() -> Self {
        // A test below guarantees the embedded file parses.
        Self::parse("").expect("built-in themes are valid")
    }

    /// `~/.config/meridian/themes.toml`, if there is a config directory.
    pub fn path() -> Option<PathBuf> {
        crate::config::Config::path().map(|config| config.with_file_name("themes.toml"))
    }

    /// The built-in moods with the user's changes applied. A missing file is
    /// fine; a broken one is reported and ignored.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::builtin();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).unwrap_or_else(|err| {
                eprintln!("{}: {err}; using the built-in themes", path.display());
                Self::builtin()
            }),
            Err(_) => Self::builtin(),
        }
    }

    /// Parse a user's themes file on top of the built-in one: any section or
    /// key left out keeps its built-in value.
    pub fn parse(user: &str) -> Result<Self, ThemeError> {
        let mut merged: toml::Table = toml::from_str(DEFAULT_THEMES).expect("built-in themes are valid TOML");
        let user: toml::Table = toml::from_str(user).map_err(|err| ThemeError(err.to_string()))?;

        for (mood, overrides) in user {
            match (merged.get_mut(&mood), overrides) {
                (Some(toml::Value::Table(base)), toml::Value::Table(overrides)) => base.extend(overrides),
                _ => return Err(ThemeError(format!("unknown section [{mood}]"))),
            }
        }

        let read = |mood: Mood| -> Result<Theme, ThemeError> {
            let table = merged.get(mood.name()).cloned().unwrap_or(toml::Value::Table(Default::default()));
            let file: ThemeFile = table
                .try_into()
                .map_err(|err| ThemeError(format!("[{}]: {err}", mood.name())))?;
            Ok(Theme {
                mood,
                sky: file.sky,
                sun: file.sun,
                hills: file.hills,
                surface: file.surface,
                surface2: file.surface2,
                ink: file.ink,
                muted: file.muted,
                accent: file.accent,
                on_accent: file.on_accent,
            })
        };

        Ok(Self {
            morning: read(Mood::Morning)?,
            dusk: read(Mood::Dusk)?,
            night: read(Mood::Night)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_css_round_trip() {
        let color = Color::parse("#b85a32").unwrap();
        assert_eq!(color.css(), "rgba(184, 90, 50, 1)");
        assert_eq!(color.with_alpha(0.5).css(), "rgba(184, 90, 50, 0.5)");
        assert!(Color::parse("b85a32").is_none());
        assert!(Color::parse("#b85a3").is_none());
    }

    #[test]
    fn moods_follow_the_day() {
        assert_eq!(Mood::for_hour(5), Mood::Night);
        assert_eq!(Mood::for_hour(6), Mood::Morning);
        assert_eq!(Mood::for_hour(16), Mood::Morning);
        assert_eq!(Mood::for_hour(17), Mood::Dusk);
        assert_eq!(Mood::for_hour(20), Mood::Dusk);
        assert_eq!(Mood::for_hour(21), Mood::Night);
        assert_eq!(Mood::from_name("dusk"), Mood::Dusk);
        assert_eq!(Mood::from_name("nonsense"), Mood::Morning);
    }

    #[test]
    fn builtin_themes_match_the_design() {
        let themes = Themes::builtin();
        assert_eq!(themes.morning.accent, Color::from_hex(0xb85a32));
        assert_eq!(themes.dusk.surface, Color::from_hex(0x2d2535));
        assert_eq!(themes.night.hills[2], Color::from_hex(0x2d4339));
        assert!(themes.morning.css_variables().contains("--m-accent: rgba(184, 90, 50, 1);"));
    }

    #[test]
    fn user_file_overrides_single_tokens() {
        let themes = Themes::parse("[night]\naccent = \"#ff0000\"\n").unwrap();
        assert_eq!(themes.night.accent, Color::from_hex(0xff0000));
        assert_eq!(themes.night.ink, Themes::builtin().night.ink);
        assert_eq!(themes.morning, Themes::builtin().morning);
        assert!(Themes::parse("[noon]\naccent = \"#ff0000\"\n").is_err());
        assert!(Themes::parse("[night]\naccent = \"red\"\n").is_err());
    }

    /// Text must stay readable in every mood: WCAG AA asks for 4.5:1.
    #[test]
    fn text_contrast_is_at_least_4_5_in_every_mood() {
        let themes = Themes::builtin();
        for mood in Mood::ALL {
            let theme = themes.get(mood);
            let pairs = [
                ("ink on surface", theme.ink, theme.surface),
                ("ink on surface2", theme.ink, theme.surface2),
                ("muted on surface", theme.muted, theme.surface),
                ("muted on surface2", theme.muted, theme.surface2),
                ("on-accent on accent", theme.on_accent, theme.accent),
            ];
            for (what, text, background) in pairs {
                let ratio = text.contrast(background);
                assert!(ratio >= 4.5, "{}: {what} is only {ratio:.2}:1", mood.name());
            }
        }
    }
}
