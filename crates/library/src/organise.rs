//! Organising the library: playlist folders and order,
//! automatic groups, and tracks whose files have gone missing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension, params};

use crate::scan::{collect_audio_files, file_stamp, store_track};
use crate::{Error, Library, Result, SourceKind, Track, TrackQuery};

/// Seconds since 1970, now.
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A range of energy levels, for grouping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EnergyBand {
    /// 1 to 3.
    Low,
    /// 4 to 6.
    Medium,
    /// 7 to 10.
    High,
}

impl EnergyBand {
    pub const ALL: [EnergyBand; 3] = [EnergyBand::Low, EnergyBand::Medium, EnergyBand::High];

    fn range(self) -> (u8, u8) {
        match self {
            EnergyBand::Low => (1, 3),
            EnergyBand::Medium => (4, 6),
            EnergyBand::High => (7, 10),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EnergyBand::Low => "Low energy (1–3)",
            EnergyBand::Medium => "Medium energy (4–6)",
            EnergyBand::High => "High energy (7–10)",
        }
    }
}

/// An automatic group of tracks, made from their tags and analysis.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    Genre(String),
    Energy(EnergyBand),
    /// The decade's first year, such as 1990.
    Decade(i32),
    /// Tracks whose files were not found at the last scan.
    Missing,
}

impl Group {
    pub fn label(&self) -> String {
        match self {
            Group::Genre(genre) => genre.clone(),
            Group::Energy(band) => band.label().to_string(),
            Group::Decade(decade) => format!("{decade}s"),
            Group::Missing => "Missing files".to_string(),
        }
    }

    /// An SQL condition over `tracks t`, with parameters numbered from
    /// `first`, and the values for them.
    pub(crate) fn sql(&self, first: usize) -> (String, Vec<rusqlite::types::Value>) {
        match self {
            Group::Genre(genre) => (
                format!(" AND TRIM(t.genre) = ?{first} COLLATE NOCASE"),
                vec![genre.clone().into()],
            ),
            Group::Energy(band) => {
                let (low, high) = band.range();
                (
                    format!(" AND t.energy BETWEEN ?{first} AND ?{}", first + 1),
                    vec![(low as i64).into(), (high as i64).into()],
                )
            }
            Group::Decade(decade) => (
                format!(" AND t.year BETWEEN ?{first} AND ?{}", first + 1),
                vec![(*decade as i64).into(), (*decade as i64 + 9).into()],
            ),
            Group::Missing => (" AND t.missing = 1".to_string(), Vec::new()),
        }
    }
}

/// A group and how many tracks it has.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupCount {
    pub group: Group,
    pub count: usize,
}

/// A folder of playlists, which may sit in another folder.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistFolder {
    pub id: i64,
    pub name: String,
    pub parent: Option<i64>,
}

impl Library {
    /// Moves a track to another place in a playlist, counting from 0. The
    /// tracks between shift up or down to make room.
    pub fn move_in_playlist(&mut self, playlist: i64, track: i64, to: usize) -> Result<()> {
        let transaction = self.conn.transaction()?;
        let mut order: Vec<i64> = {
            let mut statement = transaction.prepare(
                "SELECT track_id FROM playlist_tracks WHERE playlist_id = ?1 ORDER BY position",
            )?;
            let rows = statement.query_map([playlist], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let Some(from) = order.iter().position(|id| *id == track) else {
            return Ok(());
        };
        let moved = order.remove(from);
        order.insert(to.min(order.len()), moved);
        for (position, id) in order.iter().enumerate() {
            transaction.execute(
                "UPDATE playlist_tracks SET position = ?3 WHERE playlist_id = ?1 AND track_id = ?2",
                params![playlist, id, position as i64 + 1],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn create_folder(&self, name: &str, parent: Option<i64>) -> Result<PlaylistFolder> {
        self.conn.execute(
            "INSERT INTO playlist_folders (name, parent_id, created_at) VALUES (?1, ?2, ?3)",
            params![name, parent, now()],
        )?;
        Ok(PlaylistFolder {
            id: self.conn.last_insert_rowid(),
            name: name.to_string(),
            parent,
        })
    }

    /// All playlist folders, oldest first.
    pub fn folders(&self) -> Result<Vec<PlaylistFolder>> {
        let mut statement = self
            .conn
            .prepare("SELECT id, name, parent_id FROM playlist_folders ORDER BY created_at, id")?;
        let folders = statement.query_map([], |row| {
            Ok(PlaylistFolder {
                id: row.get(0)?,
                name: row.get(1)?,
                parent: row.get(2)?,
            })
        })?;
        Ok(folders.collect::<rusqlite::Result<_>>()?)
    }

    pub fn rename_folder(&self, id: i64, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE playlist_folders SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(())
    }

    /// Deletes a folder. What was in it moves up to the folder it was in, so
    /// no playlist is lost.
    pub fn delete_folder(&mut self, id: i64) -> Result<()> {
        let transaction = self.conn.transaction()?;
        let parent: Option<i64> = transaction.query_row(
            "SELECT parent_id FROM playlist_folders WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE playlists SET folder_id = ?2 WHERE folder_id = ?1",
            params![id, parent],
        )?;
        transaction.execute(
            "UPDATE playlist_folders SET parent_id = ?2 WHERE parent_id = ?1",
            params![id, parent],
        )?;
        transaction.execute("DELETE FROM playlist_folders WHERE id = ?1", [id])?;
        transaction.commit()?;
        Ok(())
    }

    /// Puts a playlist in a folder, or at the top level with `None`.
    pub fn move_playlist(&self, playlist: i64, folder: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE playlists SET folder_id = ?2 WHERE id = ?1",
            params![playlist, folder],
        )?;
        Ok(())
    }

    /// Puts a folder in another, or at the top level with `None`. A folder
    /// cannot go inside itself or one of its own subfolders.
    pub fn move_folder(&self, folder: i64, parent: Option<i64>) -> Result<()> {
        let folders = self.folders()?;
        let mut at = parent;
        while let Some(id) = at {
            if id == folder {
                return Err(Error::Library("a folder cannot go inside itself".into()));
            }
            at = folders.iter().find(|f| f.id == id).and_then(|f| f.parent);
        }
        self.conn.execute(
            "UPDATE playlist_folders SET parent_id = ?2 WHERE id = ?1",
            params![folder, parent],
        )?;
        Ok(())
    }

    /// The automatic groups the library's tracks fall into: genres, energy
    /// bands and decades, each with its number of tracks, and the tracks
    /// that are missing. Groups with no tracks are left out.
    pub fn groups(&self) -> Result<Vec<GroupCount>> {
        let tracks = self.tracks(&TrackQuery::default())?;
        let mut genres: HashMap<String, (String, usize)> = HashMap::new();
        let mut bands: HashMap<EnergyBand, usize> = HashMap::new();
        let mut decades: HashMap<i32, usize> = HashMap::new();
        let mut missing = 0;
        for track in &tracks {
            let genre = track.genre.trim();
            if !genre.is_empty() {
                // Spelled however its first track spells it.
                genres
                    .entry(genre.to_lowercase())
                    .or_insert_with(|| (genre.to_string(), 0))
                    .1 += 1;
            }
            if let Some(energy) = track.energy {
                let band = EnergyBand::ALL
                    .into_iter()
                    .find(|band| (band.range().0..=band.range().1).contains(&energy));
                if let Some(band) = band {
                    *bands.entry(band).or_default() += 1;
                }
            }
            if let Some(year) = track.year {
                *decades.entry(year / 10 * 10).or_default() += 1;
            }
            if track.missing {
                missing += 1;
            }
        }
        let mut groups = Vec::new();
        let mut genres: Vec<_> = genres.into_values().collect();
        genres.sort_by_key(|(name, _)| name.to_lowercase());
        groups.extend(genres.into_iter().map(|(name, count)| GroupCount {
            group: Group::Genre(name),
            count,
        }));
        groups.extend(EnergyBand::ALL.into_iter().filter_map(|band| {
            bands.get(&band).map(|count| GroupCount {
                group: Group::Energy(band),
                count: *count,
            })
        }));
        let mut decades: Vec<_> = decades.into_iter().collect();
        decades.sort();
        groups.extend(decades.into_iter().map(|(decade, count)| GroupCount {
            group: Group::Decade(decade),
            count,
        }));
        if missing > 0 {
            groups.push(GroupCount {
                group: Group::Missing,
                count: missing,
            });
        }
        Ok(groups)
    }

    /// For tracks marked missing, files under `folder` that are probably
    /// theirs: the same file name and, where the old size is known, the same
    /// size.
    pub fn find_moved(&self, folder: &Path) -> Result<Vec<(Track, PathBuf)>> {
        let missing = self.tracks(&TrackQuery {
            group: Some(Group::Missing),
            ..Default::default()
        })?;
        if missing.is_empty() {
            return Ok(Vec::new());
        }
        let mut found = Vec::new();
        collect_audio_files(folder, &mut found);
        let mut by_name: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for path in found {
            if let Some(name) = path.file_name() {
                by_name
                    .entry(name.to_string_lossy().to_lowercase())
                    .or_default()
                    .push(path);
            }
        }
        let sizes: HashMap<i64, i64> = {
            let mut statement = self
                .conn
                .prepare("SELECT id, file_size FROM tracks WHERE missing = 1")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut used = HashSet::new();
        let mut matches = Vec::new();
        for track in missing {
            let Some(name) = track.path.file_name() else {
                continue;
            };
            let size = sizes.get(&track.id).copied().unwrap_or(0);
            let candidate = by_name
                .get(&name.to_string_lossy().to_lowercase())
                .into_iter()
                .flatten()
                .find(|path| {
                    !used.contains(*path)
                        && (size <= 0 || path.metadata().is_ok_and(|m| m.len() as i64 == size))
                });
            if let Some(path) = candidate {
                used.insert(path.clone());
                matches.push((track, path.clone()));
            }
        }
        Ok(matches)
    }

    /// Points a track at a file in a new place, keeping its playlists, cue
    /// points and analysis. A file inside a library folder joins that
    /// folder; any other joins the library's loose files.
    pub fn relocate(&mut self, id: i64, file: &Path) -> Result<()> {
        let file = crate::canonical(file)?;
        let folder = self
            .sources()?
            .into_iter()
            .find(|source| source.kind == SourceKind::Folder && file.starts_with(&source.location));
        let (source_id, locator) = match folder {
            Some(source) => (
                source.id,
                file.strip_prefix(&source.location)
                    .map(|relative| relative.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            None => (
                self.loose_files_source()?,
                file.to_string_lossy().into_owned(),
            ),
        };
        let taken: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM tracks WHERE source_id = ?1 AND locator = ?2",
                params![source_id, locator],
                |row| row.get(0),
            )
            .optional()?;
        if taken.is_some_and(|taken| taken != id) {
            return Err(Error::Library(format!(
                "{} is already in the library as another track",
                file.display()
            )));
        }
        let stamp = file_stamp(&file)
            .ok_or_else(|| Error::Library(format!("{} cannot be read", file.display())))?;
        let transaction = self.conn.transaction()?;
        let tempo_version: Option<i64> = transaction.query_row(
            "SELECT tempo_version FROM tracks WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE tracks SET source_id = ?2, locator = ?3, missing = 0 WHERE id = ?1",
            params![id, source_id, locator],
        )?;
        store_track(&transaction, source_id, &locator, &file, stamp)?;
        // The same audio, so its analysis still holds.
        transaction.execute(
            "UPDATE tracks SET tempo_version = ?2 WHERE id = ?1",
            params![id, tempo_version],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Takes every track marked missing out of the library, with its place
    /// in playlists. Returns how many there were.
    pub fn remove_missing(&self) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM tracks WHERE missing = 1", [])?)
    }
}
