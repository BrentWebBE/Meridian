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

/// A recently used file, for the "Files" section.
pub struct RecentFile {
    pub name: String,
    /// The folder it is in, shown on the right ("Documents").
    pub folder: String,
    pub uri: String,
}

/// Recently used files whose name matches the query, newest first.
///
/// This is not a full file search: it only knows files that some
/// application has opened or saved (GTK's shared "recent files" list).
pub fn recent_files(query: &str, limit: usize) -> Vec<RecentFile> {
    let mut items = gtk::RecentManager::default().items();
    items.retain(|item| item.exists() && matches(&item.display_name().to_lowercase(), query));
    items.sort_by_key(|item| std::cmp::Reverse(item.modified().to_unix()));

    items
        .into_iter()
        .take(limit)
        .map(|item| {
            let uri = item.uri().to_string();
            let folder = gio::File::for_uri(&uri)
                .parent()
                .and_then(|parent| parent.basename())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            RecentFile {
                name: item.display_name().to_string(),
                folder,
                uri,
            }
        })
        .collect()
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
