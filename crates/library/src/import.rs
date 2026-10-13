//! Playlists from other DJ software.
//!
//! Reading a playlist file only yields names and file paths
//! ([`read_playlists`]); [`Library::import_playlists`] then turns those into
//! library playlists, adding any tracks the library does not have yet.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};

use crate::scan::{file_stamp, store_track};
use crate::{Error, Library, Result, SourceKind};

/// A playlist as another program describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedPlaylist {
    /// Folders the playlist sits in are part of the name: "Folder / Name".
    pub name: String,
    pub files: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ImportSummary {
    pub playlists: usize,
    /// Playlist entries added.
    pub tracks: usize,
    /// Entries skipped because the file is not on this computer.
    pub missing: usize,
}

/// Reads playlists from any supported place:
///
/// - a rekordbox collection exported as XML,
/// - a VirtualDJ folder (the one holding its `Playlists` and `MyLists`),
/// - a single `.vdjfolder`, `.m3u` or `.m3u8` playlist file.
pub fn read_playlists(path: &Path) -> Result<Vec<ImportedPlaylist>> {
    if path.is_dir() {
        return virtualdj_folder(path);
    }
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    match extension.as_str() {
        "xml" => rekordbox_xml(&std::fs::read_to_string(path)?),
        "vdjfolder" => Ok(vec![ImportedPlaylist {
            name,
            files: vdjfolder(&std::fs::read_to_string(path)?)?,
        }]),
        "m3u" | "m3u8" => Ok(vec![ImportedPlaylist {
            name,
            files: m3u(
                &String::from_utf8_lossy(&std::fs::read(path)?),
                path.parent(),
            ),
        }]),
        _ => Err(Error::Import(format!(
            "{} is not a rekordbox XML, VirtualDJ or M3U playlist",
            path.display()
        ))),
    }
}

fn xml_error(error: roxmltree::Error) -> Error {
    Error::Import(format!("not valid XML: {error}"))
}

/// rekordbox's "Export Collection in xml format".
fn rekordbox_xml(text: &str) -> Result<Vec<ImportedPlaylist>> {
    let document = roxmltree::Document::parse(text).map_err(xml_error)?;
    let root = document.root_element();
    if root.tag_name().name() != "DJ_PLAYLISTS" {
        return Err(Error::Import(
            "this XML file is not a rekordbox collection".into(),
        ));
    }
    fn child<'a, 'input>(
        node: roxmltree::Node<'a, 'input>,
        name: &str,
    ) -> Option<roxmltree::Node<'a, 'input>> {
        node.children().find(|n| n.has_tag_name(name))
    }

    // Playlists refer to tracks by id or by location.
    let mut by_id = HashMap::new();
    if let Some(collection) = child(root, "COLLECTION") {
        for track in collection.children().filter(|n| n.has_tag_name("TRACK")) {
            if let (Some(id), Some(location)) =
                (track.attribute("TrackID"), track.attribute("Location"))
            {
                by_id.insert(id, file_url_to_path(location));
            }
        }
    }

    fn walk(
        node: roxmltree::Node<'_, '_>,
        folder: &str,
        by_id: &HashMap<&str, PathBuf>,
        playlists: &mut Vec<ImportedPlaylist>,
    ) {
        for item in node.children().filter(|n| n.has_tag_name("NODE")) {
            let name = item.attribute("Name").unwrap_or("Untitled");
            let name = if folder.is_empty() {
                name.to_string()
            } else {
                format!("{folder} / {name}")
            };
            // Type 0 is a folder, type 1 a playlist.
            if item.attribute("Type") == Some("0") {
                walk(item, &name, by_id, playlists);
                continue;
            }
            let by_location = item.attribute("KeyType") == Some("1");
            let files = item
                .children()
                .filter(|n| n.has_tag_name("TRACK"))
                .filter_map(|track| {
                    let key = track.attribute("Key")?;
                    if by_location {
                        Some(file_url_to_path(key))
                    } else {
                        by_id.get(key).cloned()
                    }
                })
                .collect();
            playlists.push(ImportedPlaylist { name, files });
        }
    }

    let mut playlists = Vec::new();
    // The single top-level node is rekordbox's invisible root folder.
    if let Some(top) = child(root, "PLAYLISTS").and_then(|p| child(p, "NODE")) {
        walk(top, "", &by_id, &mut playlists);
    }
    Ok(playlists)
}

/// Turns `file://localhost/Users/me/My%20Song.mp3` into a path.
fn file_url_to_path(url: &str) -> PathBuf {
    let rest = url
        .strip_prefix("file://localhost")
        .or_else(|| url.strip_prefix("file://"))
        .unwrap_or(url);
    let mut bytes = Vec::with_capacity(rest.len());
    let mut input = rest.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let hex = [input.next(), input.next()];
            if let [Some(high), Some(low)] = hex
                && let Ok(value) =
                    u8::from_str_radix(&format!("{}{}", high as char, low as char), 16)
            {
                bytes.push(value);
                continue;
            }
        }
        bytes.push(byte);
    }
    let decoded = String::from_utf8_lossy(&bytes).into_owned();
    // Windows locations look like `/C:/Music/song.mp3`.
    let has_drive = decoded.as_bytes().get(2) == Some(&b':');
    PathBuf::from(if has_drive { &decoded[1..] } else { &decoded })
}

/// A VirtualDJ list: `<VirtualFolder><song path="…"/>…</VirtualFolder>`.
fn vdjfolder(text: &str) -> Result<Vec<PathBuf>> {
    let document = roxmltree::Document::parse(text).map_err(xml_error)?;
    Ok(document
        .root_element()
        .children()
        .filter(|n| n.has_tag_name("song"))
        .filter_map(|song| song.attribute("path"))
        .map(PathBuf::from)
        .collect())
}

/// An M3U playlist: one path per line, with `#` lines as comments. Relative
/// paths are taken from the playlist's own folder.
fn m3u(text: &str, folder: Option<&Path>) -> Vec<PathBuf> {
    text.lines()
        .map(|line| line.trim().trim_start_matches('\u{feff}'))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let path = Path::new(line);
            match folder {
                Some(folder) if path.is_relative() => folder.join(path),
                _ => path.to_path_buf(),
            }
        })
        .collect()
}

/// Every playlist under a VirtualDJ home folder. Its `History` folder holds
/// one automatic list per day and is left out.
fn virtualdj_folder(home: &Path) -> Result<Vec<ImportedPlaylist>> {
    fn walk(dir: &Path, prefix: &str, playlists: &mut Vec<ImportedPlaylist>) -> Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)?
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        for path in entries {
            let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            let name = if prefix.is_empty() {
                stem
            } else {
                format!("{prefix} / {stem}")
            };
            if path.is_dir() {
                walk(&path, &name, playlists)?;
                continue;
            }
            let extension = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default();
            let files = match extension.to_ascii_lowercase().as_str() {
                "vdjfolder" => vdjfolder(&std::fs::read_to_string(&path)?)?,
                "m3u" | "m3u8" => m3u(&String::from_utf8_lossy(&std::fs::read(&path)?), Some(dir)),
                _ => continue,
            };
            playlists.push(ImportedPlaylist { name, files });
        }
        Ok(())
    }

    let mut playlists = Vec::new();
    let mut found_any = false;
    for folder in ["Playlists", "MyLists"] {
        let dir = home.join(folder);
        if dir.is_dir() {
            found_any = true;
            walk(&dir, "", &mut playlists)?;
        }
    }
    if !found_any {
        return Err(Error::Import(format!(
            "{} has no Playlists or MyLists folder, so it does not look like a VirtualDJ folder",
            home.display()
        )));
    }
    Ok(playlists)
}

impl Library {
    /// Creates a playlist for each imported one, or adds to the playlist of
    /// the same name if there is one. Tracks the library does not have are
    /// added to it; files that are not on disk are counted and skipped.
    pub fn import_playlists(&mut self, playlists: &[ImportedPlaylist]) -> Result<ImportSummary> {
        let folders: Vec<_> = self
            .sources()?
            .into_iter()
            .filter(|source| source.kind == SourceKind::Folder)
            .collect();
        let mut summary = ImportSummary::default();
        let mut track_ids: HashMap<&Path, Option<i64>> = HashMap::new();

        for imported in playlists {
            let mut tracks = Vec::new();
            for file in &imported.files {
                let id = match track_ids.get(file.as_path()) {
                    Some(id) => *id,
                    None => {
                        let id = self.track_for_file(file, &folders)?;
                        track_ids.insert(file, id);
                        id
                    }
                };
                match id {
                    Some(id) => tracks.push(id),
                    None => summary.missing += 1,
                }
            }
            let existing: Option<i64> = self
                .conn
                .query_row(
                    "SELECT id FROM playlists WHERE name = ?1",
                    [&imported.name],
                    |row| row.get(0),
                )
                .optional()?;
            let playlist = match existing {
                Some(id) => id,
                None => self.create_playlist(&imported.name)?.id,
            };
            summary.tracks += self.add_to_playlist(playlist, &tracks)?;
            summary.playlists += 1;
        }
        Ok(summary)
    }

    /// The library track for a file, adding it if needed. A file inside one
    /// of the library's folders belongs to that folder; any other file goes
    /// under the loose-files source. `None` if the file does not exist.
    fn track_for_file(&self, file: &Path, folders: &[crate::Source]) -> Result<Option<i64>> {
        let Ok(file) = crate::canonical(file) else {
            return Ok(None);
        };
        let Some(stamp) = file_stamp(&file).filter(|_| file.is_file()) else {
            return Ok(None);
        };
        let folder = folders
            .iter()
            .filter(|folder| file.starts_with(&folder.location))
            .max_by_key(|folder| folder.location.len());
        let (source_id, locator) = match folder {
            Some(folder) => {
                let relative = file.strip_prefix(&folder.location).unwrap_or(&file);
                (folder.id, relative.to_string_lossy().into_owned())
            }
            None => (
                self.loose_files_source()?,
                file.to_string_lossy().into_owned(),
            ),
        };
        let find = || {
            self.conn
                .query_row(
                    "SELECT id FROM tracks WHERE source_id = ?1 AND locator = ?2",
                    params![source_id, locator],
                    |row| row.get(0),
                )
                .optional()
        };
        if let Some(id) = find()? {
            return Ok(Some(id));
        }
        store_track(&self.conn, source_id, &locator, &file, stamp)?;
        Ok(find()?)
    }

    fn loose_files_source(&self) -> Result<i64> {
        self.conn.execute(
            "INSERT OR IGNORE INTO sources (kind, location, name)
             VALUES ('files', '', 'Imported files')",
            [],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM sources WHERE kind = 'files' AND location = ''",
            [],
            |row| row.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_rekordbox_locations() {
        assert_eq!(
            file_url_to_path("file://localhost/Users/me/My%20Song%20%231.mp3"),
            PathBuf::from("/Users/me/My Song #1.mp3")
        );
        assert_eq!(
            file_url_to_path("file://localhost/C:/Music/caf%C3%A9.mp3"),
            PathBuf::from("C:/Music/café.mp3")
        );
    }

    #[test]
    fn reads_m3u_lines() {
        let text = "\u{feff}#EXTM3U\n#EXTVDJ:<artist>A</artist>\n/abs/one.mp3\n\nsub/two.mp3\r\n";
        assert_eq!(
            m3u(text, Some(Path::new("/lists"))),
            [
                PathBuf::from("/abs/one.mp3"),
                PathBuf::from("/lists/sub/two.mp3")
            ]
        );
    }
}
