//! The list of installed applications and how search matches them.
//!
//! Applications announce themselves with `.desktop` files in
//! `/usr/share/applications` and `~/.local/share/applications`. GIO (part of
//! GLib) parses them, including translations and the "hidden" flags.

use gtk::{gio, prelude::*};

pub struct AppEntry {
    /// Lowercase desktop file id without ".desktop".
    pub id: String,
    pub name: String,
    /// Lowercase text that search looks through besides the name.
    haystack: String,
    pub info: gio::AppInfo,
}

impl AppEntry {
    /// Description, command and id: what the app can be found by besides its name.
    pub fn keywords(&self) -> &str {
        &self.haystack
    }
}

/// All applications that want to be shown in menus, sorted by name.
pub fn installed() -> Vec<AppEntry> {
    let mut entries: Vec<AppEntry> = gio::AppInfo::all()
        .into_iter()
        .filter(|info| info.should_show())
        .filter_map(|info| {
            let id = info.id()?.to_lowercase();
            let id = id.strip_suffix(".desktop")?.to_string();
            let name = info.name().to_string();
            let haystack = format!(
                "{} {} {}",
                info.description().unwrap_or_default(),
                info.executable().unwrap_or_default().to_string_lossy(),
                id
            )
            .to_lowercase();
            Some(AppEntry { id, name, haystack, info })
        })
        .collect();

    entries.sort_by_key(|entry| entry.name.to_lowercase());
    entries
}
