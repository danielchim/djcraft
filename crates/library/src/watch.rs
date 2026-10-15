//! Watching folder sources for changes on disk.
//!
//! The watcher only notes which sources changed and when. Bringing the
//! library up to date is still [`Library::scan`](crate::Library::scan),
//! which opens nothing that has not changed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::scan::is_audio_file;
use crate::{Error, Result};

/// When each changed source last saw a change.
type Changed = Arc<Mutex<HashMap<i64, Instant>>>;

/// Watches folders and remembers which of them have changed since they
/// were last asked about.
#[derive(Default)]
pub struct FolderWatcher {
    /// One watcher per source, so a change needs no matching to a folder.
    watchers: HashMap<i64, RecommendedWatcher>,
    changed: Changed,
}

impl FolderWatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts watching a folder and everything under it on behalf of a
    /// source, replacing any folder watched for that source before.
    pub fn watch(&mut self, source_id: i64, folder: &Path) -> Result<()> {
        // Changes are reported by real path, which a linked folder's is not.
        let root = folder.canonicalize()?;
        let changed = self.changed.clone();
        let filter_root = root.clone();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            // An error means changes may have been missed, so look again.
            if event.map_or(true, |event| concerns_library(&filter_root, &event)) {
                changed
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(source_id, Instant::now());
            }
        })
        .map_err(watch_error)?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(watch_error)?;
        self.watchers.insert(source_id, watcher);
        Ok(())
    }

    pub fn unwatch(&mut self, source_id: i64) {
        self.watchers.remove(&source_id);
        self.lock().remove(&source_id);
    }

    /// The sources that changed and have since been left alone for `quiet`,
    /// in order of id. Each is reported once per burst of changes; one
    /// still being written to (a copy under way, say) waits its turn.
    pub fn take_changed(&self, quiet: Duration) -> Vec<i64> {
        let mut changed = self.lock();
        let mut settled: Vec<i64> = changed
            .iter()
            .filter(|(_, last)| last.elapsed() >= quiet)
            .map(|(id, _)| *id)
            .collect();
        settled.sort_unstable();
        for id in &settled {
            changed.remove(id);
        }
        settled
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Instant>> {
        self.changed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn watch_error(error: notify::Error) -> Error {
    Error::Watch(error.to_string())
}

/// Whether a change under `root` could alter what a scan of it finds.
fn concerns_library(root: &Path, event: &Event) -> bool {
    // Reading a file is reported on some systems, and a scan reads files:
    // counting those would have every scan set off the next.
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    // With no path given, anything may have changed.
    event.paths.is_empty() || event.paths.iter().any(|path| could_be_track(root, path))
}

fn could_be_track(root: &Path, path: &Path) -> bool {
    // Scans skip hidden files and folders.
    let hidden = |path: &Path| {
        path.components()
            .any(|part| part.as_os_str().to_string_lossy().starts_with('.'))
    };
    let within: PathBuf = match path.strip_prefix(root) {
        Ok(relative) => relative.to_path_buf(),
        Err(_) => path.file_name().map(PathBuf::from).unwrap_or_default(),
    };
    if hidden(&within) {
        return false;
    }
    // A folder that came or went takes its tracks with it, and a path that
    // is gone cannot be told from a folder, so only what is plainly some
    // other kind of file is passed over.
    !path.is_file() || is_audio_file(path)
}

#[cfg(test)]
mod tests {
    use notify::event::{AccessKind, CreateKind, RemoveKind};

    use super::*;

    fn event(kind: EventKind, path: &str) -> Event {
        Event::new(kind).add_path(PathBuf::from(path))
    }

    #[test]
    fn only_changes_a_scan_would_see_count() {
        let root = Path::new("/music");
        let created = EventKind::Create(CreateKind::Any);
        let removed = EventKind::Remove(RemoveKind::Any);

        assert!(concerns_library(
            root,
            &event(created, "/music/house/one.mp3")
        ));
        // Gone from disk, so it may have been a track or a folder of them.
        assert!(concerns_library(root, &event(removed, "/music/house")));
        assert!(concerns_library(root, &Event::new(created)));

        assert!(!concerns_library(root, &event(created, "/music/.DS_Store")));
        assert!(!concerns_library(
            root,
            &event(created, "/music/.trash/one.mp3")
        ));
        assert!(!concerns_library(
            root,
            &event(EventKind::Access(AccessKind::Any), "/music/one.mp3")
        ));
        // A hidden folder above the root does not hide what is in it.
        assert!(concerns_library(
            Path::new("/home/.dj/music"),
            &event(created, "/home/.dj/music/one.mp3")
        ));
    }

    #[test]
    fn files_that_are_not_audio_are_passed_over() {
        let dir = std::env::temp_dir().join(format!("djcraft-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("album")).unwrap();
        std::fs::write(dir.join("cover.jpg"), b"x").unwrap();
        std::fs::write(dir.join("one.MP3"), b"x").unwrap();

        assert!(!could_be_track(&dir, &dir.join("cover.jpg")));
        assert!(could_be_track(&dir, &dir.join("one.MP3")));
        assert!(could_be_track(&dir, &dir.join("album")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
