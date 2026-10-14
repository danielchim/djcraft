use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::{Accessor, ItemKey, Tag, TagExt};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

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
    /// Tracks whose files are gone. They stay in the library, marked
    /// missing, until they are found again or removed.
    pub removed: usize,
    /// Tracks whose files had moved within the folder, and were followed.
    pub relinked: usize,
}

struct Tags {
    title: String,
    artist: String,
    album: String,
    genre: String,
    duration: f64,
    bpm: Option<f64>,
    key: Option<String>,
    year: Option<i32>,
}

/// The details djcraft reads from and writes to a file's tags.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackEdit {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    /// `None` leaves the file's comment as it is.
    #[serde(default)]
    pub comment: Option<String>,
    /// The grouping (content group) field. `None` leaves it as it is.
    #[serde(default)]
    pub grouping: Option<String>,
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

/// What a file's tags say, as they say it: an empty title stays empty
/// rather than taking the file's name.
pub(crate) fn read_file_tags(path: &Path) -> Result<TrackEdit> {
    let file = lofty::read_from_path(path).map_err(|e| Error::Tags(e.to_string()))?;
    let Some(tag) = file.primary_tag().or_else(|| file.first_tag()) else {
        return Ok(TrackEdit {
            comment: Some(String::new()),
            grouping: Some(String::new()),
            ..TrackEdit::default()
        });
    };
    let text = |value: Option<std::borrow::Cow<'_, str>>| {
        value.map(|v| v.trim().to_string()).unwrap_or_default()
    };
    let item = |key: ItemKey| tag.get_string(key).map(|v| v.trim().to_string());
    Ok(TrackEdit {
        title: text(tag.title()),
        artist: text(tag.artist()),
        album: text(tag.album()),
        genre: text(tag.genre()),
        bpm: item(ItemKey::Bpm)
            .or_else(|| item(ItemKey::IntegerBpm))
            .and_then(|bpm| bpm.parse::<f64>().ok())
            .filter(|bpm| *bpm > 0.0),
        key: item(ItemKey::InitialKey).filter(|key| !key.is_empty()),
        comment: Some(text(tag.comment())),
        grouping: Some(item(ItemKey::ContentGroup).unwrap_or_default()),
    })
}

/// Writes details into a file's own tags, leaving everything else in the
/// tag (artwork, other fields) as it was.
pub(crate) fn write_tags(path: &Path, edit: &TrackEdit) -> Result<()> {
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
    if let Some(comment) = &edit.comment {
        match text(comment) {
            Some(comment) => tag.set_comment(comment),
            None => tag.remove_comment(),
        }
    }
    if let Some(grouping) = &edit.grouping {
        tag.remove_key(ItemKey::ContentGroup);
        if let Some(grouping) = text(grouping) {
            tag.insert_text(ItemKey::ContentGroup, grouping);
        }
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
                             duration, bpm, key, file_size, modified_at, added_at,
                             year, year_checked)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1)
         ON CONFLICT (source_id, locator) DO UPDATE SET
             title = excluded.title, artist = excluded.artist,
             album = excluded.album, genre = excluded.genre,
             duration = excluded.duration, bpm = excluded.bpm, key = excluded.key,
             file_size = excluded.file_size, modified_at = excluded.modified_at,
             year = excluded.year, year_checked = 1, missing = 0,
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
            unix_seconds(SystemTime::now()),
            tags.year
        ],
    )?;
    Ok(())
}

impl Library {
    /// Brings a source up to date with what is on disk: new files are added
    /// and changed files re-read. A file that moved within the folder (the
    /// same name and size, somewhere else) keeps its track; one that is gone
    /// leaves its track marked missing. Files whose size and modification
    /// time are unchanged are not opened.
    pub fn scan(
        &mut self,
        source_id: i64,
        mut progress: impl FnMut(ScanProgress),
    ) -> Result<ScanSummary> {
        let Some(source) = self.source(source_id)? else {
            return Ok(ScanSummary::default());
        };
        struct Known {
            id: i64,
            stamp: (i64, i64),
            year_checked: bool,
            missing: bool,
        }
        let mut known: HashMap<String, Known> = {
            let mut statement = self.conn.prepare(
                "SELECT locator, id, file_size, modified_at, year_checked, missing
                 FROM tracks WHERE source_id = ?1",
            )?;
            let rows = statement.query_map([source_id], |row| {
                Ok((
                    row.get(0)?,
                    Known {
                        id: row.get(1)?,
                        stamp: (row.get(2)?, row.get(3)?),
                        year_checked: row.get(4)?,
                        missing: row.get(5)?,
                    },
                ))
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

        // Tracks whose files are not where they were, by file name and size,
        // so that a file that turns up elsewhere can be matched to one.
        let present: HashSet<&str> = files.iter().map(|(locator, _)| locator.as_str()).collect();
        let mut gone: HashMap<(String, i64), i64> = known
            .iter()
            .filter(|(locator, _)| !present.contains(locator.as_str()))
            .map(|(locator, track)| ((file_name(locator), track.stamp.0), track.id))
            .collect();
        drop(present);

        let mut summary = ScanSummary::default();
        let total = files.len();
        for (batch_index, batch) in files.chunks(BATCH).enumerate() {
            let transaction = self.conn.transaction()?;
            for (locator, path) in batch {
                let Some(stamp) = file_stamp(path) else {
                    continue;
                };
                match known.remove(locator) {
                    Some(track) if track.stamp == stamp => {
                        if track.missing {
                            transaction.execute(
                                "UPDATE tracks SET missing = 0 WHERE id = ?1",
                                [track.id],
                            )?;
                        }
                        // Libraries from before years were read get them
                        // without the file counting as changed.
                        if !track.year_checked {
                            transaction.execute(
                                "UPDATE tracks SET year = ?2, year_checked = 1 WHERE id = ?1",
                                params![track.id, read_tags(path).year],
                            )?;
                        }
                        summary.unchanged += 1;
                    }
                    Some(_) => {
                        store_track(&transaction, source_id, locator, path, stamp)?;
                        summary.updated += 1;
                    }
                    None => {
                        let moved = gone.remove(&(file_name(locator), stamp.0));
                        if let Some(id) = moved {
                            // The track follows its file, keeping its id,
                            // playlists, cue points and analysis.
                            transaction.execute(
                                "UPDATE tracks SET locator = ?2, missing = 0 WHERE id = ?1",
                                params![id, locator],
                            )?;
                            known.retain(|_, track| track.id != id);
                            summary.relinked += 1;
                        } else {
                            summary.added += 1;
                        }
                        let tempo_version: Option<Option<i64>> = match moved {
                            Some(id) => Some(transaction.query_row(
                                "SELECT tempo_version FROM tracks WHERE id = ?1",
                                [id],
                                |row| row.get(0),
                            )?),
                            None => None,
                        };
                        store_track(&transaction, source_id, locator, path, stamp)?;
                        // A moved file is the same audio: no new analysis.
                        if let (Some(id), Some(version)) = (moved, tempo_version) {
                            transaction.execute(
                                "UPDATE tracks SET tempo_version = ?2 WHERE id = ?1",
                                params![id, version],
                            )?;
                        }
                    }
                }
            }
            transaction.commit()?;
            progress(ScanProgress {
                done: (batch_index * BATCH + batch.len()).min(total),
                total,
            });
        }

        // Whatever is left in `known` is no longer on disk. Its tracks stay,
        // with their playlists and cue points, until found or removed.
        let transaction = self.conn.transaction()?;
        for track in known.values() {
            if !track.missing {
                transaction.execute("UPDATE tracks SET missing = 1 WHERE id = ?1", [track.id])?;
                summary.removed += 1;
            }
        }
        transaction.commit()?;
        Ok(summary)
    }
}

/// The last part of a locator: the file's name.
fn file_name(locator: &str) -> String {
    Path::new(locator)
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub(crate) fn unix_seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Finds audio files under `dir`, skipping hidden files and folders.
/// Unreadable folders are skipped.
pub(crate) fn collect_audio_files(dir: &Path, files: &mut Vec<PathBuf>) {
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
        year: None,
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
            tags.year = tag
                .date()
                .map(|date| date.year as i32)
                .or_else(|| {
                    [
                        ItemKey::Year,
                        ItemKey::ReleaseDate,
                        ItemKey::OriginalReleaseDate,
                    ]
                    .into_iter()
                    .find_map(|key| tag.get_string(key))
                    .and_then(|text| text.trim().get(..4)?.parse().ok())
                })
                .filter(|year| (1000..=9999).contains(year));
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
