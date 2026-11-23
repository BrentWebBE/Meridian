//! User configuration, read from `~/.config/meridian/config.toml`.
//!
//! `serde` generates the parsing code: `#[derive(Deserialize)]` on a struct
//! means "a TOML table with these keys can be turned into this struct".

use std::{collections::HashMap, fmt, path::PathBuf};

use serde::{Deserialize, Serialize};

/// The built-in defaults, also installed as an example file.
pub const DEFAULT_CONFIG: &str = include_str!("../resources/default-config.toml");

#[derive(Debug, Clone, PartialEq, Deserialize)]
// `deny_unknown_fields`: typos are reported instead of silently ignored.
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    pub layout: LayoutKind,
    pub terminal: String,
    /// Programs started together with the compositor (bar, dock, ...).
    pub autostart: Vec<String>,
    pub appearance: Appearance,
    pub keyboard: Keyboard,
    pub output: Output,
    pub tiled: Tiled,
    pub dock: Dock,
    /// Key combination (e.g. "Mod+Return") to action.
    pub keybindings: HashMap<String, Action>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutKind {
    Floating,
    Tiled,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Appearance {
    /// The pinned mood: "morning", "dusk" or "night".
    pub theme: String,
    /// Ignore `theme` and pick the mood by the clock instead.
    pub follow_time_of_day: bool,
    /// Path to an image to use instead of the drawn landscape. Empty = none.
    pub wallpaper: String,
}

impl Appearance {
    /// The mood to use right now, given the current hour (0-23).
    pub fn current_mood(&self, hour: u32) -> crate::theme::Mood {
        if self.follow_time_of_day {
            crate::theme::Mood::for_hour(hour)
        } else {
            crate::theme::Mood::from_name(&self.theme)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Keyboard {
    pub layout: String,
    pub variant: String,
    pub options: String,
    pub repeat_delay: i32,
    pub repeat_rate: i32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Output {
    /// 0 means "choose automatically".
    pub scale: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Tiled {
    pub gap: i32,
    pub master_ratio: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Dock {
    pub mode: DockMode,
    pub position: DockPosition,
    /// Show a dot under apps that are running.
    pub running_dots: bool,
    /// Desktop file ids without ".desktop", e.g. "org.gnome.Nautilus".
    pub pinned: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DockMode {
    /// Always visible.
    Dock,
    /// Hidden until the pointer touches the screen edge.
    AutoHide,
    /// No dock; launch with Find (Super).
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DockPosition {
    Bottom,
    Left,
    Right,
}

/// What a keybinding does.
///
/// In TOML a plain variant is a string (`"close-window"`) and a variant with
/// data is a small table (`{ spawn = "firefox" }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    SpawnTerminal,
    CloseWindow,
    Quit,
    ToggleLayout,
    FocusNext,
    FocusPrevious,
    /// Save a picture of the screen to ~/Pictures/Screenshots.
    Screenshot,
    /// Show all windows as thumbnails, with the spaces strip and search.
    ToggleOverview,
    /// Switch to a space (virtual desktop), counted from 1.
    Workspace(u8),
    /// Send the focused window to a space, counted from 1.
    MoveToWorkspace(u8),
    /// Run a command line (split on whitespace, no shell).
    Spawn(String),
}

#[derive(Debug)]
pub enum ConfigError {
    Read(PathBuf, std::io::Error),
    Parse(PathBuf, toml::de::Error),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(path, err) => write!(f, "cannot read {}: {err}", path.display()),
            Self::Parse(path, err) => write!(f, "error in {}:\n{err}", path.display()),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// `$XDG_CONFIG_HOME/meridian/config.toml`, usually under `~/.config`.
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(base.join("meridian").join("config.toml"))
    }

    /// Load the user's config. A missing file is not an error: you get the defaults.
    pub fn load() -> Result<Self, ConfigError> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map_err(|err| ConfigError::Parse(path, err)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(ConfigError::Read(path, err)),
        }
    }

    /// Parse a user config, filling everything it leaves out from the defaults.
    ///
    /// Both files are first read as generic TOML tables and merged; only the
    /// merged result is turned into a `Config`. That keeps the default file
    /// the single place where default values are written down.
    pub fn parse(user: &str) -> Result<Self, toml::de::Error> {
        let mut merged: toml::Table =
            toml::from_str(DEFAULT_CONFIG).expect("built-in default config is valid TOML");
        let user: toml::Table = toml::from_str(user)?;

        for (key, value) in user {
            match (merged.get_mut(&key), value) {
                // A section like [tiled]: override only the keys the user wrote.
                (Some(toml::Value::Table(defaults)), toml::Value::Table(overrides)) if key != "keybindings" => {
                    defaults.extend(overrides);
                }
                // Plain values, and [keybindings] as a whole, replace the default.
                (_, value) => {
                    merged.insert(key, value);
                }
            }
        }

        merged.try_into()
    }
}

/// Change one setting in the user's config file, keeping everything else
/// (including comments) as it is. `section` is a table name such as "dock",
/// or `None` for a top-level key. Used by the Settings app.
pub fn write_setting(section: Option<&str>, key: &str, value: impl Into<toml_edit::Value>) -> Result<(), ConfigError> {
    let path = Config::path().ok_or_else(|| {
        ConfigError::Read(PathBuf::from("~/.config"), std::io::Error::other("no home directory"))
    })?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(ConfigError::Read(path, err)),
    };

    // `toml_edit` parses into a document that remembers formatting.
    let mut document: toml_edit::DocumentMut = text.parse().map_err(|err: toml_edit::TomlError| {
        ConfigError::Read(path.clone(), std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string()))
    })?;
    let item = toml_edit::value(value);
    match section {
        Some(section) => {
            // Create the [section] if the file does not have it yet.
            if document.get(section).is_none() {
                document[section] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            document[section][key] = item;
        }
        None => document[key] = item,
    }

    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Write to a temporary file first, then rename: a crash can never
        // leave a half-written config behind.
        let temporary = path.with_extension("toml.tmp");
        std::fs::write(&temporary, document.to_string())?;
        std::fs::rename(&temporary, &path)
    };
    write().map_err(|err| ConfigError::Read(path.clone(), err))
}

impl Default for Config {
    fn default() -> Self {
        // A test below guarantees the embedded file parses.
        Self::parse("").expect("built-in default config is valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_parses_as_user_config() {
        let config = Config::parse(DEFAULT_CONFIG).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.keybindings["Mod+Return"], Action::SpawnTerminal);
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let config = Config::parse("layout = \"tiled\"\n[tiled]\ngap = 4\n").unwrap();
        assert_eq!(config.layout, LayoutKind::Tiled);
        assert_eq!(config.tiled.gap, 4);
        assert_eq!(config.tiled.master_ratio, Config::default().tiled.master_ratio);
        assert!(!config.keybindings.is_empty());
    }

    #[test]
    fn spawn_action_takes_a_command() {
        let config = Config::parse("[keybindings]\n\"Mod+B\" = { spawn = \"firefox --new-window\" }\n").unwrap();
        assert_eq!(config.keybindings["Mod+B"], Action::Spawn("firefox --new-window".into()));
        assert_eq!(config.keybindings.len(), 1);
    }

    #[test]
    fn appearance_follows_the_clock_only_when_asked() {
        let fixed = Config::parse("[appearance]\ntheme = \"night\"\nfollow-time-of-day = false\n").unwrap();
        assert_eq!(fixed.appearance.current_mood(10).name(), "night");
        let auto = Config::parse("[appearance]\nfollow-time-of-day = true\n").unwrap();
        assert_eq!(auto.appearance.current_mood(10).name(), "morning");
        assert_eq!(auto.appearance.current_mood(18).name(), "dusk");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("layuot = \"tiled\"").is_err());
        assert!(Config::parse("[keybindings]\n\"Mod+X\" = \"explode\"").is_err());
        assert!(Config::parse("[tiled]\ngapp = 3").is_err());
    }
}
