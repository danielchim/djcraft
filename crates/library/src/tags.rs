//! Changing tags in music files: one track from the info panel, or many at
//! once from the tag tools.
//!
//! Every change is first a [`PlannedChange`]: what the file says now and
//! what it would say after, for the user to review. Applying a set of
//! changes writes the files, keeps the library in step, and records the
//! old values as a batch that can be undone. A file that changed after it
//! was reviewed is skipped rather than overwritten.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::{OptionalExtension, params};

use crate::scan::{file_stamp, read_file_tags, store_track, unix_seconds, write_tags};
use crate::{Camelot, Error, Library, Result, SourceKind, TrackEdit, TrackQuery};

/// One file's change, ready to review.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedChange {
    pub track_id: i64,
    pub path: PathBuf,
    /// What the file's tags say now.
    pub before: TrackEdit,
    /// What they would say after.
    pub after: TrackEdit,
    /// A new name for the file, in the same folder.
    pub rename_to: Option<PathBuf>,
}

impl PlannedChange {
    /// The details that would change: name, value before, value after.
    pub fn differences(&self) -> Vec<(&'static str, String, String)> {
        let bpm = |bpm: Option<f64>| bpm.map(format_bpm).unwrap_or_default();
        let opt = |value: &Option<String>| value.clone().unwrap_or_default();
        let (b, a) = (&self.before, &self.after);
        let mut differences: Vec<(&'static str, String, String)> = [
            ("Title", b.title.clone(), a.title.clone()),
            ("Artist", b.artist.clone(), a.artist.clone()),
            ("Album", b.album.clone(), a.album.clone()),
            ("Genre", b.genre.clone(), a.genre.clone()),
            ("BPM", bpm(b.bpm), bpm(a.bpm)),
            ("Key", opt(&b.key), opt(&a.key)),
        ]
        .into_iter()
        .collect();
        if a.comment.is_some() {
            differences.push(("Comment", opt(&b.comment), opt(&a.comment)));
        }
        if a.grouping.is_some() {
            differences.push(("Grouping", opt(&b.grouping), opt(&a.grouping)));
        }
        if let Some(to) = &self.rename_to {
            let name = |path: &Path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };
            differences.push(("File name", name(&self.path), name(to)));
        }
        differences.retain(|(_, before, after)| before.trim() != after.trim());
        differences
    }

    /// Whether applying it would change nothing.
    pub fn is_empty(&self) -> bool {
        self.differences().is_empty()
    }
}

/// What happened when changes were applied or undone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ApplySummary {
    /// The batch the changes were recorded as, if any were made.
    pub batch: Option<i64>,
    pub written: usize,
    /// Files left alone, and why.
    pub skipped: Vec<(PathBuf, String)>,
}

/// A set of changes applied together, which can be undone together.
#[derive(Clone, Debug, PartialEq)]
pub struct TagBatch {
    pub id: i64,
    pub label: String,
    pub files: usize,
    pub undone: bool,
}

/// Where a track's key or energy is written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// Not written.
    #[default]
    Off,
    /// The tag's own key field. For energy, which has none, this is `Off`.
    KeyField,
    /// At the start of the comment, before anything already there.
    Comment,
    /// The grouping field, before anything already there.
    Grouping,
    /// "8A - Title"
    TitlePrefix,
    /// "Title - 8A"
    TitleSuffix,
    /// "8A - Artist"
    ArtistPrefix,
    /// "Artist - 8A"
    ArtistSuffix,
}

impl Placement {
    pub const FOR_KEY: [Placement; 8] = [
        Placement::KeyField,
        Placement::Comment,
        Placement::Grouping,
        Placement::TitlePrefix,
        Placement::TitleSuffix,
        Placement::ArtistPrefix,
        Placement::ArtistSuffix,
        Placement::Off,
    ];
    pub const FOR_ENERGY: [Placement; 7] = [
        Placement::Comment,
        Placement::Grouping,
        Placement::TitlePrefix,
        Placement::TitleSuffix,
        Placement::ArtistPrefix,
        Placement::ArtistSuffix,
        Placement::Off,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Placement::Off => "Do not write",
            Placement::KeyField => "Key field",
            Placement::Comment => "Start of comment",
            Placement::Grouping => "Start of grouping",
            Placement::TitlePrefix => "Before the title",
            Placement::TitleSuffix => "After the title",
            Placement::ArtistPrefix => "Before the artist",
            Placement::ArtistSuffix => "After the artist",
        }
    }
}

/// What "write analysis to tags" writes, and where.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignOptions {
    pub bpm: bool,
    pub key: Placement,
    pub energy: Placement,
}

/// A text field the clean-up tools work on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagField {
    Title,
    Artist,
    Album,
    Genre,
    Comment,
}

impl TagField {
    pub const ALL: [TagField; 5] = [
        TagField::Title,
        TagField::Artist,
        TagField::Album,
        TagField::Genre,
        TagField::Comment,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TagField::Title => "Title",
            TagField::Artist => "Artist",
            TagField::Album => "Album",
            TagField::Genre => "Genre",
            TagField::Comment => "Comment",
        }
    }
}

/// A clean-up to make across many tracks.
#[derive(Clone, Debug, PartialEq)]
pub enum Cleanup {
    /// Replace text in the chosen fields.
    Replace {
        find: String,
        replace: String,
        fields: Vec<TagField>,
        match_case: bool,
    },
    /// Trim the ends and turn runs of spaces into one.
    ExtraSpaces,
    /// Give titles, artists, albums and genres capital letters where
    /// they are all lowercase or all capitals.
    Capitalise,
}

/// Characters that are not allowed in file names on some system. A colon
/// becomes a dash, as in "Title - Remix"; a slash a hyphen, as in "AC-DC";
/// the rest an underscore.
const UNSAFE_IN_NAMES: &[char] = &['\\', '*', '?', '"', '<', '>', '|'];
/// Words left lowercase inside a title, unless they come first.
const SMALL_WORDS: &[&str] = &[
    "a", "an", "and", "as", "at", "but", "by", "feat", "feat.", "ft", "ft.", "for", "from", "in",
    "into", "nor", "of", "on", "or", "the", "to", "vs", "vs.", "with", "x",
];

impl Library {
    /// The file's current tags for a track, and where the file is.
    pub fn file_tags(&self, id: i64) -> Result<(PathBuf, TrackEdit)> {
        let path = self.track_path(id)?;
        Ok((path.clone(), read_file_tags(&path)?))
    }

    /// Changes a track's details at once, with no review or undo. The file
    /// stays the record of what the track is.
    pub fn edit_track(&mut self, id: i64, edit: &TrackEdit) -> Result<()> {
        let change = self.plan_edit(id, edit)?;
        self.write_change(&change)
    }

    /// The change that saving `edit` would make. A comment or grouping of
    /// `None` keeps what the file has.
    pub fn plan_edit(&self, id: i64, edit: &TrackEdit) -> Result<PlannedChange> {
        let (path, before) = self.file_tags(id)?;
        let mut after = edit.clone();
        if after.comment.is_none() {
            after.comment = before.comment.clone();
        }
        if after.grouping.is_none() {
            after.grouping = before.grouping.clone();
        }
        Ok(PlannedChange {
            track_id: id,
            path,
            before,
            after,
            rename_to: None,
        })
    }

    /// Writes the measured tempo, key and energy into the tags of the given
    /// tracks. Tracks that would not change are left out.
    pub fn plan_align(&self, ids: &[i64], options: &AlignOptions) -> Result<Vec<PlannedChange>> {
        self.plan_each(ids, |library, id, mut after| {
            let listed = library.listed(id)?;
            if options.bpm
                && let Some(bpm) = listed.bpm
            {
                after.bpm = Some((bpm * 100.0).round() / 100.0);
            }
            let key = listed.camelot.map(|key| key.to_string());
            let energy = listed.energy.map(|energy| format!("Energy {energy}"));
            place_markers(&mut after, options, key.as_deref(), energy.as_deref());
            Ok(after)
        })
    }

    /// Makes a clean-up across the given tracks.
    pub fn plan_cleanup(&self, ids: &[i64], cleanup: &Cleanup) -> Result<Vec<PlannedChange>> {
        self.plan_each(ids, |_, _, mut after| {
            match cleanup {
                Cleanup::Replace {
                    find,
                    replace,
                    fields,
                    match_case,
                } => {
                    if find.is_empty() {
                        return Ok(after);
                    }
                    for field in fields {
                        let value = field_mut(&mut after, *field);
                        *value = replace_text(value, find, replace, *match_case);
                    }
                }
                Cleanup::ExtraSpaces => {
                    for field in TagField::ALL {
                        let value = field_mut(&mut after, field);
                        *value = collapse_spaces(value);
                    }
                    if let Some(grouping) = &mut after.grouping {
                        *grouping = collapse_spaces(grouping);
                    }
                }
                Cleanup::Capitalise => {
                    for field in [
                        TagField::Title,
                        TagField::Artist,
                        TagField::Album,
                        TagField::Genre,
                    ] {
                        let value = field_mut(&mut after, field);
                        *value = capitalise(value);
                    }
                }
            }
            Ok(after)
        })
    }

    /// Renames the given tracks' files from their details, in the folder
    /// they are in. `template` names details in braces: {artist}, {title},
    /// {album}, {genre}, {bpm}, {key} and {energy}. A file whose new name
    /// is taken by another file is left out.
    pub fn plan_rename(&self, ids: &[i64], template: &str) -> Result<Vec<PlannedChange>> {
        let mut taken: HashSet<PathBuf> = HashSet::new();
        let mut changes = Vec::new();
        for &id in ids {
            let listed = self.listed(id)?;
            let path = listed.path.clone();
            let Some(stem) = render_name(template, &listed) else {
                continue;
            };
            let extension = path
                .extension()
                .map(|ext| format!(".{}", ext.to_string_lossy()))
                .unwrap_or_default();
            let folder = path.parent().unwrap_or(Path::new("")).to_path_buf();
            let mut target = folder.join(format!("{stem}{extension}"));
            let mut copy = 2;
            while target != path && (taken.contains(&lower(&target)) || occupied(&target, &path)) {
                target = folder.join(format!("{stem} ({copy}){extension}"));
                copy += 1;
            }
            taken.insert(lower(&target));
            if target == path {
                continue;
            }
            let before = read_file_tags(&path)?;
            changes.push(PlannedChange {
                track_id: id,
                path,
                after: before.clone(),
                before,
                rename_to: Some(target),
            });
        }
        Ok(changes)
    }

    /// Writes reviewed changes to their files as one batch, which
    /// [`Library::undo_batch`] can undo. A file whose tags changed since
    /// they were reviewed, or that has gone, is skipped.
    pub fn apply_changes(
        &mut self,
        label: &str,
        changes: &[PlannedChange],
    ) -> Result<ApplySummary> {
        let mut summary = ApplySummary::default();
        let mut batch = None;
        for change in changes.iter().filter(|change| !change.is_empty()) {
            if let Err(error) = self.check_unchanged(&change.path, &change.before) {
                summary.skipped.push((change.path.clone(), error));
                continue;
            }
            if let Err(error) = self.write_change(change) {
                summary
                    .skipped
                    .push((change.path.clone(), error.to_string()));
                continue;
            }
            // The batch is made with its first change, so that a run that
            // changes nothing leaves nothing to undo.
            let batch_id = match batch {
                Some(id) => id,
                None => {
                    self.conn.execute(
                        "INSERT INTO tag_batches (label, created_at) VALUES (?1, ?2)",
                        params![label, unix_seconds(SystemTime::now())],
                    )?;
                    let id = self.conn.last_insert_rowid();
                    batch = Some(id);
                    id
                }
            };
            let written = change.rename_to.as_ref().unwrap_or(&change.path);
            self.conn.execute(
                "INSERT INTO tag_history (batch_id, track_id, path_before, path_after, before, after)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    batch_id,
                    change.track_id,
                    change.path.to_string_lossy(),
                    written.to_string_lossy(),
                    to_json(&change.before)?,
                    to_json(&change.after)?
                ],
            )?;
            summary.written += 1;
        }
        summary.batch = batch;
        Ok(summary)
    }

    /// The most recent batch of changes that has not been undone.
    pub fn last_batch(&self) -> Result<Option<TagBatch>> {
        Ok(self
            .conn
            .query_row(
                "SELECT b.id, b.label, b.undone, COUNT(h.batch_id)
                 FROM tag_batches b LEFT JOIN tag_history h ON h.batch_id = b.id
                 WHERE b.undone = 0 GROUP BY b.id ORDER BY b.id DESC LIMIT 1",
                [],
                |row| {
                    Ok(TagBatch {
                        id: row.get(0)?,
                        label: row.get(1)?,
                        undone: row.get(2)?,
                        files: row.get::<_, i64>(3)? as usize,
                    })
                },
            )
            .optional()?)
    }

    /// Puts back what a batch changed: the old tags, and the old file
    /// names. A file changed again since is skipped.
    pub fn undo_batch(&mut self, batch: i64) -> Result<ApplySummary> {
        let entries: Vec<(Option<i64>, String, String, String, String)> = {
            let mut statement = self.conn.prepare(
                "SELECT track_id, path_before, path_after, before, after FROM tag_history
                 WHERE batch_id = ?1 ORDER BY rowid DESC",
            )?;
            let rows = statement.query_map([batch], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut summary = ApplySummary {
            batch: Some(batch),
            ..Default::default()
        };
        for (track_id, path_before, path_after, before, after) in entries {
            let (before, after): (TrackEdit, TrackEdit) = (from_json(&before)?, from_json(&after)?);
            let (old_path, path) = (PathBuf::from(path_before), PathBuf::from(path_after));
            if let Err(error) = self.check_unchanged(&path, &after) {
                summary.skipped.push((path, error));
                continue;
            }
            let Some(id) = track_id else {
                summary
                    .skipped
                    .push((path, "the track is no longer in the library".into()));
                continue;
            };
            let undo = PlannedChange {
                track_id: id,
                path: path.clone(),
                before: after,
                after: before,
                rename_to: (old_path != path).then_some(old_path),
            };
            match self.write_change(&undo) {
                Ok(()) => summary.written += 1,
                Err(error) => summary.skipped.push((path, error.to_string())),
            }
        }
        self.conn
            .execute("UPDATE tag_batches SET undone = 1 WHERE id = ?1", [batch])?;
        Ok(summary)
    }

    /// Plans the same kind of change for each track, leaving out those it
    /// would not change.
    fn plan_each(
        &self,
        ids: &[i64],
        change: impl Fn(&Library, i64, TrackEdit) -> Result<TrackEdit>,
    ) -> Result<Vec<PlannedChange>> {
        let mut changes = Vec::new();
        for &id in ids {
            let Ok((path, before)) = self.file_tags(id) else {
                continue;
            };
            let after = change(self, id, before.clone())?;
            let planned = PlannedChange {
                track_id: id,
                path,
                before,
                after,
                rename_to: None,
            };
            if !planned.is_empty() {
                changes.push(planned);
            }
        }
        Ok(changes)
    }

    /// A track as the library lists it.
    fn listed(&self, id: i64) -> Result<crate::Track> {
        let query = TrackQuery {
            id: Some(id),
            ..Default::default()
        };
        self.tracks(&query)?
            .pop()
            .ok_or_else(|| Error::Tags(format!("track {id} is not in the library")))
    }

    fn track_path(&self, id: i64) -> Result<PathBuf> {
        let (root, locator): (String, String) = self.conn.query_row(
            "SELECT s.location, t.locator FROM tracks t JOIN sources s ON s.id = t.source_id
             WHERE t.id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(Path::new(&root).join(locator))
    }

    /// An error message if the file's tags no longer say `expected`.
    fn check_unchanged(
        &self,
        path: &Path,
        expected: &TrackEdit,
    ) -> std::result::Result<(), String> {
        let now = read_file_tags(path).map_err(|_| "the file cannot be read".to_string())?;
        let same = |a: &TrackEdit, b: &TrackEdit| {
            a.title == b.title
                && a.artist == b.artist
                && a.album == b.album
                && a.genre == b.genre
                && a.key == b.key
                && match (a.bpm, b.bpm) {
                    (Some(x), Some(y)) => (x - y).abs() < 0.005,
                    (x, y) => x == y,
                }
                && (expected.comment.is_none() || a.comment == b.comment)
                && (expected.grouping.is_none() || a.grouping == b.grouping)
        };
        if same(&now, expected) {
            Ok(())
        } else {
            Err("its tags changed after the review".into())
        }
    }

    /// Writes one change: the tags, then the new name, then the library's
    /// record of the track, keeping its analysis and playlists.
    fn write_change(&mut self, change: &PlannedChange) -> Result<()> {
        type Row = (
            i64,
            String,
            String,
            Option<f64>,
            Option<String>,
            Option<i64>,
        );
        let (source_id, location, kind, shown_bpm, shown_key, tempo_version): Row =
            self.conn.query_row(
                &format!(
                    "SELECT t.source_id, s.location, s.kind, {}, {}, t.tempo_version
                     FROM tracks t JOIN sources s ON s.id = t.source_id WHERE t.id = ?1",
                    crate::shown_bpm_sql(),
                    crate::shown_key_sql(),
                ),
                [change.track_id],
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
        // A rename alone leaves the tags, and the file's date, untouched.
        if change.before != change.after {
            write_tags(&change.path, &change.after)?;
        }
        let mut path = change.path.clone();
        if let Some(target) = &change.rename_to {
            if occupied(target, &change.path) {
                return Err(Error::Tags(format!("{} already exists", target.display())));
            }
            std::fs::rename(&change.path, target)?;
            path = target.clone();
        }
        let locator = match crate::source_kind(&kind) {
            SourceKind::Folder => path
                .strip_prefix(&location)
                .map(|relative| relative.to_string_lossy().into_owned())
                .map_err(|_| Error::Tags(format!("{} left its folder", path.display())))?,
            SourceKind::Files => path.to_string_lossy().into_owned(),
        };
        self.conn.execute(
            "UPDATE tracks SET locator = ?2 WHERE id = ?1",
            params![change.track_id, locator],
        )?;
        let stamp = file_stamp(&path)
            .ok_or_else(|| Error::Tags(format!("{} disappeared", path.display())))?;
        // Re-read what the file now says, rather than trust the request.
        store_track(&self.conn, source_id, &locator, &path, stamp)?;
        // Only the tags changed, so the audio need not be analysed again.
        self.conn.execute(
            "UPDATE tracks SET tempo_version = ?2 WHERE id = ?1",
            params![change.track_id, tempo_version],
        )?;
        // A tempo or key typed in stands over a measured one from now on.
        let edit = &change.after;
        let same_bpm = match (edit.bpm, shown_bpm) {
            (Some(new), Some(shown)) => (new - shown).abs() < 0.005,
            (None, None) => true,
            _ => false,
        };
        if !same_bpm {
            self.conn.execute(
                "UPDATE tracks SET bpm_manual = 1 WHERE id = ?1",
                [change.track_id],
            )?;
        }
        let typed_key = edit
            .key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty());
        if typed_key != shown_key.as_deref() {
            self.conn.execute(
                "UPDATE tracks SET key_manual = 1 WHERE id = ?1",
                [change.track_id],
            )?;
        }
        Ok(())
    }
}

fn to_json(edit: &TrackEdit) -> Result<String> {
    serde_json::to_string(edit).map_err(|e| Error::Tags(e.to_string()))
}

fn from_json(text: &str) -> Result<TrackEdit> {
    serde_json::from_str(text).map_err(|e| Error::Tags(e.to_string()))
}

fn format_bpm(bpm: f64) -> String {
    let text = format!("{bpm:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Whether another file is already at `target`. On a system that ignores
/// case, a new name that differs from the old one only in case is free.
fn occupied(target: &Path, source: &Path) -> bool {
    match (target.canonicalize(), source.canonicalize()) {
        (Ok(target), Ok(source)) => target != source,
        (Ok(_), Err(_)) => true,
        (Err(_), _) => false,
    }
}

fn lower(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().to_lowercase())
}

fn field_mut(edit: &mut TrackEdit, field: TagField) -> &mut String {
    match field {
        TagField::Title => &mut edit.title,
        TagField::Artist => &mut edit.artist,
        TagField::Album => &mut edit.album,
        TagField::Genre => &mut edit.genre,
        TagField::Comment => edit.comment.get_or_insert_with(String::new),
    }
}

fn replace_text(text: &str, find: &str, replace: &str, match_case: bool) -> String {
    if match_case {
        return text.replace(find, replace);
    }
    let (lower_text, lower_find) = (text.to_lowercase(), find.to_lowercase());
    // Lowercasing can change byte lengths; fall back to an exact match then.
    if lower_text.len() != text.len() || lower_find.len() != find.len() {
        return text.replace(find, replace);
    }
    let mut out = String::new();
    let mut at = 0;
    while let Some(found) = lower_text[at..].find(&lower_find) {
        out.push_str(&text[at..at + found]);
        out.push_str(replace);
        at += found + find.len();
    }
    out.push_str(&text[at..]);
    out
}

fn collapse_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Title case for text that is all lowercase or all capitals. In text with
/// mixed case, only words that are all lowercase get a capital, so names
/// such as "deadmau5", "AC/DC" or "McCartney" survive unless the whole
/// field was typed in one case.
fn capitalise(text: &str) -> String {
    let letters: Vec<char> = text.chars().filter(|c| c.is_alphabetic()).collect();
    let uniform =
        letters.iter().all(|c| c.is_lowercase()) || letters.iter().all(|c| c.is_uppercase());
    let mut first = true;
    text.split(' ')
        .map(|word| {
            let is_first = first;
            first = first && word.is_empty();
            let bare = word.trim_start_matches(['(', '[', '"', '\'']);
            let lead = &word[..word.len() - bare.len()];
            let lowered = bare.to_lowercase();
            let all_lower = bare.chars().all(|c| !c.is_alphabetic() || c.is_lowercase());
            if !uniform && !all_lower {
                return word.to_string();
            }
            if bare.chars().any(|c| c.is_ascii_digit()) && !uniform {
                return word.to_string();
            }
            // Small words stay small, except at the start or after a bracket.
            if !is_first && lead.is_empty() && SMALL_WORDS.contains(&lowered.as_str()) {
                return lowered;
            }
            let mut chars = lowered.chars();
            match chars.next() {
                Some(c) => format!("{lead}{}{}", c.to_uppercase(), chars.as_str()),
                None => word.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A key written into text: "8A".
fn is_key_marker(token: &str) -> bool {
    let token = token.trim();
    let digits = token.chars().take_while(char::is_ascii_digit).count();
    (1..=2).contains(&digits) && token.len() == digits + 1 && Camelot::parse(token).is_some()
}

/// Energy written into text: "Energy 7".
fn is_energy_marker(token: &str) -> bool {
    token.trim().strip_prefix("Energy ").is_some_and(|level| {
        level
            .parse::<u8>()
            .is_ok_and(|level| (1..=10).contains(&level))
    })
}

fn is_marker(token: &str) -> bool {
    is_key_marker(token) || is_energy_marker(token)
}

/// Takes key and energy markers off the start of `text`.
fn strip_prefix_markers(text: &str) -> String {
    let mut parts: Vec<&str> = text.split(" - ").collect();
    while parts.len() > 1 && is_marker(parts[0]) {
        parts.remove(0);
    }
    if parts.len() == 1 && is_marker(parts[0]) {
        return String::new();
    }
    parts.join(" - ")
}

/// Takes key and energy markers off the end of `text`.
fn strip_suffix_markers(text: &str) -> String {
    let mut parts: Vec<&str> = text.split(" - ").collect();
    while parts.len() > 1 && is_marker(parts[parts.len() - 1]) {
        parts.pop();
    }
    parts.join(" - ")
}

/// Puts the key and energy where the options say, after taking out any
/// markers an earlier run left, so running it twice changes nothing.
fn place_markers(
    edit: &mut TrackEdit,
    options: &AlignOptions,
    key: Option<&str>,
    energy: Option<&str>,
) {
    let used = [options.key, options.energy];
    // Only fields this run writes to are cleared of old markers.
    for place in used {
        match place {
            Placement::TitlePrefix => edit.title = strip_prefix_markers(&edit.title),
            Placement::TitleSuffix => edit.title = strip_suffix_markers(&edit.title),
            Placement::ArtistPrefix => edit.artist = strip_prefix_markers(&edit.artist),
            Placement::ArtistSuffix => edit.artist = strip_suffix_markers(&edit.artist),
            Placement::Comment => {
                let comment = edit.comment.get_or_insert_with(String::new);
                *comment = strip_prefix_markers(comment);
            }
            Placement::Grouping => {
                let grouping = edit.grouping.get_or_insert_with(String::new);
                *grouping = strip_prefix_markers(grouping);
            }
            Placement::Off | Placement::KeyField => {}
        }
    }
    let mut markers: Vec<(Placement, &str)> = Vec::new();
    if let Some(key) = key {
        markers.push((options.key, key));
    }
    if let Some(energy) = energy {
        markers.push((options.energy, energy));
    }
    // Key first, then energy, wherever both go to the same place.
    for place in [
        Placement::KeyField,
        Placement::Comment,
        Placement::Grouping,
        Placement::TitlePrefix,
        Placement::TitleSuffix,
        Placement::ArtistPrefix,
        Placement::ArtistSuffix,
    ] {
        let here: Vec<&str> = markers
            .iter()
            .filter(|(p, _)| *p == place)
            .map(|(_, text)| *text)
            .collect();
        if here.is_empty() {
            continue;
        }
        let marker = here.join(" - ");
        let before = |text: &str| {
            if text.is_empty() {
                marker.clone()
            } else {
                format!("{marker} - {text}")
            }
        };
        let after = |text: &str| {
            if text.is_empty() {
                marker.clone()
            } else {
                format!("{text} - {marker}")
            }
        };
        match place {
            // The key field holds the key alone.
            Placement::KeyField => {
                if let Some(key) = key.filter(|_| options.key == Placement::KeyField) {
                    edit.key = Some(key.to_string());
                }
            }
            Placement::Comment => {
                let comment = edit.comment.get_or_insert_with(String::new);
                *comment = before(comment);
            }
            Placement::Grouping => {
                let grouping = edit.grouping.get_or_insert_with(String::new);
                *grouping = before(grouping);
            }
            Placement::TitlePrefix => edit.title = before(&edit.title),
            Placement::TitleSuffix => edit.title = after(&edit.title),
            Placement::ArtistPrefix => edit.artist = before(&edit.artist),
            Placement::ArtistSuffix => edit.artist = after(&edit.artist),
            Placement::Off => {}
        }
    }
}

/// A file name, without its extension, made from a template. `None` if
/// nothing is left once empty details and unsafe characters are taken out.
fn render_name(template: &str, track: &crate::Track) -> Option<String> {
    let bpm = track
        .bpm
        .map(|bpm| format!("{}", bpm.round() as i64))
        .unwrap_or_default();
    let key = track.camelot.map(|key| key.to_string()).unwrap_or_default();
    let energy = track
        .energy
        .map(|energy| energy.to_string())
        .unwrap_or_default();
    let mut name = template.to_string();
    for (token, value) in [
        ("{artist}", track.artist.as_str()),
        ("{title}", track.title.as_str()),
        ("{album}", track.album.as_str()),
        ("{genre}", track.genre.as_str()),
        ("{bpm}", bpm.as_str()),
        ("{key}", key.as_str()),
        ("{energy}", energy.as_str()),
    ] {
        name = name.replace(token, value);
    }
    let safe: String = name
        .replace(':', " -")
        .replace('/', "-")
        .chars()
        .map(|c| {
            if UNSAFE_IN_NAMES.contains(&c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    // A detail that was empty leaves its separators behind: " -  - Title".
    let mut parts: Vec<&str> = safe.split(" - ").map(str::trim).collect();
    parts.retain(|part| !part.is_empty() && *part != "-");
    let name = collapse_spaces(&parts.join(" - "));
    let name = name.trim_matches(|c: char| c == '.' || c == ' ' || c == '-' || c == '_');
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capitalises_text_typed_in_one_case() {
        assert_eq!(
            capitalise("losing it (original mix)"),
            "Losing It (Original Mix)"
        );
        assert_eq!(capitalise("ONE MORE TIME"), "One More Time");
        assert_eq!(capitalise("the sound of the city"), "The Sound of the City");
        assert_eq!(capitalise("deadmau5 and kaskade"), "Deadmau5 and Kaskade");
        // Mixed case is trusted, apart from words wholly in lowercase.
        assert_eq!(capitalise("AC/DC back in black"), "AC/DC Back in Black");
        assert_eq!(capitalise("Paul McCartney"), "Paul McCartney");
        assert_eq!(capitalise("deadmau5"), "Deadmau5");
        assert_eq!(capitalise(""), "");
    }

    #[test]
    fn replaces_and_tidies_text() {
        assert_eq!(
            replace_text("Original Mix", "original", "Extended", false),
            "Extended Mix"
        );
        assert_eq!(
            replace_text("Original Mix", "original", "Extended", true),
            "Original Mix"
        );
        assert_eq!(replace_text("a-b-c", "-", " - ", true), "a - b - c");
        assert_eq!(collapse_spaces("  Losing   It  "), "Losing It");
    }

    #[test]
    fn markers_go_where_asked_and_replace_old_ones() {
        let edit = TrackEdit {
            title: "Losing It".into(),
            artist: "Fisher".into(),
            comment: Some("Great track".into()),
            ..Default::default()
        };
        let place = |key: Placement, energy: Placement, edit: &TrackEdit| {
            let mut edit = edit.clone();
            let options = AlignOptions {
                bpm: false,
                key,
                energy,
            };
            place_markers(&mut edit, &options, Some("8A"), Some("Energy 7"));
            edit
        };

        let both = place(Placement::Comment, Placement::Comment, &edit);
        assert_eq!(both.comment.as_deref(), Some("8A - Energy 7 - Great track"));
        // Running it again, or with new values, does not pile up markers.
        let again = place(Placement::Comment, Placement::Comment, &both);
        assert_eq!(again, both);

        let title = place(Placement::TitlePrefix, Placement::TitleSuffix, &edit);
        assert_eq!(title.title, "8A - Losing It - Energy 7");
        assert_eq!(
            place(Placement::TitlePrefix, Placement::TitleSuffix, &title),
            title
        );

        let field = place(Placement::KeyField, Placement::ArtistSuffix, &edit);
        assert_eq!(field.key.as_deref(), Some("8A"));
        assert_eq!(field.artist, "Fisher - Energy 7");

        let grouping = place(Placement::Off, Placement::Grouping, &edit);
        assert_eq!(grouping.grouping.as_deref(), Some("Energy 7"));
        assert_eq!(grouping.key, None);
        // A title that merely starts with something like a key keeps it.
        assert_eq!(strip_prefix_markers("1A Tribute"), "1A Tribute");
        assert_eq!(strip_prefix_markers("8A"), "");
    }

    #[test]
    fn file_names_come_from_the_template() {
        let track = crate::Track {
            id: 1,
            source_id: 1,
            source_name: String::new(),
            source_kind: SourceKind::Folder,
            path: PathBuf::from("/music/x.mp3"),
            title: "Losing It: Remix".into(),
            artist: "Fisher".into(),
            album: String::new(),
            genre: String::new(),
            duration_seconds: 0.0,
            bpm: Some(125.02),
            key: Some("8A".into()),
            camelot: Camelot::parse("8A"),
            energy: Some(7),
            year: None,
            added_at: 0,
        };
        assert_eq!(
            render_name("{artist} - {title}", &track).as_deref(),
            Some("Fisher - Losing It - Remix")
        );
        assert_eq!(
            render_name("{key} - {bpm} - {artist} - {title}", &track).as_deref(),
            Some("8A - 125 - Fisher - Losing It - Remix")
        );
        // Empty details take their separators with them.
        assert_eq!(
            render_name("{album} - {artist} - {genre}", &track).as_deref(),
            Some("Fisher")
        );
        assert_eq!(render_name("{album}", &track), None);
        assert_eq!(
            render_name("../{title}", &track).as_deref(),
            Some("Losing It - Remix")
        );
        let mut slashed = track.clone();
        slashed.artist = "AC/DC".into();
        assert_eq!(render_name("{artist}", &slashed).as_deref(), Some("AC-DC"));
    }
}
