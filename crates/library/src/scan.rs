use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::{Accessor, ItemKey, Tag, TagExt};
use rusqlite::{Connection, params};

use crate::{Error, Library, Result, SourceKind};

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "aif", "aiff", "m4a", "aac", "ogg", "opus",
];
/// Files written per database transaction.
const BATCH: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScanProgress {
    pub done: usize,
    pub total: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScanSummary {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub removed: usize,
}

struct Tags {
    title: String,
    artist: String,
    album: String,
    genre: String,
    duration: f64,
    bpm: Option<f64>,
    key: Option<String>,
}

/// New values for a track's editable details.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackEdit {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub bpm: Option<f64>,
    pub key: Option<String>,
}

/// Cover art embedded in a file.
pub struct Artwork {
    pub data: Vec<u8>,
    /// For example `image/jpeg`, when the file says.
    pub mime_type: Option<String>,
}

/// The first picture embedded in an audio file, if it has one.
pub fn read_artwork(path: &Path) -> Option<Artwork> {
    let file = lofty::read_from_path(path).ok()?;
    let picture = file.tags().iter().find_map(|tag| tag.pictures().first())?;
    Some(Artwork {
        data: picture.data().to_vec(),
        mime_type: picture.mime_type().map(|mime| mime.as_str().to_string()),
    })
}

/// Writes edited details into a file's own tags, leaving everything else in
/// the tag (artwork, comments and so on) as it was.
fn write_tags(path: &Path, edit: &TrackEdit) -> Result<()> {
    let file = lofty::read_from_path(path).map_err(|e| Error::Tags(e.to_string()))?;
    let mut tag = file
        .primary_tag()
        .or_else(|| file.first_tag())
        .cloned()
        .unwrap_or_else(|| Tag::new(file.primary_tag_type()));

    let text = |value: &str| {
        Some(value.trim())
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    match text(&edit.title) {
        Some(title) => tag.set_title(title),
        None => tag.remove_title(),
    }
    match text(&edit.artist) {
        Some(artist) => tag.set_artist(artist),
        None => tag.remove_artist(),
    }
    match text(&edit.album) {
        Some(album) => tag.set_album(album),
        None => tag.remove_album(),
    }
    match text(&edit.genre) {
        Some(genre) => tag.set_genre(genre),
        None => tag.remove_genre(),
    }
    // Formats differ in which tempo field they have; each keeps the one it
    // knows and ignores the other.
    tag.remove_key(ItemKey::Bpm);
    tag.remove_key(ItemKey::IntegerBpm);
    if let Some(bpm) = edit.bpm.filter(|bpm| *bpm > 0.0) {
        let bpm = format!("{:.2}", bpm);
        let bpm = bpm.trim_end_matches('0').trim_end_matches('.').to_string();
        tag.insert_text(ItemKey::Bpm, bpm.clone());
        tag.insert_text(ItemKey::IntegerBpm, bpm);
    }
    tag.remove_key(ItemKey::InitialKey);
    if let Some(key) = edit.key.as_deref().and_then(text) {
        tag.insert_text(ItemKey::InitialKey, key);
    }
    tag.save_to_path(path, WriteOptions::default())
        .map_err(|e| Error::Tags(e.to_string()))
}

/// A file's size and modification time, used to tell whether it changed.
pub(crate) fn file_stamp(path: &Path) -> Option<(i64, i64)> {
    let metadata = path.metadata().ok()?;
    let modified = metadata.modified().map(unix_seconds).unwrap_or(0);
    Some((metadata.len() as i64, modified))
}

/// Reads a file's tags and stores it as a track, replacing what was known
/// about the same file. Its id and playlist memberships are kept, and it
/// is marked as needing analysis again.
pub(crate) fn store_track(
    conn: &Connection,
    source_id: i64,
    locator: &str,
    path: &Path,
    (size, modified): (i64, i64),
) -> Result<()> {
    let tags = read_tags(path);
    conn.execute(
        "INSERT INTO tracks (source_id, locator, title, artist, album, genre,
                             duration, bpm, key, file_size, modified_at, added_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT (source_id, locator) DO UPDATE SET
             title = excluded.title, artist = excluded.artist,
             album = excluded.album, genre = excluded.genre,
             duration = excluded.duration, bpm = excluded.bpm, key = excluded.key,
             file_size = excluded.file_size, modified_at = excluded.modified_at,
             tempo_version = NULL",
        params![
            source_id,
            locator,
            tags.title,
            tags.artist,
            tags.album,
            tags.genre,
            tags.duration,
            tags.bpm,
            tags.key,
            size,
            modified,
            unix_seconds(SystemTime::now())
        ],
    )?;
    Ok(())
}

impl Library {
    /// Changes a track's details. They are written to the file's tags
    /// first, so the file stays the record of what the track is.
    pub fn edit_track(&self, id: i64, edit: &TrackEdit) -> Result<()> {
        type Row = (
            i64,
            String,
            String,
            Option<f64>,
            Option<String>,
            Option<i64>,
        );
        let (source_id, root, locator, shown_bpm, shown_key, tempo_version): Row =
            self.conn.query_row(
                &format!(
                    "SELECT t.source_id, s.location, t.locator, {}, {}, t.tempo_version
                     FROM tracks t JOIN sources s ON s.id = t.source_id WHERE t.id = ?1",
                    crate::shown_bpm_sql(),
                    crate::shown_key_sql(),
                ),
                [id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )?;
        let path = Path::new(&root).join(&locator);
        write_tags(&path, edit)?;
        let stamp = file_stamp(&path)
            .ok_or_else(|| Error::Tags(format!("{} disappeared", path.display())))?;
        // Re-read what the file now says, rather than trust the request.
        store_track(&self.conn, source_id, &locator, &path, stamp)?;
        // Only the tags changed, so the audio need not be analysed again.
        // A tempo the user typed in stands until they change it.
        let same_bpm = match (edit.bpm, shown_bpm) {
            (Some(new), Some(shown)) => (new - shown).abs() < 0.005,
            (None, None) => true,
            _ => false,
        };
        self.conn.execute(
            "UPDATE tracks SET tempo_version = ?2 WHERE id = ?1",
            params![id, tempo_version],
        )?;
        if !same_bpm {
            self.conn
                .execute("UPDATE tracks SET bpm_manual = 1 WHERE id = ?1", [id])?;
        }
        // The same for a key: one typed in stands over a detected one.
        let typed_key = edit
            .key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty());
        if typed_key != shown_key.as_deref() {
            self.conn
                .execute("UPDATE tracks SET key_manual = 1 WHERE id = ?1", [id])?;
        }
        Ok(())
    }

    /// Brings a source up to date with what is on disk: new files are added,
    /// changed files re-read, and missing files removed. Files whose size
    /// and modification time are unchanged are not opened.
    pub fn scan(
        &mut self,
        source_id: i64,
        mut progress: impl FnMut(ScanProgress),
    ) -> Result<ScanSummary> {
        let Some(source) = self.source(source_id)? else {
            return Ok(ScanSummary::default());
        };
        let mut known: HashMap<String, (i64, i64)> = {
            let mut statement = self.conn.prepare(
                "SELECT locator, file_size, modified_at FROM tracks WHERE source_id = ?1",
            )?;
            let rows = statement.query_map([source_id], |row| {
                Ok((row.get(0)?, (row.get(1)?, row.get(2)?)))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        // Each file with the locator it is stored under.
        let mut files: Vec<(String, PathBuf)> = Vec::new();
        match source.kind {
            SourceKind::Folder => {
                let root = PathBuf::from(&source.location);
                // An unplugged drive must not look like every track was deleted.
                std::fs::read_dir(&root)?;
                let mut found = Vec::new();
                collect_audio_files(&root, &mut found);
                for path in found {
                    if let Ok(relative) = path.strip_prefix(&root) {
                        files.push((relative.to_string_lossy().into_owned(), path));
                    }
                }
            }
            // Loose files are only ever added by name, so there is nothing
            // to discover: just check on the ones already listed.
            SourceKind::Files => {
                for locator in known.keys() {
                    files.push((locator.clone(), PathBuf::from(locator)));
                }
            }
        }
        files.sort();

        let mut summary = ScanSummary::default();
        let total = files.len();
        for (batch_index, batch) in files.chunks(BATCH).enumerate() {
            let transaction = self.conn.transaction()?;
            for (locator, path) in batch {
                let Some(stamp) = file_stamp(path) else {
                    continue;
                };
                let previous = known.remove(locator);
                if previous == Some(stamp) {
                    summary.unchanged += 1;
                    continue;
                }
                store_track(&transaction, source_id, locator, path, stamp)?;
                if previous.is_some() {
                    summary.updated += 1;
                } else {
                    summary.added += 1;
                }
            }
            transaction.commit()?;
            progress(ScanProgress {
                done: (batch_index * BATCH + batch.len()).min(total),
                total,
            });
        }

        // Whatever is left in `known` is no longer on disk.
        let transaction = self.conn.transaction()?;
        for locator in known.keys() {
            transaction.execute(
                "DELETE FROM tracks WHERE source_id = ?1 AND locator = ?2",
                params![source_id, locator],
            )?;
            summary.removed += 1;
        }
        transaction.commit()?;
        Ok(summary)
    }
}

fn unix_seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Finds audio files under `dir`, skipping hidden files and folders.
/// Unreadable folders are skipped.
fn collect_audio_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        // `metadata` follows symlinks, so linked folders and files count.
        let Ok(metadata) = path.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            collect_audio_files(&path, files);
        } else if is_audio_file(&path) {
            files.push(path);
        }
    }
}

/// Whether a path has the extension of a format the library reads.
pub(crate) fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Reads what the file's tags say. A file with no readable tags still gets
/// an entry, titled after its filename.
fn read_tags(path: &Path) -> Tags {
    let mut tags = Tags {
        title: String::new(),
        artist: String::new(),
        album: String::new(),
        genre: String::new(),
        duration: 0.0,
        bpm: None,
        key: None,
    };
    if let Ok(file) = lofty::read_from_path(path) {
        tags.duration = file.properties().duration().as_secs_f64();
        if let Some(tag) = file.primary_tag().or_else(|| file.first_tag()) {
            let text = |value: Option<std::borrow::Cow<'_, str>>| {
                value.map(|v| v.trim().to_string()).unwrap_or_default()
            };
            tags.title = text(tag.title());
            tags.artist = text(tag.artist());
            tags.album = text(tag.album());
            tags.genre = text(tag.genre());
            tags.bpm = tag
                .get_string(ItemKey::Bpm)
                .or_else(|| tag.get_string(ItemKey::IntegerBpm))
                .and_then(|bpm| bpm.trim().parse::<f64>().ok())
                .filter(|bpm| *bpm > 0.0);
            tags.key = tag
                .get_string(ItemKey::InitialKey)
                .map(|key| key.trim().to_string())
                .filter(|key| !key.is_empty());
        }
    }
    if tags.title.is_empty() {
        tags.title = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    tags
}
