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
    /// Desktop file ids without ".desktop", e.g. "org.gnome.Nautilus".
    pub pinned: Vec<String>,
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
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("layuot = \"tiled\"").is_err());
        assert!(Config::parse("[keybindings]\n\"Mod+X\" = \"explode\"").is_err());
        assert!(Config::parse("[tiled]\ngapp = 3").is_err());
    }
}
