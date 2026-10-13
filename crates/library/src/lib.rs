//! Music library: the database of sources and tracks.
//!
//! A track belongs to a source and is found by a locator within it. Local
//! folders are the only source so far; other kinds (network shares, plugin
//! sources) fit the same two fields.

mod import;
pub mod lookup;
mod scan;

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

pub use import::{ImportSummary, ImportedPlaylist, read_playlists};
pub use scan::{Artwork, ScanProgress, ScanSummary, TrackEdit, read_artwork};

const SCHEMA_VERSION: i64 = 3;

/// A measured tempo is shown only when the analysis was at least this sure
/// of it. Below that (mixes with several tempos, music with no clear beat)
/// the tempo in the file's tags is shown instead.
const MIN_TEMPO_CONFIDENCE: f64 = 0.1;

#[derive(Debug)]
pub enum Error {
    Database(rusqlite::Error),
    Io(io::Error),
    /// The database was written by a newer djcraft.
    NewerSchema(i64),
    /// A playlist file from another program could not be understood.
    Import(String),
    /// A file's tags could not be written.
    Tags(String),
    /// An online lookup failed.
    Lookup(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Database(e) => write!(f, "library database error: {e}"),
            Error::Io(e) => write!(f, "library file error: {e}"),
            Error::Import(message) => write!(f, "cannot import: {message}"),
            Error::Tags(message) => write!(f, "cannot write tags: {message}"),
            Error::Lookup(message) => f.write_str(message),
            Error::NewerSchema(version) => write!(
                f,
                "library was created by a newer version (schema {version})"
            ),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Database(e)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub id: i64,
    pub kind: SourceKind,
    pub name: String,
    /// For a folder, its absolute path. Empty for loose files.
    pub location: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SourceKind {
    /// A folder; everything under it belongs to the library.
    Folder,
    /// Individual files from anywhere on disk, such as tracks brought in
    /// by an imported playlist. Each is located by its absolute path.
    Files,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: i64,
    pub source_id: i64,
    /// Where to load the track from.
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub duration_seconds: f64,
    /// The measured tempo when there is a trustworthy one, otherwise the
    /// tempo from the file's tags.
    pub bpm: Option<f64>,
    pub key: Option<String>,
}

/// What tempo analysis found for a track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoResult {
    pub bpm: f64,
    pub first_beat_seconds: f64,
    /// 0.0 to 1.0.
    pub confidence: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub track_count: usize,
}

/// Which tracks to list.
#[derive(Clone, Debug, Default)]
pub struct TrackQuery {
    /// Only tracks from this source.
    pub source: Option<i64>,
    /// Only tracks in this playlist, in playlist order.
    pub playlist: Option<i64>,
    /// Words that must each appear in the title, artist, album or genre.
    pub search: String,
}

/// One connection to the library database. Open one per thread; they share
/// the file safely.
pub struct Library {
    conn: Connection,
}

impl Library {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        // WAL lets the UI keep reading while a scan writes.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.busy_timeout(Duration::from_secs(10))?;

        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(Error::NewerSchema(version));
        }
        if version < 1 {
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE sources (
                     id INTEGER PRIMARY KEY,
                     kind TEXT NOT NULL,
                     location TEXT NOT NULL,
                     name TEXT NOT NULL,
                     UNIQUE (kind, location)
                 );
                 CREATE TABLE tracks (
                     id INTEGER PRIMARY KEY,
                     source_id INTEGER NOT NULL REFERENCES sources (id) ON DELETE CASCADE,
                     locator TEXT NOT NULL,
                     title TEXT NOT NULL,
                     artist TEXT NOT NULL,
                     album TEXT NOT NULL,
                     genre TEXT NOT NULL,
                     duration REAL NOT NULL,
                     bpm REAL,
                     key TEXT,
                     file_size INTEGER NOT NULL,
                     modified_at INTEGER NOT NULL,
                     added_at INTEGER NOT NULL,
                     UNIQUE (source_id, locator)
                 );
                 PRAGMA user_version = 1;
                 COMMIT;",
            )?;
        }
        if version < 2 {
            // A playlist holds references to tracks, so one playlist can mix
            // tracks from any number of sources.
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE playlists (
                     id INTEGER PRIMARY KEY,
                     name TEXT NOT NULL,
                     created_at INTEGER NOT NULL
                 );
                 CREATE TABLE playlist_tracks (
                     playlist_id INTEGER NOT NULL REFERENCES playlists (id) ON DELETE CASCADE,
                     track_id INTEGER NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
                     position INTEGER NOT NULL,
                     PRIMARY KEY (playlist_id, track_id)
                 );
                 CREATE INDEX playlist_tracks_order ON playlist_tracks (playlist_id, position);
                 PRAGMA user_version = 2;
                 COMMIT;",
            )?;
        }
        if version < 3 {
            // Results of tempo analysis. `tempo_version` says which version
            // of the analysis produced them; it is NULL for a track that has
            // not been analysed since its file last changed.
            // `bpm_manual` marks a tempo the user typed in, which analysis
            // must not replace.
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE tracks ADD COLUMN tempo_bpm REAL;
                 ALTER TABLE tracks ADD COLUMN tempo_first_beat REAL;
                 ALTER TABLE tracks ADD COLUMN tempo_confidence REAL;
                 ALTER TABLE tracks ADD COLUMN tempo_version INTEGER;
                 ALTER TABLE tracks ADD COLUMN bpm_manual INTEGER NOT NULL DEFAULT 0;
                 PRAGMA user_version = 3;
                 COMMIT;",
            )?;
        }
        Ok(Self { conn })
    }

    /// Tracks whose tempo has not been analysed by this `version` of the
    /// analysis, or whose file has changed since it was.
    pub fn pending_tempo_analysis(&self, version: i64) -> Result<Vec<(i64, PathBuf)>> {
        let mut statement = self.conn.prepare(
            "SELECT t.id, s.location, t.locator
             FROM tracks t JOIN sources s ON s.id = t.source_id
             WHERE t.tempo_version IS NULL OR t.tempo_version < ?1
             ORDER BY t.id",
        )?;
        let rows = statement.query_map([version], |row| {
            let root: String = row.get(1)?;
            let locator: String = row.get(2)?;
            Ok((row.get(0)?, Path::new(&root).join(locator)))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records the outcome of tempo analysis; `None` when no steady tempo
    /// was found. Either way the track is not analysed again until its file
    /// or the analysis version changes.
    pub fn store_tempo(&self, id: i64, version: i64, tempo: Option<TempoResult>) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET tempo_bpm = ?2, tempo_first_beat = ?3, tempo_confidence = ?4,
                               tempo_version = ?5
             WHERE id = ?1",
            params![
                id,
                tempo.map(|t| t.bpm),
                tempo.map(|t| t.first_beat_seconds),
                tempo.map(|t| t.confidence),
                version
            ],
        )?;
        Ok(())
    }

    /// The beat grid measured for a track: its tempo and first beat.
    pub fn beat_grid(&self, id: i64) -> Result<Option<TempoResult>> {
        Ok(self
            .conn
            .query_row(
                "SELECT tempo_bpm, tempo_first_beat, tempo_confidence FROM tracks
                 WHERE id = ?1 AND tempo_bpm IS NOT NULL AND tempo_confidence >= ?2",
                params![id, MIN_TEMPO_CONFIDENCE],
                |row| {
                    Ok(TempoResult {
                        bpm: row.get(0)?,
                        first_beat_seconds: row.get(1)?,
                        confidence: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn create_playlist(&self, name: &str) -> Result<Playlist> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO playlists (name, created_at) VALUES (?1, ?2)",
            params![name, now],
        )?;
        Ok(Playlist {
            id: self.conn.last_insert_rowid(),
            name: name.to_string(),
            track_count: 0,
        })
    }

    /// All playlists, oldest first.
    pub fn playlists(&self) -> Result<Vec<Playlist>> {
        let mut statement = self.conn.prepare(
            "SELECT p.id, p.name, COUNT(pt.track_id)
             FROM playlists p LEFT JOIN playlist_tracks pt ON pt.playlist_id = p.id
             GROUP BY p.id ORDER BY p.created_at, p.id",
        )?;
        let playlists = statement.query_map([], |row| {
            Ok(Playlist {
                id: row.get(0)?,
                name: row.get(1)?,
                track_count: row.get::<_, i64>(2)? as usize,
            })
        })?;
        Ok(playlists.collect::<rusqlite::Result<_>>()?)
    }

    pub fn rename_playlist(&self, id: i64, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE playlists SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(())
    }

    /// Deletes a playlist. Its tracks stay in the library.
    pub fn delete_playlist(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM playlists WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Appends tracks to a playlist, skipping any it already contains.
    /// Returns how many were added.
    pub fn add_to_playlist(&mut self, playlist: i64, tracks: &[i64]) -> Result<usize> {
        let transaction = self.conn.transaction()?;
        let mut position: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(position), 0) FROM playlist_tracks WHERE playlist_id = ?1",
            [playlist],
            |row| row.get(0),
        )?;
        let mut added = 0;
        for track in tracks {
            position += 1;
            added += transaction.execute(
                "INSERT OR IGNORE INTO playlist_tracks (playlist_id, track_id, position)
                 VALUES (?1, ?2, ?3)",
                params![playlist, track, position],
            )?;
        }
        transaction.commit()?;
        Ok(added)
    }

    pub fn remove_from_playlist(&self, playlist: i64, track: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM playlist_tracks WHERE playlist_id = ?1 AND track_id = ?2",
            params![playlist, track],
        )?;
        Ok(())
    }

    /// Registers a folder as a source, or returns the existing one. The
    /// folder is not read until [`Library::scan`] is called.
    pub fn add_folder(&self, folder: &Path) -> Result<Source> {
        let folder = folder.canonicalize()?;
        let location = folder.to_string_lossy().into_owned();
        let name = folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| location.clone());
        self.conn.execute(
            "INSERT OR IGNORE INTO sources (kind, location, name) VALUES ('folder', ?1, ?2)",
            params![location, name],
        )?;
        Ok(self.conn.query_row(
            "SELECT id, name, location, kind FROM sources WHERE kind = 'folder' AND location = ?1",
            [&location],
            source_from_row,
        )?)
    }

    pub fn source(&self, id: i64) -> Result<Option<Source>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, name, location, kind FROM sources WHERE id = ?1",
                [id],
                source_from_row,
            )
            .optional()?)
    }

    pub fn sources(&self) -> Result<Vec<Source>> {
        let mut statement = self
            .conn
            .prepare("SELECT id, name, location, kind FROM sources ORDER BY name COLLATE NOCASE")?;
        let sources = statement.query_map([], source_from_row)?;
        Ok(sources.collect::<rusqlite::Result<_>>()?)
    }

    /// Removes a source and its tracks from the library. Files are untouched.
    pub fn remove_source(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM sources WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Lists tracks, ordered by artist and then title, or in playlist order
    /// when the query names a playlist.
    pub fn tracks(&self, query: &TrackQuery) -> Result<Vec<Track>> {
        let mut sql = String::from(
            "SELECT t.id, t.source_id, s.location, t.locator, t.title, t.artist, t.album,
                    t.genre, t.duration,
                    CASE WHEN t.bpm_manual = 0 AND t.tempo_confidence >= ?3
                         THEN t.tempo_bpm ELSE t.bpm END,
                    t.key
             FROM tracks t JOIN sources s ON s.id = t.source_id
             LEFT JOIN playlist_tracks pt ON pt.track_id = t.id AND pt.playlist_id = ?2
             WHERE (?1 IS NULL OR t.source_id = ?1) AND (?2 IS NULL OR pt.track_id IS NOT NULL)",
        );
        let mut values: Vec<rusqlite::types::Value> = vec![
            query.source.into(),
            query.playlist.into(),
            MIN_TEMPO_CONFIDENCE.into(),
        ];
        for word in query.search.split_whitespace() {
            let escaped = word
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            values.push(format!("%{escaped}%").into());
            let n = values.len();
            sql.push_str(&format!(
                " AND (t.title LIKE ?{n} ESCAPE '\\' OR t.artist LIKE ?{n} ESCAPE '\\'
                   OR t.album LIKE ?{n} ESCAPE '\\' OR t.genre LIKE ?{n} ESCAPE '\\')"
            ));
        }
        sql.push_str(if query.playlist.is_some() {
            " ORDER BY pt.position"
        } else {
            " ORDER BY t.artist COLLATE NOCASE, t.title COLLATE NOCASE"
        });

        let mut statement = self.conn.prepare(&sql)?;
        let tracks = statement.query_map(rusqlite::params_from_iter(values), |row| {
            let root: String = row.get(2)?;
            let locator: String = row.get(3)?;
            Ok(Track {
                id: row.get(0)?,
                source_id: row.get(1)?,
                path: Path::new(&root).join(locator),
                title: row.get(4)?,
                artist: row.get(5)?,
                album: row.get(6)?,
                genre: row.get(7)?,
                duration_seconds: row.get(8)?,
                bpm: row.get(9)?,
                key: row.get(10)?,
            })
        })?;
        Ok(tracks.collect::<rusqlite::Result<_>>()?)
    }
}

fn source_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Source> {
    Ok(Source {
        id: row.get(0)?,
        name: row.get(1)?,
        location: row.get(2)?,
        kind: match row.get::<_, String>(3)?.as_str() {
            "files" => SourceKind::Files,
            _ => SourceKind::Folder,
        },
    })
}
