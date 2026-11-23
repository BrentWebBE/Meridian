//! The list of installed applications and how search matches them.
//!
//! Applications announce themselves with `.desktop` files in
//! `/usr/share/applications` and `~/.local/share/applications`. GIO (part of
//! GLib) parses them, including translations and the "hidden" flags.

use gtk::{gio, glib, prelude::*};

pub struct AppEntry {
    /// Lowercase desktop file id without ".desktop".
    pub id: String,
    pub name: String,
    /// Lowercase text that search looks through: name, description, command.
    haystack: String,
    pub info: gio::AppInfo,
}

impl AppEntry {
    /// True if every word of the query occurs somewhere in the app's text.
    /// An empty query matches everything.
    pub fn matches(&self, query: &str) -> bool {
        matches(&self.haystack, query)
    }
}

fn matches(haystack: &str, query: &str) -> bool {
    query
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
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
                "{} {} {} {}",
                name,
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

/// "Today", "Yesterday" or a short date, for the recent files list.
pub fn relative_day(when: &glib::DateTime) -> String {
    let Ok(now) = glib::DateTime::now_local() else {
        return String::new();
    };
    let Ok(when) = when.to_local() else {
        return String::new();
    };
    let same_day = |a: &glib::DateTime, b: &glib::DateTime| a.year() == b.year() && a.day_of_year() == b.day_of_year();

    if same_day(&now, &when) {
        return "Today".into();
    }
    if now.add_days(-1).is_ok_and(|yesterday| same_day(&yesterday, &when)) {
        return "Yesterday".into();
    }
    when.format("%-e %b").map(|text| text.to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::matches;

    const FIREFOX: &str = "firefox browse the world wide web /usr/lib/firefox/firefox firefox";

    #[test]
    fn empty_query_matches() {
        assert!(matches(FIREFOX, ""));
        assert!(matches(FIREFOX, "   "));
    }

    #[test]
    fn every_word_must_occur() {
        assert!(matches(FIREFOX, "fire"));
        assert!(matches(FIREFOX, "Web FIRE"));
        assert!(!matches(FIREFOX, "fire editor"));
    }
}
