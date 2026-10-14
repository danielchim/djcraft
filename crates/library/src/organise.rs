//! Organising the library: automatic groups of tracks, and
//! tracks whose files have gone missing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};

use crate::scan::{collect_audio_files, file_stamp, store_track};
use crate::{Error, Library, Result, SourceKind, Track, TrackQuery};

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

impl Library {
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
