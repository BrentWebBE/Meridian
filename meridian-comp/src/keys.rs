//! Keybindings: turn config strings like "Mod+Shift+Return" into something
//! we can compare against key presses.

use std::collections::HashMap;

use meridian_common::config::Action;
use smithay::input::keyboard::{Keysym, ModifiersState, xkb};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool, // the Super / Windows key
}

impl From<&ModifiersState> for Modifiers {
    fn from(state: &ModifiersState) -> Self {
        Self {
            ctrl: state.ctrl,
            alt: state.alt,
            shift: state.shift,
            logo: state.logo,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub modifiers: Modifiers,
    /// The key without modifiers applied, e.g. `q` even for "Shift+Q".
    pub keysym: Keysym,
    pub action: Action,
}

/// Parse all bindings from the config. Invalid entries are logged and skipped.
/// `mod_is_logo`: what "Mod" stands for (Super on a real session, Alt nested).
pub fn parse_bindings(config: &HashMap<String, Action>, mod_is_logo: bool) -> Vec<Binding> {
    let mut bindings = Vec::new();
    for (combo, action) in config {
        match parse_combo(combo, mod_is_logo) {
            Ok((modifiers, keysym)) => bindings.push(Binding {
                modifiers,
                keysym,
                action: action.clone(),
            }),
            Err(reason) => tracing::warn!("ignoring keybinding \"{combo}\": {reason}"),
        }
    }
    bindings
}

fn parse_combo(combo: &str, mod_is_logo: bool) -> Result<(Modifiers, Keysym), String> {
    let mut modifiers = Modifiers::default();
    let mut key = None;

    for part in combo.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "mod" if mod_is_logo => modifiers.logo = true,
            "mod" => modifiers.alt = true,
            "super" | "logo" | "win" => modifiers.logo = true,
            "alt" => modifiers.alt = true,
            "ctrl" | "control" => modifiers.ctrl = true,
            "shift" => modifiers.shift = true,
            "" => return Err("empty part between '+' signs".into()),
            _ if key.is_some() => return Err("more than one non-modifier key".into()),
            _ => key = Some(part),
        }
    }

    let name = key.ok_or("no key, only modifiers")?;
    // Case-insensitive lookup returns the lowercase keysym for letters, which
    // is what a key produces before Shift is applied.
    let keysym = xkb::keysym_from_name(name, xkb::KEYSYM_CASE_INSENSITIVE);
    if keysym == Keysym::NoSymbol {
        return Err(format!("unknown key name \"{name}\""));
    }
    Ok((modifiers, keysym))
}

/// The action bound to this key press, if any.
pub fn find_action(bindings: &[Binding], modifiers: &ModifiersState, raw_syms: &[Keysym]) -> Option<Action> {
    let held = Modifiers::from(modifiers);
    bindings
        .iter()
        .find(|b| b.modifiers == held && raw_syms.contains(&b.keysym))
        .map(|b| b.action.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mod_depends_on_backend() {
        let (nested, _) = parse_combo("Mod+Return", false).unwrap();
        assert!(nested.alt && !nested.logo);
        let (session, sym) = parse_combo("Mod+Return", true).unwrap();
        assert!(session.logo && !session.alt);
        assert_eq!(sym, Keysym::Return);
    }

    #[test]
    fn letters_are_case_insensitive() {
        let (mods, sym) = parse_combo("mod+SHIFT+E", true).unwrap();
        assert!(mods.shift && mods.logo);
        assert_eq!(sym, Keysym::e);
    }

    #[test]
    fn bad_combos_are_rejected() {
        assert!(parse_combo("Mod+Shift", true).is_err());
        assert!(parse_combo("Mod+A+B", true).is_err());
        assert!(parse_combo("Mod+NotAKey", true).is_err());
        assert!(parse_combo("Mod++A", true).is_err());
    }
}
