//! Finding files by name.
//!
//! A worker thread walks the home folder once, remembers every file name
//! (up to a cap), and answers queries against that list. Walking a disk and
//! scoring tens of thousands of names takes too long for the UI thread, so
//! the UI only sends messages to the worker and receives results back.

use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use crate::search::Ranker;

/// Stop indexing after this many files, so a huge home folder cannot eat memory.
const MAX_FILES: usize = 60_000;
/// How deep below the home folder to look.
const MAX_DEPTH: usize = 8;
/// Folders that hold machine-made files nobody searches for by name.
const SKIPPED_FOLDERS: [&str; 6] = ["target", "node_modules", "__pycache__", "venv", "build", "dist"];
/// Re-walk the disk at most this often.
const REINDEX_AFTER: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq)]
pub struct FileHit {
    pub name: String,
    /// The folder it is in, shown on the right ("Documents").
    pub folder: String,
    pub path: PathBuf,
}

enum Command {
    /// Walk the disk again, unless that happened recently.
    Refresh,
    /// Find files for this query; `generation` identifies the request.
    Query { generation: u64, text: String, limit: usize },
}

/// Handle to the worker thread.
pub struct FileSearch {
    commands: mpsc::Sender<Command>,
}

impl FileSearch {
    /// Start the worker. Results arrive on `results` as (generation, hits).
    pub fn start(results: async_channel::Sender<(u64, Vec<FileHit>)>) -> Self {
        let (commands, inbox) = mpsc::channel::<Command>();

        std::thread::spawn(move || {
            let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
                return;
            };
            let mut index = build_index(&home, MAX_FILES);
            let mut indexed_at = Instant::now();
            let mut ranker = Ranker::new();

            // `recv` sleeps until a command arrives; it fails (ending the
            // thread) when the UI side is gone.
            while let Ok(mut command) = inbox.recv() {
                // Typing sends many queries; only the newest matters.
                let mut refresh = matches!(command, Command::Refresh);
                while let Ok(newer) = inbox.try_recv() {
                    refresh |= matches!(newer, Command::Refresh);
                    if matches!(newer, Command::Query { .. }) || !matches!(command, Command::Query { .. }) {
                        command = newer;
                    }
                }

                if refresh && indexed_at.elapsed() > REINDEX_AFTER {
                    index = build_index(&home, MAX_FILES);
                    indexed_at = Instant::now();
                }
                if let Command::Query { generation, text, limit } = command {
                    let hits = ranker
                        .rank(index.iter(), &text, limit, |hit| (hit.name.as_str(), ""))
                        .into_iter()
                        .cloned()
                        .collect();
                    if results.send_blocking((generation, hits)).is_err() {
                        return;
                    }
                }
            }
        });

        Self { commands }
    }

    pub fn refresh(&self) {
        let _ = self.commands.send(Command::Refresh);
    }

    pub fn query(&self, generation: u64, text: &str, limit: usize) {
        let _ = self.commands.send(Command::Query {
            generation,
            text: text.to_string(),
            limit,
        });
    }
}

/// List the files below `root`: no hidden entries, no build folders, no
/// symlinks (which could loop), at most `cap` files.
fn build_index(root: &Path, cap: usize) -> Vec<FileHit> {
    let mut hits = Vec::new();
    // Folders still to visit, with their depth. A stack instead of
    // recursion keeps deep trees from overflowing the call stack.
    let mut pending = vec![(root.to_path_buf(), 0usize)];

    while let Some((folder, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        let folder_name = folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            // `file_type` does not follow symlinks.
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if depth < MAX_DEPTH && !SKIPPED_FOLDERS.contains(&name.as_str()) {
                    pending.push((entry.path(), depth + 1));
                }
            } else if kind.is_file() {
                hits.push(FileHit {
                    name,
                    folder: folder_name.clone(),
                    path: entry.path(),
                });
                if hits.len() >= cap {
                    return hits;
                }
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder that is removed again when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("meridian-find-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(&self, relative: &str) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"").unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn index_skips_hidden_and_build_folders() {
        let scratch = Scratch::new("skip");
        scratch.file("Documents/design-notes.md");
        scratch.file("Projects/compositor/design-tokens.toml");
        scratch.file(".config/secret.toml");
        scratch.file("Projects/compositor/target/debug/junk.o");
        scratch.file("Projects/web/node_modules/pkg/index.js");

        let mut names: Vec<String> = build_index(&scratch.0, 100).into_iter().map(|hit| hit.name).collect();
        names.sort();
        assert_eq!(names, ["design-notes.md", "design-tokens.toml"]);
    }

    #[test]
    fn index_records_the_folder_and_respects_the_cap() {
        let scratch = Scratch::new("cap");
        for i in 0..10 {
            scratch.file(&format!("Documents/file-{i}.txt"));
        }
        let hits = build_index(&scratch.0, 4);
        assert_eq!(hits.len(), 4);
        assert!(hits.iter().all(|hit| hit.folder == "Documents"));
    }
}
