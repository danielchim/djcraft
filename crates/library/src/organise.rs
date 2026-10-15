//! Organising the library: playlist folders and order, smart playlists,
//! automatic groups, suggestions for the next track, duplicates, and tracks
//! whose files have gone missing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::scan::{collect_audio_files, file_stamp, store_track};
use crate::{Camelot, Error, Library, Playlist, Result, SourceKind, Track, TrackQuery};

/// Seconds since 1970, now.
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// What a smart playlist holds: every track that passes all of its rules,
/// or any one of them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SmartRules {
    pub match_all: bool,
    pub rules: Vec<Rule>,
}

/// One test a track must pass to be in a smart playlist.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "snake_case")]
pub enum Rule {
    Text {
        field: TextField,
        op: TextOp,
        value: String,
    },
    /// Tempo from `min` to `max`, both included.
    Bpm { min: f64, max: f64 },
    /// Energy from `min` to `max`, both included.
    Energy { min: u8, max: u8 },
    /// Year from `min` to `max`, both included.
    Year { min: i32, max: i32 },
    /// In this key or, with `compatible`, in any key that mixes with it.
    Key { key: Camelot, compatible: bool },
    /// Joined the library in the last so many days.
    AddedWithinDays { days: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextField {
    Title,
    Artist,
    Album,
    Genre,
}

impl TextField {
    pub const ALL: [TextField; 4] = [
        TextField::Title,
        TextField::Artist,
        TextField::Album,
        TextField::Genre,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TextField::Title => "Title",
            TextField::Artist => "Artist",
            TextField::Album => "Album",
            TextField::Genre => "Genre",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextOp {
    Contains,
    DoesNotContain,
    Is,
    StartsWith,
}

impl TextOp {
    pub const ALL: [TextOp; 4] = [
        TextOp::Contains,
        TextOp::DoesNotContain,
        TextOp::Is,
        TextOp::StartsWith,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TextOp::Contains => "contains",
            TextOp::DoesNotContain => "does not contain",
            TextOp::Is => "is",
            TextOp::StartsWith => "starts with",
        }
    }
}

impl SmartRules {
    pub(crate) fn from_json(text: &str) -> Option<SmartRules> {
        serde_json::from_str(text).ok()
    }

    /// Whether a track belongs. No rules at all matches nothing, so a new,
    /// empty smart playlist is not the whole library.
    pub fn matches(&self, track: &Track, now: i64) -> bool {
        if self.rules.is_empty() {
            return false;
        }
        let mut results = self.rules.iter().map(|rule| rule.matches(track, now));
        if self.match_all {
            results.all(|passed| passed)
        } else {
            results.any(|passed| passed)
        }
    }
}

impl Rule {
    fn matches(&self, track: &Track, now: i64) -> bool {
        match self {
            Rule::Text { field, op, value } => {
                let text = match field {
                    TextField::Title => &track.title,
                    TextField::Artist => &track.artist,
                    TextField::Album => &track.album,
                    TextField::Genre => &track.genre,
                }
                .to_lowercase();
                let value = value.trim().to_lowercase();
                match op {
                    TextOp::Contains => text.contains(&value),
                    TextOp::DoesNotContain => !text.contains(&value),
                    TextOp::Is => text.trim() == value,
                    TextOp::StartsWith => text.starts_with(&value),
                }
            }
            Rule::Bpm { min, max } => track.bpm.is_some_and(|bpm| bpm >= *min && bpm <= *max),
            Rule::Energy { min, max } => track
                .energy
                .is_some_and(|energy| energy >= *min && energy <= *max),
            Rule::Year { min, max } => track.year.is_some_and(|year| year >= *min && year <= *max),
            Rule::Key { key, compatible } => track.camelot.is_some_and(|camelot| {
                if *compatible {
                    key.compatible().contains(&camelot)
                } else {
                    camelot == *key
                }
            }),
            Rule::AddedWithinDays { days } => now - track.added_at <= *days as i64 * 86_400,
        }
    }
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

/// How well a track's key mixes with another's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyFit {
    Same,
    /// One step round the Camelot wheel.
    Adjacent,
    /// The relative major or minor.
    Relative,
    /// One of the two keys is not known.
    Unknown,
}

impl KeyFit {
    pub fn label(self) -> &'static str {
        match self {
            KeyFit::Same => "same key",
            KeyFit::Adjacent => "next key",
            KeyFit::Relative => "relative key",
            KeyFit::Unknown => "key unknown",
        }
    }
}

/// A track that could follow another in a mix.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub track: Track,
    pub key_fit: KeyFit,
    /// How far the tempo would move to match, in percent: +2.0 means the
    /// suggested track must be sped up by 2%.
    pub tempo_change: f64,
}

/// Tracks that look like copies of one another, the one best kept first.
#[derive(Clone, Debug, PartialEq)]
pub struct DuplicateGroup {
    pub tracks: Vec<Track>,
}

/// Tempos further apart than this, in percent, are not suggested.
const MAX_TEMPO_CHANGE: f64 = 6.0;
/// Copies of a track may differ in length by this much, from encoding.
const DUPLICATE_SECONDS: f64 = 2.0;

impl Library {
    /// The rules of a smart playlist; `None` for an ordinary one.
    pub fn playlist_rules(&self, playlist: i64) -> Result<Option<SmartRules>> {
        let rules: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT rules FROM playlists WHERE id = ?1",
                [playlist],
                |row| row.get(0),
            )
            .optional()?;
        Ok(rules.flatten().as_deref().and_then(SmartRules::from_json))
    }

    pub fn create_smart_playlist(&self, name: &str, rules: &SmartRules) -> Result<Playlist> {
        let mut playlist = self.create_playlist(name)?;
        self.set_playlist_rules(playlist.id, rules)?;
        playlist.rules = Some(rules.clone());
        Ok(playlist)
    }

    /// Changes a smart playlist's rules.
    pub fn set_playlist_rules(&self, playlist: i64, rules: &SmartRules) -> Result<()> {
        let json = serde_json::to_string(rules).map_err(|e| Error::Library(e.to_string()))?;
        self.conn.execute(
            "UPDATE playlists SET rules = ?2 WHERE id = ?1",
            params![playlist, json],
        )?;
        Ok(())
    }

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

    /// Tracks that would mix well after `reference`: in a key that mixes with
    /// it and within a few percent of its tempo (or half or double it),
    /// best first. A reference with no tempo has no suggestions.
    pub fn suggest_next(&self, reference: i64, limit: usize) -> Result<Vec<Suggestion>> {
        let all = self.tracks(&TrackQuery::default())?;
        let Some(base) = all.iter().find(|track| track.id == reference) else {
            return Ok(Vec::new());
        };
        let Some(base_bpm) = base.bpm.filter(|bpm| *bpm > 0.0) else {
            return Ok(Vec::new());
        };
        let mut found: Vec<(f64, Suggestion)> = all
            .iter()
            .filter(|track| track.id != reference && !track.missing)
            .filter_map(|track| {
                let bpm = track.bpm.filter(|bpm| *bpm > 0.0)?;
                // A track at half or double the tempo mixes too.
                let change = [bpm, bpm * 2.0, bpm / 2.0]
                    .into_iter()
                    .map(|bpm| (base_bpm / bpm - 1.0) * 100.0)
                    .min_by(|a, b| a.abs().total_cmp(&b.abs()))?;
                if change.abs() > MAX_TEMPO_CHANGE {
                    return None;
                }
                let key_fit = match (base.camelot, track.camelot) {
                    (Some(a), Some(b)) if a == b => KeyFit::Same,
                    (Some(a), Some(b)) if a.major == b.major && a.compatible().contains(&b) => {
                        KeyFit::Adjacent
                    }
                    (Some(a), Some(b)) if a.number == b.number => KeyFit::Relative,
                    (Some(_), Some(_)) => return None,
                    _ => KeyFit::Unknown,
                };
                let key_cost = match key_fit {
                    KeyFit::Same => 0.0,
                    KeyFit::Adjacent => 1.0,
                    KeyFit::Relative => 1.5,
                    KeyFit::Unknown => 3.0,
                };
                let energy_cost = match (base.energy, track.energy) {
                    (Some(a), Some(b)) => (a as f64 - b as f64).abs() * 0.3,
                    _ => 0.5,
                };
                let cost = key_cost + change.abs() / 2.0 + energy_cost;
                Some((
                    cost,
                    Suggestion {
                        track: track.clone(),
                        key_fit,
                        tempo_change: change,
                    },
                ))
            })
            .collect();
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        Ok(found.into_iter().take(limit).map(|(_, s)| s).collect())
    }

    /// Groups of tracks that look like copies: the same artist and title
    /// with about the same length, or files of exactly the same size and
    /// length. In each group, the copy best kept comes first: one whose
    /// file is there, in the most playlists, with the largest file.
    pub fn duplicate_groups(&self) -> Result<Vec<DuplicateGroup>> {
        let tracks = self.tracks(&TrackQuery::default())?;
        let info: HashMap<i64, (i64, usize)> = {
            let mut statement = self.conn.prepare(
                "SELECT t.id, t.file_size, COUNT(pt.playlist_id) FROM tracks t
                 LEFT JOIN playlist_tracks pt ON pt.track_id = t.id GROUP BY t.id",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((row.get(0)?, (row.get(1)?, row.get::<_, i64>(2)? as usize)))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        // Joins tracks into groups as matches are found.
        let mut parent: Vec<usize> = (0..tracks.len()).collect();
        fn root(parent: &mut [usize], mut at: usize) -> usize {
            while parent[at] != at {
                parent[at] = parent[parent[at]];
                at = parent[at];
            }
            at
        }
        let join = |a: usize, b: usize, parent: &mut Vec<usize>| {
            let (a, b) = (root(parent, a), root(parent, b));
            if a != b {
                parent[a] = b;
            }
        };
        let normal = |text: &str| crate::lookup::normal_words(text);
        let mut by_name: HashMap<(String, String), Vec<usize>> = HashMap::new();
        let mut by_size: HashMap<i64, Vec<usize>> = HashMap::new();
        for (index, track) in tracks.iter().enumerate() {
            let title = normal(&track.title);
            if !title.is_empty() {
                by_name
                    .entry((normal(&track.artist), title))
                    .or_default()
                    .push(index);
            }
            if let Some((size, _)) = info.get(&track.id).filter(|(size, _)| *size > 0) {
                by_size.entry(*size).or_default().push(index);
            }
        }
        for candidates in by_name.values().chain(by_size.values()) {
            for (i, &a) in candidates.iter().enumerate() {
                for &b in &candidates[i + 1..] {
                    let close = (tracks[a].duration_seconds - tracks[b].duration_seconds).abs()
                        <= DUPLICATE_SECONDS;
                    if close {
                        join(a, b, &mut parent);
                    }
                }
            }
        }
        let mut groups: HashMap<usize, Vec<Track>> = HashMap::new();
        for (index, track) in tracks.iter().enumerate() {
            let group = root(&mut parent, index);
            groups.entry(group).or_default().push(track.clone());
        }
        let mut groups: Vec<DuplicateGroup> = groups
            .into_values()
            .filter(|tracks| tracks.len() > 1)
            .map(|mut tracks| {
                tracks.sort_by_key(|track| {
                    let (size, playlists) = info.get(&track.id).copied().unwrap_or((0, 0));
                    (
                        track.missing,
                        std::cmp::Reverse(playlists),
                        std::cmp::Reverse(size),
                    )
                });
                DuplicateGroup { tracks }
            })
            .collect();
        groups.sort_by_key(|group| {
            (
                group.tracks[0].artist.to_lowercase(),
                group.tracks[0].title.to_lowercase(),
            )
        });
        Ok(groups)
    }

    /// Keeps one copy of a track and takes the others out of the library.
    /// Their places in playlists and Flow pass to the kept copy, and their
    /// files are left on disk but no longer added by scans.
    pub fn merge_duplicates(&mut self, keep: i64, others: &[i64]) -> Result<()> {
        let transaction = self.conn.transaction()?;
        for &other in others.iter().filter(|other| **other != keep) {
            let playlists: Vec<i64> = {
                let mut statement = transaction
                    .prepare("SELECT playlist_id FROM playlist_tracks WHERE track_id = ?1")?;
                let rows = statement.query_map([other], |row| row.get(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            for playlist in playlists {
                let has_keep: bool = transaction.query_row(
                    "SELECT COUNT(*) > 0 FROM playlist_tracks WHERE playlist_id = ?1 AND track_id = ?2",
                    params![playlist, keep],
                    |row| row.get(0),
                )?;
                if has_keep {
                    transaction.execute(
                        "DELETE FROM playlist_tracks WHERE playlist_id = ?1 AND track_id = ?2",
                        params![playlist, other],
                    )?;
                } else {
                    transaction.execute(
                        "UPDATE playlist_tracks SET track_id = ?3 WHERE playlist_id = ?1 AND track_id = ?2",
                        params![playlist, other, keep],
                    )?;
                }
            }
            transaction.execute(
                "UPDATE flow_clips SET track_id = ?2 WHERE track_id = ?1",
                params![other, keep],
            )?;
            transaction.execute(
                "INSERT OR IGNORE INTO ignored_files (source_id, locator)
                 SELECT source_id, locator FROM tracks WHERE id = ?1",
                [other],
            )?;
            transaction.execute("DELETE FROM tracks WHERE id = ?1", [other])?;
        }
        transaction.commit()?;
        Ok(())
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
        transaction.execute(
            "DELETE FROM ignored_files WHERE source_id = ?1 AND locator = ?2",
            params![source_id, locator],
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

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, artist: &str, bpm: f64, key: &str, energy: u8) -> Track {
        Track {
            id: 0,
            source_id: 1,
            source_name: String::new(),
            source_kind: SourceKind::Folder,
            path: PathBuf::new(),
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            genre: "Tech House".into(),
            duration_seconds: 300.0,
            bpm: Some(bpm),
            key: Some(key.into()),
            camelot: Camelot::parse(key),
            energy: Some(energy),
            year: Some(2019),
            missing: false,
            added_at: 1_000_000,
        }
    }

    #[test]
    fn smart_rules_match_all_or_any() {
        let track = track("Losing It", "Fisher", 125.0, "8A", 7);
        let rules = |match_all: bool, rules: Vec<Rule>| SmartRules { match_all, rules };
        let fast = Rule::Bpm {
            min: 124.0,
            max: 128.0,
        };
        let slow = Rule::Bpm {
            min: 90.0,
            max: 110.0,
        };
        let fisher = Rule::Text {
            field: TextField::Artist,
            op: TextOp::Is,
            value: "fisher".into(),
        };
        let now = 1_000_000 + 3 * 86_400;
        assert!(rules(true, vec![fast.clone(), fisher.clone()]).matches(&track, now));
        assert!(!rules(true, vec![slow.clone(), fisher.clone()]).matches(&track, now));
        assert!(rules(false, vec![slow, fisher]).matches(&track, now));
        assert!(!rules(true, vec![]).matches(&track, now));

        let mixes = Rule::Key {
            key: Camelot::parse("9A").unwrap(),
            compatible: true,
        };
        assert!(rules(true, vec![mixes]).matches(&track, now));
        let recent = |days| Rule::AddedWithinDays { days };
        assert!(rules(true, vec![recent(7)]).matches(&track, now));
        assert!(!rules(true, vec![recent(2)]).matches(&track, now));
        let decade = Rule::Year {
            min: 2010,
            max: 2019,
        };
        let energetic = Rule::Energy { min: 7, max: 10 };
        let not_house = Rule::Text {
            field: TextField::Genre,
            op: TextOp::DoesNotContain,
            value: "house".into(),
        };
        assert!(rules(true, vec![decade, energetic.clone()]).matches(&track, now));
        assert!(!rules(true, vec![energetic, not_house]).matches(&track, now));

        // Rules keep their meaning through storage.
        let stored = rules(false, vec![fast, recent(30)]);
        let json = serde_json::to_string(&stored).unwrap();
        assert_eq!(SmartRules::from_json(&json), Some(stored));
    }
}
