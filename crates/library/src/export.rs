//! Writing playlists for other DJ software.

use std::path::Path;

use crate::{Error, Library, Result, Track, TrackQuery};

impl Library {
    /// Writes a playlist to a file other software can import. The format
    /// follows the file's extension: `.xml` for a rekordbox collection,
    /// `.m3u` or `.m3u8` for an M3U playlist. Returns the number of tracks
    /// written.
    pub fn export_playlist(&self, playlist: i64, file: &Path) -> Result<usize> {
        let name: String = self.conn.query_row(
            "SELECT name FROM playlists WHERE id = ?1",
            [playlist],
            |row| row.get(0),
        )?;
        let tracks = self.tracks(&TrackQuery {
            playlist: Some(playlist),
            ..Default::default()
        })?;
        let extension = file
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let text = match extension.as_str() {
            "xml" => rekordbox_xml(&name, &tracks),
            "m3u" | "m3u8" => m3u(&tracks),
            _ => {
                return Err(Error::Import(format!(
                    "{} must end in .m3u8 or .xml to be exported",
                    file.display()
                )));
            }
        };
        std::fs::write(file, text)?;
        Ok(tracks.len())
    }
}

/// Extended M3U: a length and a display name before each path.
fn m3u(tracks: &[Track]) -> String {
    let mut text = String::from("#EXTM3U\n");
    for track in tracks {
        let label = match track.artist.as_str() {
            "" => track.title.clone(),
            artist => format!("{artist} - {}", track.title),
        };
        text.push_str(&format!(
            "#EXTINF:{},{label}\n{}\n",
            track.duration_seconds.round() as i64,
            track.path.display()
        ));
    }
    text
}

/// The XML rekordbox imports: a collection of the tracks, and one playlist
/// listing them in order.
fn rekordbox_xml(name: &str, tracks: &[Track]) -> String {
    let mut text = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<DJ_PLAYLISTS Version=\"1.0.0\">\n",
    );
    text.push_str(&format!(
        "  <PRODUCT Name=\"djcraft\" Version=\"{}\" Company=\"\"/>\n",
        env!("CARGO_PKG_VERSION")
    ));
    text.push_str(&format!("  <COLLECTION Entries=\"{}\">\n", tracks.len()));
    for (index, track) in tracks.iter().enumerate() {
        text.push_str(&format!(
            "    <TRACK TrackID=\"{}\" Name=\"{}\" Artist=\"{}\" Album=\"{}\" Genre=\"{}\" \
             TotalTime=\"{}\"",
            index + 1,
            escape(&track.title),
            escape(&track.artist),
            escape(&track.album),
            escape(&track.genre),
            track.duration_seconds.round() as i64,
        ));
        if let Some(bpm) = track.bpm {
            text.push_str(&format!(" AverageBpm=\"{bpm:.2}\""));
        }
        if let Some(key) = track.camelot {
            text.push_str(&format!(" Tonality=\"{key}\""));
        } else if let Some(key) = &track.key {
            text.push_str(&format!(" Tonality=\"{}\"", escape(key)));
        }
        text.push_str(&format!(
            " Location=\"{}\"/>\n",
            escape(&file_url(&track.path))
        ));
    }
    text.push_str(
        "  </COLLECTION>\n  <PLAYLISTS>\n    <NODE Type=\"0\" Name=\"ROOT\" Count=\"1\">\n",
    );
    text.push_str(&format!(
        "      <NODE Name=\"{}\" Type=\"1\" KeyType=\"0\" Entries=\"{}\">\n",
        escape(name),
        tracks.len()
    ));
    for index in 0..tracks.len() {
        text.push_str(&format!("        <TRACK Key=\"{}\"/>\n", index + 1));
    }
    text.push_str("      </NODE>\n    </NODE>\n  </PLAYLISTS>\n</DJ_PLAYLISTS>\n");
    text
}

/// Text made safe to sit inside an XML attribute.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A path as rekordbox writes locations: `file://localhost/...`, with
/// everything but plain letters, digits and a few marks percent-encoded.
fn file_url(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from("file://localhost");
    if !path.starts_with('/') {
        url.push('/');
    }
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                url.push(byte as char)
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}
