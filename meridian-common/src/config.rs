//! User configuration, read from `~/.config/meridian/config.toml`.
//!
//! `serde` generates the parsing code: `#[derive(Deserialize)]` on a struct
//! means "a TOML table with these keys can be turned into this struct".

use std::{collections::HashMap, fmt, path::PathBuf};

use serde::Deserialize;

/// The built-in defaults, also installed as an example file.
pub const DEFAULT_CONFIG: &str = include_str!("../resources/default-config.toml");

#[derive(Debug, Clone, PartialEq, Deserialize)]
// `default`: missing keys take their value from `Config::default()`.
// `deny_unknown_fields`: typos are reported instead of silently ignored.
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    pub layout: LayoutKind,
    pub terminal: String,
    pub keyboard: Keyboard,
    pub output: Output,
    pub tiled: Tiled,
    /// Key combination (e.g. "Mod+Return") to action.
    pub keybindings: HashMap<String, Action>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutKind {
    Floating,
    Tiled,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Keyboard {
    pub layout: String,
    pub variant: String,
    pub options: String,
    pub repeat_delay: i32,
    pub repeat_rate: i32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Output {
    /// 0 means "choose automatically".
    pub scale: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Tiled {
    pub gap: i32,
    pub master_ratio: f64,
}

/// What a keybinding does.
///
/// In TOML a plain variant is a string (`"close-window"`) and a variant with
/// data is a small table (`{ spawn = "firefox" }`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    SpawnTerminal,
    CloseWindow,
    Quit,
    ToggleLayout,
    FocusNext,
    FocusPrevious,
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
            Ok(text) => toml::from_str(&text).map_err(|err| ConfigError::Parse(path, err)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(ConfigError::Read(path, err)),
        }
    }
}

/// The values of the embedded default file, parsed once per call.
struct Defaults {
    layout: LayoutKind,
    terminal: String,
    keyboard: Keyboard,
    output: Output,
    tiled: Tiled,
    keybindings: HashMap<String, Action>,
}

// The `Default` impls below all read from DEFAULT_CONFIG, so the TOML file is
// the single place where defaults are written down. This mirror struct has no
// `#[serde(default)]`, which avoids `Config::default()` calling itself.
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RawDefaults {
    layout: LayoutKind,
    terminal: String,
    keyboard: RawKeyboard,
    output: RawOutput,
    tiled: RawTiled,
    keybindings: HashMap<String, Action>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RawKeyboard {
    layout: String,
    variant: String,
    options: String,
    repeat_delay: i32,
    repeat_rate: i32,
}

#[derive(Deserialize)]
struct RawOutput {
    scale: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RawTiled {
    gap: i32,
    master_ratio: f64,
}

fn defaults() -> Defaults {
    // A test below guarantees the embedded file parses, so this cannot fail at runtime.
    let raw: RawDefaults = toml::from_str(DEFAULT_CONFIG).expect("built-in default config is valid");
    Defaults {
        layout: raw.layout,
        terminal: raw.terminal,
        keyboard: Keyboard {
            layout: raw.keyboard.layout,
            variant: raw.keyboard.variant,
            options: raw.keyboard.options,
            repeat_delay: raw.keyboard.repeat_delay,
            repeat_rate: raw.keyboard.repeat_rate,
        },
        output: Output { scale: raw.output.scale },
        tiled: Tiled {
            gap: raw.tiled.gap,
            master_ratio: raw.tiled.master_ratio,
        },
        keybindings: raw.keybindings,
    }
}

impl Default for Config {
    fn default() -> Self {
        let d = defaults();
        Self {
            layout: d.layout,
            terminal: d.terminal,
            keyboard: d.keyboard,
            output: d.output,
            tiled: d.tiled,
            keybindings: d.keybindings,
        }
    }
}

impl Default for Keyboard {
    fn default() -> Self {
        defaults().keyboard
    }
}

impl Default for Output {
    fn default() -> Self {
        defaults().output
    }
}

impl Default for Tiled {
    fn default() -> Self {
        defaults().tiled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_parses_as_user_config() {
        let config: Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.keybindings["Mod+Return"], Action::SpawnTerminal);
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let config: Config = toml::from_str("layout = \"tiled\"\n[tiled]\ngap = 4\n").unwrap();
        assert_eq!(config.layout, LayoutKind::Tiled);
        assert_eq!(config.tiled.gap, 4);
        assert_eq!(config.tiled.master_ratio, Config::default().tiled.master_ratio);
        assert!(!config.keybindings.is_empty());
    }

    #[test]
    fn spawn_action_takes_a_command() {
        let config: Config = toml::from_str("[keybindings]\n\"Mod+B\" = { spawn = \"firefox --new-window\" }\n").unwrap();
        assert_eq!(config.keybindings["Mod+B"], Action::Spawn("firefox --new-window".into()));
        assert_eq!(config.keybindings.len(), 1);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<Config>("layuot = \"tiled\"").is_err());
        assert!(toml::from_str::<Config>("[keybindings]\n\"Mod+X\" = \"explode\"").is_err());
    }
}
