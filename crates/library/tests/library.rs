use std::path::{Path, PathBuf};

use djcraft_library::{
    Camelot, ClipPlacement, CuePoint, ImportSummary, Library, ScanSummary, SourceKind, TempoResult,
    TrackEdit, TrackQuery, read_artwork, read_playlists,
};
use lofty::config::WriteOptions;
use lofty::tag::{Accessor, ItemKey, Tag, TagExt, TagType};

/// A fresh, empty folder that is deleted when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("djcraft-lib-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes one second of silence as a 16-bit mono WAV file.
fn write_wav(path: &Path) {
    write_wav_of(path, 1);
}

fn write_wav_of(path: &Path, seconds: usize) {
    let rate = 8000u32;
    let data = vec![0u8; rate as usize * 2 * seconds];
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&data);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn write_tagged(path: &Path, title: &str, artist: &str, genre: &str, bpm: &str, key: &str) {
    write_wav(path);
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title(title.into());
    tag.set_artist(artist.into());
    tag.set_genre(genre.into());
    // ID3v2 keeps tempo in TBPM, which DJ software also uses for decimals.
    tag.insert_text(ItemKey::IntegerBpm, bpm.into());
    tag.insert_text(ItemKey::InitialKey, key.into());
    tag.save_to_path(path, WriteOptions::default()).unwrap();
}

fn titles(library: &Library, search: &str) -> Vec<String> {
    let query = TrackQuery {
        search: search.into(),
        ..Default::default()
    };
    library
        .tracks(&query)
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect()
}

#[test]
fn scans_a_folder_and_reads_tags() {
    let dir = TempDir::new("tags");
    write_tagged(&dir.0.join("a.wav"), "Opening", "Zed", "House", "124", "8A");
    write_tagged(
        &dir.0.join("sub/b.wav"),
        "Closer",
        "Abel",
        "Techno",
        "132.5",
        "Am",
    );
    write_wav(&dir.0.join("sub/untagged track.wav"));
    write_wav(&dir.0.join(".hidden.wav"));
    std::fs::write(dir.0.join("notes.txt"), "not audio").unwrap();

    let mut library = Library::open_in_memory().unwrap();
    let source = library.add_folder(&dir.0).unwrap();
    let mut last = None;
    let summary = library
        .scan(source.id, |progress| last = Some(progress))
        .unwrap();
    assert_eq!(
        summary,
        ScanSummary {
            added: 3,
            ..Default::default()
        }
    );
    assert_eq!(last.map(|p| (p.done, p.total)), Some((3, 3)));

    // Ordered by artist, then title; the untagged file has no artist.
    let tracks = library.tracks(&TrackQuery::default()).unwrap();
    let titles: Vec<_> = tracks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["untagged track", "Closer", "Opening"]);

    let closer = &tracks[1];
    assert_eq!(closer.artist, "Abel");
    assert_eq!(closer.genre, "Techno");
    assert_eq!(closer.bpm, Some(132.5));
    assert_eq!(closer.key.as_deref(), Some("Am"));
    assert!((closer.duration_seconds - 1.0).abs() < 0.05);
    assert_eq!(closer.path, dir.0.canonicalize().unwrap().join("sub/b.wav"));
    assert_eq!(tracks[0].bpm, None);
}

#[test]
fn rescan_picks_up_changes_only() {
    let dir = TempDir::new("rescan");
    write_tagged(&dir.0.join("a.wav"), "One", "A", "", "120", "1A");
    write_tagged(&dir.0.join("b.wav"), "Two", "B", "", "120", "1A");
    let mut library = Library::open_in_memory().unwrap();
    let source = library.add_folder(&dir.0).unwrap();
    library.scan(source.id, |_| {}).unwrap();
    let id_of_two = library.tracks(&TrackQuery::default()).unwrap()[1].id;

    let unchanged = library.scan(source.id, |_| {}).unwrap();
    assert_eq!(
        unchanged,
        ScanSummary {
            unchanged: 2,
            ..Default::default()
        }
    );

    std::fs::remove_file(dir.0.join("a.wav")).unwrap();
    write_tagged(&dir.0.join("b.wav"), "Two (Remix)", "B", "", "121", "1A");
    write_wav(&dir.0.join("c.wav"));
    let summary = library.scan(source.id, |_| {}).unwrap();
    assert_eq!(
        summary,
        ScanSummary {
            added: 1,
            updated: 1,
            unchanged: 0,
            removed: 1
        }
    );
    assert_eq!(titles(&library, ""), ["c", "Two (Remix)"]);
    // A re-read file keeps its identity, so playlists can keep pointing at it.
    assert_eq!(
        library.tracks(&TrackQuery::default()).unwrap()[1].id,
        id_of_two
    );
}

#[test]
fn a_missing_folder_does_not_empty_the_library() {
    let dir = TempDir::new("unplugged");
    write_wav(&dir.0.join("a.wav"));
    let mut library = Library::open_in_memory().unwrap();
    let source = library.add_folder(&dir.0).unwrap();
    library.scan(source.id, |_| {}).unwrap();

    std::fs::remove_dir_all(&dir.0).unwrap();
    assert!(library.scan(source.id, |_| {}).is_err());
    assert_eq!(titles(&library, ""), ["a"]);
}

#[test]
fn searches_and_filters_by_source() {
    let first = TempDir::new("search-1");
    let second = TempDir::new("search-2");
    write_tagged(
        &first.0.join("a.wav"),
        "Night Drive",
        "Kavinsky",
        "Synthwave",
        "100",
        "5A",
    );
    write_tagged(
        &first.0.join("b.wav"),
        "100% Pure",
        "Kavinsky",
        "House",
        "124",
        "6A",
    );
    write_tagged(
        &second.0.join("c.wav"),
        "Drive Me Home",
        "Other",
        "House",
        "126",
        "7A",
    );

    let mut library = Library::open_in_memory().unwrap();
    let one = library.add_folder(&first.0).unwrap();
    let two = library.add_folder(&second.0).unwrap();
    assert_eq!(library.add_folder(&first.0).unwrap(), one);
    library.scan(one.id, |_| {}).unwrap();
    library.scan(two.id, |_| {}).unwrap();

    assert_eq!(titles(&library, "drive"), ["Night Drive", "Drive Me Home"]);
    assert_eq!(titles(&library, "kavinsky house"), ["100% Pure"]);
    // Wildcard characters are matched literally.
    assert_eq!(titles(&library, "100%"), ["100% Pure"]);
    assert_eq!(titles(&library, "_"), Vec::<String>::new());

    let only_second = TrackQuery {
        source: Some(two.id),
        ..Default::default()
    };
    assert_eq!(library.tracks(&only_second).unwrap().len(), 1);

    library.remove_source(one.id).unwrap();
    assert_eq!(titles(&library, ""), ["Drive Me Home"]);
    assert_eq!(library.sources().unwrap(), [two]);
}

#[test]
fn the_database_persists_and_rejects_newer_schemas() {
    let dir = TempDir::new("persist");
    write_wav(&dir.0.join("music/a.wav"));
    let db = dir.0.join("db/library.db");
    {
        let mut library = Library::open(&db).unwrap();
        let source = library.add_folder(&dir.0.join("music")).unwrap();
        library.scan(source.id, |_| {}).unwrap();
    }
    assert_eq!(titles(&Library::open(&db).unwrap(), ""), ["a"]);

    let raw = rusqlite::Connection::open(&db).unwrap();
    raw.pragma_update(None, "user_version", 99).unwrap();
    drop(raw);
    assert!(Library::open(&db).is_err());
}

#[test]
fn playlists_hold_tracks_from_several_sources_in_order() {
    let first = TempDir::new("pl-1");
    let second = TempDir::new("pl-2");
    write_tagged(&first.0.join("a.wav"), "Alpha", "A", "", "120", "1A");
    write_tagged(&first.0.join("b.wav"), "Beta", "B", "", "120", "1A");
    write_tagged(&second.0.join("c.wav"), "Gamma", "C", "", "120", "1A");

    let mut library = Library::open_in_memory().unwrap();
    let one = library.add_folder(&first.0).unwrap();
    let two = library.add_folder(&second.0).unwrap();
    library.scan(one.id, |_| {}).unwrap();
    library.scan(two.id, |_| {}).unwrap();
    let id = |title: &str| {
        let all = library.tracks(&TrackQuery::default()).unwrap();
        all.iter().find(|t| t.title == title).unwrap().id
    };
    let (alpha, beta, gamma) = (id("Alpha"), id("Beta"), id("Gamma"));

    let warmup = library.create_playlist("Warm up").unwrap();
    let peak = library.create_playlist("Peak").unwrap();
    assert_eq!(
        library.add_to_playlist(warmup.id, &[gamma, alpha]).unwrap(),
        2
    );
    // Adding a track twice keeps one copy, in its original place.
    assert_eq!(
        library.add_to_playlist(warmup.id, &[gamma, beta]).unwrap(),
        1
    );
    library.add_to_playlist(peak.id, &[beta]).unwrap();

    let in_playlist = |library: &Library, playlist: i64, search: &str| -> Vec<String> {
        let query = TrackQuery {
            playlist: Some(playlist),
            search: search.into(),
            ..Default::default()
        };
        library
            .tracks(&query)
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect()
    };
    assert_eq!(
        in_playlist(&library, warmup.id, ""),
        ["Gamma", "Alpha", "Beta"]
    );
    assert_eq!(in_playlist(&library, warmup.id, "alpha"), ["Alpha"]);
    assert_eq!(in_playlist(&library, peak.id, ""), ["Beta"]);

    let counts: Vec<_> = library
        .playlists()
        .unwrap()
        .into_iter()
        .map(|p| (p.name, p.track_count))
        .collect();
    assert_eq!(
        counts,
        [("Warm up".to_string(), 3), ("Peak".to_string(), 1)]
    );

    library.remove_from_playlist(warmup.id, alpha).unwrap();
    library.rename_playlist(warmup.id, "Opening").unwrap();
    assert_eq!(in_playlist(&library, warmup.id, ""), ["Gamma", "Beta"]);
    assert_eq!(library.playlists().unwrap()[0].name, "Opening");

    // Removing a source takes its tracks out of every playlist.
    library.remove_source(one.id).unwrap();
    assert_eq!(in_playlist(&library, warmup.id, ""), ["Gamma"]);
    assert_eq!(in_playlist(&library, peak.id, ""), Vec::<String>::new());

    // Deleting a playlist leaves the tracks in the library.
    library.delete_playlist(warmup.id).unwrap();
    assert_eq!(titles(&library, ""), ["Gamma"]);
    assert_eq!(library.playlists().unwrap().len(), 1);
}

#[test]
fn an_existing_library_gains_playlists_on_upgrade() {
    let dir = TempDir::new("upgrade");
    let db = dir.0.join("library.db");
    drop(Library::open(&db).unwrap());
    // Put the file back to how the first schema left it.
    let raw = rusqlite::Connection::open(&db).unwrap();
    raw.execute_batch(
        "DROP TABLE playlist_tracks; DROP TABLE playlists;
         ALTER TABLE tracks DROP COLUMN cues_manual;
         DROP TABLE cues; ALTER TABLE tracks DROP COLUMN energy;
         DROP TABLE flow_clips; DROP TABLE flows;
         ALTER TABLE tracks DROP COLUMN tempo_bpm;
         ALTER TABLE tracks DROP COLUMN tempo_first_beat;
         ALTER TABLE tracks DROP COLUMN tempo_confidence;
         ALTER TABLE tracks DROP COLUMN tempo_version;
         ALTER TABLE tracks DROP COLUMN bpm_manual;
         PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(raw);

    let library = Library::open(&db).unwrap();
    assert_eq!(library.create_playlist("New").unwrap().name, "New");
    assert!(library.pending_tempo_analysis(1).unwrap().is_empty());
}

fn file_url(path: &Path) -> String {
    format!(
        "file://localhost{}",
        path.to_string_lossy().replace(' ', "%20")
    )
}

#[test]
fn imports_rekordbox_xml_playlists() {
    let music = TempDir::new("rb-music");
    let elsewhere = TempDir::new("rb-elsewhere");
    write_tagged(
        &music.0.join("in folder.wav"),
        "In Folder",
        "A",
        "",
        "120",
        "1A",
    );
    write_tagged(
        &elsewhere.0.join("loose.wav"),
        "Loose",
        "B",
        "",
        "120",
        "1A",
    );
    let (music_dir, elsewhere_dir) = (
        music.0.canonicalize().unwrap(),
        elsewhere.0.canonicalize().unwrap(),
    );
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<DJ_PLAYLISTS Version="1.0.0">
  <PRODUCT Name="rekordbox" Version="6.8.5" Company="AlphaTheta"/>
  <COLLECTION Entries="3">
    <TRACK TrackID="1" Name="In Folder" Location="{}"/>
    <TRACK TrackID="2" Name="Loose" Location="{}"/>
    <TRACK TrackID="3" Name="Gone" Location="file://localhost/nowhere/gone.mp3"/>
  </COLLECTION>
  <PLAYLISTS>
    <NODE Type="0" Name="ROOT" Count="2">
      <NODE Name="Club" Type="0" Count="1">
        <NODE Name="Friday" Type="1" KeyType="0" Entries="3">
          <TRACK Key="2"/><TRACK Key="1"/><TRACK Key="3"/>
        </NODE>
      </NODE>
      <NODE Name="By location" Type="1" KeyType="1" Entries="1">
        <TRACK Key="{}"/>
      </NODE>
    </NODE>
  </PLAYLISTS>
</DJ_PLAYLISTS>"#,
        file_url(&music_dir.join("in folder.wav")),
        file_url(&elsewhere_dir.join("loose.wav")),
        file_url(&elsewhere_dir.join("loose.wav")),
    );
    let xml_path = elsewhere.0.join("collection.xml");
    std::fs::write(&xml_path, xml).unwrap();

    let mut library = Library::open_in_memory().unwrap();
    let folder = library.add_folder(&music.0).unwrap();
    library.scan(folder.id, |_| {}).unwrap();

    let playlists = read_playlists(&xml_path).unwrap();
    let names: Vec<_> = playlists.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Club / Friday", "By location"]);

    let summary = library.import_playlists(&playlists).unwrap();
    assert_eq!(
        summary,
        ImportSummary {
            playlists: 2,
            tracks: 3,
            missing: 1
        }
    );

    let friday = library.playlists().unwrap()[0].id;
    let query = TrackQuery {
        playlist: Some(friday),
        ..Default::default()
    };
    let tracks = library.tracks(&query).unwrap();
    let titles: Vec<_> = tracks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["Loose", "In Folder"]);
    // The track already in a library folder is reused, not duplicated; the
    // other one lives under the loose-files source and can still be found.
    assert_eq!(tracks[1].source_id, folder.id);
    assert_eq!(tracks[1].source_name, folder.name);
    assert_eq!(tracks[0].source_name, "Imported files");
    assert_eq!(tracks[0].path, elsewhere_dir.join("loose.wav"));
    assert_eq!(library.tracks(&TrackQuery::default()).unwrap().len(), 2);
    let kinds: Vec<_> = library
        .sources()
        .unwrap()
        .into_iter()
        .map(|s| s.kind)
        .collect();
    assert!(kinds.contains(&SourceKind::Files));

    // Importing again changes nothing.
    let again = library.import_playlists(&playlists).unwrap();
    assert_eq!(
        again,
        ImportSummary {
            playlists: 2,
            tracks: 0,
            missing: 1
        }
    );
    assert_eq!(library.playlists().unwrap().len(), 2);

    // Loose files are kept up to date like any other source.
    let loose = library.tracks(&query).unwrap()[0].source_id;
    let scan = library.scan(loose, |_| {}).unwrap();
    assert_eq!(
        scan,
        ScanSummary {
            unchanged: 1,
            ..Default::default()
        }
    );
    std::fs::remove_file(elsewhere_dir.join("loose.wav")).unwrap();
    assert_eq!(library.scan(loose, |_| {}).unwrap().removed, 1);
}

#[test]
fn imports_a_virtualdj_folder() {
    let home = TempDir::new("vdj-home");
    let music = TempDir::new("vdj-music");
    write_tagged(&music.0.join("one.wav"), "One", "A", "", "120", "1A");
    write_tagged(&music.0.join("two & co.wav"), "Two", "B", "", "120", "1A");
    let dir = music.0.canonicalize().unwrap();

    std::fs::create_dir_all(home.0.join("MyLists/Weddings")).unwrap();
    std::fs::create_dir_all(home.0.join("Playlists")).unwrap();
    std::fs::create_dir_all(home.0.join("History")).unwrap();
    std::fs::write(
        home.0.join("MyLists/Weddings/First dance.vdjfolder"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<VirtualFolder noDuplicates="no" ordered="yes">
  <song path="{}" size="1" songlength="1.0" idx="0" />
  <song path="{}" size="1" songlength="1.0" idx="1" />
</VirtualFolder>"#,
            dir.join("two &amp; co.wav").display(),
            dir.join("one.wav").display(),
        ),
    )
    .unwrap();
    std::fs::write(
        home.0.join("Playlists/Saturday.m3u"),
        format!(
            "#EXTVDJ:<title>One</title>\n{}\n",
            dir.join("one.wav").display()
        ),
    )
    .unwrap();
    std::fs::write(home.0.join("History/2026-10-01.m3u"), "ignored.mp3\n").unwrap();

    let playlists = read_playlists(&home.0).unwrap();
    let names: Vec<_> = playlists.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Saturday", "Weddings / First dance"]);

    let mut library = Library::open_in_memory().unwrap();
    let summary = library.import_playlists(&playlists).unwrap();
    assert_eq!(
        summary,
        ImportSummary {
            playlists: 2,
            tracks: 3,
            missing: 0
        }
    );
    let dance = library.playlists().unwrap()[1].id;
    let query = TrackQuery {
        playlist: Some(dance),
        ..Default::default()
    };
    let titles: Vec<_> = library
        .tracks(&query)
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
    assert_eq!(titles, ["Two", "One"]);

    // A folder that is not VirtualDJ's, and a file that is not a playlist.
    assert!(read_playlists(&music.0).is_err());
    assert!(read_playlists(&dir.join("one.wav")).is_err());
    std::fs::write(music.0.join("other.xml"), "<Something/>").unwrap();
    assert!(read_playlists(&music.0.join("other.xml")).is_err());
}

#[test]
fn editing_a_track_writes_its_tags() {
    let dir = TempDir::new("edit");
    write_tagged(
        &dir.0.join("tagged.wav"),
        "Old",
        "Someone",
        "House",
        "120",
        "1A",
    );
    write_wav(&dir.0.join("bare.wav"));
    let mut library = Library::open_in_memory().unwrap();
    let source = library.add_folder(&dir.0).unwrap();
    library.scan(source.id, |_| {}).unwrap();
    let find = |library: &Library, title: &str| {
        let all = library.tracks(&TrackQuery::default()).unwrap();
        all.into_iter().find(|t| t.title == title).unwrap()
    };

    let edit = TrackEdit {
        title: "New Title".into(),
        artist: "  New Artist ".into(),
        album: "The Album".into(),
        genre: String::new(),
        bpm: Some(127.5),
        key: Some("8A".into()),
    };
    let tagged = find(&library, "Old");
    library.edit_track(tagged.id, &edit).unwrap();
    let edited = find(&library, "New Title");
    assert_eq!(edited.id, tagged.id);
    assert_eq!(edited.artist, "New Artist");
    assert_eq!(edited.album, "The Album");
    assert_eq!(edited.genre, "");
    assert_eq!(edited.bpm, Some(127.5));
    assert_eq!(edited.key.as_deref(), Some("8A"));

    // A file with no tags at all gains them.
    let bare = find(&library, "bare");
    library
        .edit_track(
            bare.id,
            &TrackEdit {
                bpm: None,
                key: None,
                ..edit.clone()
            },
        )
        .unwrap();

    // The files themselves changed, so a library built from scratch agrees,
    // and the next scan of this one has nothing to do.
    let scan = library.scan(source.id, |_| {}).unwrap();
    assert_eq!(
        scan,
        ScanSummary {
            unchanged: 2,
            ..Default::default()
        }
    );
    let mut fresh = Library::open_in_memory().unwrap();
    let source = fresh.add_folder(&dir.0).unwrap();
    fresh.scan(source.id, |_| {}).unwrap();
    let tracks = fresh.tracks(&TrackQuery::default()).unwrap();
    assert!(
        tracks
            .iter()
            .all(|t| t.title == "New Title" && t.artist == "New Artist")
    );
    let bpms: Vec<_> = tracks.iter().map(|t| t.bpm).collect();
    assert!(bpms.contains(&Some(127.5)) && bpms.contains(&None));

    assert!(read_artwork(&dir.0.join("bare.wav")).is_none());
}

#[test]
fn measured_tempo_is_stored_and_shown() {
    let dir = TempDir::new("tempo");
    write_tagged(&dir.0.join("a.wav"), "Tagged", "A", "", "100", "1A");
    write_wav(&dir.0.join("b.wav"));
    let mut library = Library::open_in_memory().unwrap();
    let source = library.add_folder(&dir.0).unwrap();
    library.scan(source.id, |_| {}).unwrap();
    let bpm_of = |library: &Library, title: &str| {
        let all = library.tracks(&TrackQuery::default()).unwrap();
        all.into_iter().find(|t| t.title == title).unwrap().bpm
    };

    let pending = library.pending_tempo_analysis(1).unwrap();
    assert_eq!(pending.len(), 2);
    let id_of = |name: &str| {
        pending
            .iter()
            .find(|(_, path)| path.ends_with(name))
            .unwrap()
            .0
    };
    let (tagged, bare) = (id_of("a.wav"), id_of("b.wav"));

    // A confident measurement replaces the tag's tempo in listings.
    let sure = TempoResult {
        bpm: 128.02,
        first_beat_seconds: 0.25,
        confidence: 0.4,
    };
    library.store_tempo(tagged, 1, Some(sure)).unwrap();
    assert_eq!(bpm_of(&library, "Tagged"), Some(128.02));
    assert_eq!(library.beat_grid(tagged).unwrap(), Some(sure));

    // A doubtful one, or none at all, leaves the track as it was.
    let unsure = TempoResult {
        bpm: 87.0,
        first_beat_seconds: 0.0,
        confidence: 0.03,
    };
    library.store_tempo(bare, 1, Some(unsure)).unwrap();
    assert_eq!(bpm_of(&library, "b"), None);
    assert_eq!(library.beat_grid(bare).unwrap(), None);

    // Analysed tracks are not offered again, until the analysis improves.
    assert!(library.pending_tempo_analysis(1).unwrap().is_empty());
    assert_eq!(library.pending_tempo_analysis(2).unwrap().len(), 2);

    // Editing other details keeps the measurement and needs no re-analysis.
    let mut edit = TrackEdit {
        title: "Renamed".into(),
        artist: "A".into(),
        album: String::new(),
        genre: String::new(),
        bpm: Some(128.02),
        key: None,
    };
    library.edit_track(tagged, &edit).unwrap();
    assert_eq!(bpm_of(&library, "Renamed"), Some(128.02));
    assert!(library.pending_tempo_analysis(1).unwrap().is_empty());

    // A tempo typed in by hand wins over the measurement from then on.
    edit.bpm = Some(64.0);
    library.edit_track(tagged, &edit).unwrap();
    assert_eq!(bpm_of(&library, "Renamed"), Some(64.0));
    library.store_tempo(tagged, 2, Some(sure)).unwrap();
    assert_eq!(bpm_of(&library, "Renamed"), Some(64.0));

    // A file that changes on disk is analysed again.
    write_tagged(&dir.0.join("b.wav"), "Now Tagged", "B", "", "90", "2A");
    library.scan(source.id, |_| {}).unwrap();
    let pending = library.pending_tempo_analysis(1).unwrap();
    assert_eq!(
        pending.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [bare]
    );
}

#[test]
fn tracks_can_be_counted_and_filtered_by_key() {
    let dir = TempDir::new("keys");
    write_tagged(&dir.0.join("a.wav"), "One", "A", "", "120", "8A");
    write_tagged(&dir.0.join("b.wav"), "Two", "A", "", "120", "Am");
    write_tagged(&dir.0.join("c.wav"), "Three", "A", "", "120", "C");
    write_tagged(&dir.0.join("d.wav"), "Four", "A", "", "120", "F#m");
    write_tagged(&dir.0.join("e.wav"), "Five", "A", "", "120", "nonsense");
    let mut library = Library::open_in_memory().unwrap();
    let source = library.add_folder(&dir.0).unwrap();
    library.scan(source.id, |_| {}).unwrap();

    let key = |text: &str| Camelot::parse(text).unwrap();
    let counts = library.key_counts().unwrap();
    assert_eq!(counts.get(&key("8A")), Some(&2));
    assert_eq!(counts.get(&key("8B")), Some(&1));
    assert_eq!(counts.get(&key("11A")), Some(&1));
    assert_eq!(counts.len(), 3);

    // Everything that mixes with A minor: 8A itself, 7A, 9A and 8B.
    let query = TrackQuery {
        keys: key("8A").compatible().to_vec(),
        ..Default::default()
    };
    let mut titles: Vec<_> = library
        .tracks(&query)
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
    titles.sort();
    assert_eq!(titles, ["One", "Three", "Two"]);

    let all = library.tracks(&TrackQuery::default()).unwrap();
    let five = all.iter().find(|t| t.title == "Five").unwrap();
    assert_eq!(
        (five.key.as_deref(), five.camelot),
        (Some("nonsense"), None)
    );
}

#[test]
fn flows_place_and_trim_clips_on_lanes() {
    let dir = TempDir::new("flow");
    write_tagged(&dir.0.join("a.wav"), "First", "A", "", "120", "8A");
    write_wav_of(&dir.0.join("long.wav"), 10);
    let mut library = Library::open_in_memory().unwrap();
    let source = library.add_folder(&dir.0).unwrap();
    library.scan(source.id, |_| {}).unwrap();
    let all = library.tracks(&TrackQuery::default()).unwrap();
    let id_of = |title: &str| all.iter().find(|t| t.title == title).unwrap().id;
    let (first, second) = (id_of("First"), id_of("long"));
    let grid = TempoResult {
        bpm: 120.0,
        first_beat_seconds: 0.1,
        confidence: 0.5,
    };
    library.store_tempo(first, 1, Some(grid)).unwrap();

    let flow = library.create_flow("Friday").unwrap();
    let one = library.add_clip(flow.id, first, 0, 0.0).unwrap();
    // The same track may be used more than once.
    let two = library.add_clip(flow.id, second, 1, 0.6).unwrap();
    library.add_clip(flow.id, first, 9, -5.0).unwrap();

    let clips = library.flow_clips(flow.id).unwrap();
    assert_eq!(clips.len(), 3);
    // Out-of-range lanes and times are brought back onto the timeline.
    assert_eq!((clips[1].lane, clips[1].start), (3, 0.0));
    let whole = &clips[0];
    assert_eq!((whole.id, whole.lane, whole.offset), (one, 0, 0.0));
    assert!((whole.length - 1.0).abs() < 0.05);
    assert_eq!(whole.track.title, "First");
    assert_eq!(whole.grid, Some(grid));
    assert_eq!(clips[2].grid, None);
    assert_eq!(library.flows().unwrap()[0].clip_count, 3);

    // Cue points and energy belong to the track, and come with its clips.
    let cues = [
        CuePoint {
            seconds: 0.1,
            energy: 3,
        },
        CuePoint {
            seconds: 0.6,
            energy: 9,
        },
    ];
    library.store_structure(first, Some(8), &cues).unwrap();
    library.store_structure(first, Some(7), &cues).unwrap();
    let clips = library.flow_clips(flow.id).unwrap();
    assert_eq!(clips[0].cues, cues);
    assert_eq!(clips[0].track.energy, Some(7));
    assert_eq!((clips[2].cues.len(), clips[2].track.energy), (0, None));

    // Cue points edited by hand are sorted, and survive later analysis.
    let mine = [
        CuePoint {
            seconds: 0.8,
            energy: 6,
        },
        CuePoint {
            seconds: 0.2,
            energy: 4,
        },
    ];
    library.set_cues(first, &mine).unwrap();
    library.store_structure(first, Some(9), &cues).unwrap();
    let seconds: Vec<f64> = library
        .cues(first)
        .unwrap()
        .iter()
        .map(|c| c.seconds)
        .collect();
    assert_eq!(seconds, [0.2, 0.8]);
    assert_eq!(library.track(first).unwrap().unwrap().energy, Some(9));
    library.set_cues(first, &cues).unwrap();

    library.rename_flow(flow.id, "Saturday").unwrap();
    assert_eq!(library.flows().unwrap()[0].name, "Saturday");

    // A clip cannot be trimmed past the ends of its track.
    let placed = |library: &Library, placement| {
        library.place_clip(two, placement).unwrap();
        let clips = library.flow_clips(flow.id).unwrap();
        let clip = clips.into_iter().find(|c| c.id == two).unwrap();
        (clip.lane, clip.start, clip.offset, clip.length)
    };
    let trimmed = ClipPlacement {
        lane: 2,
        start: 30.0,
        offset: 2.5,
        length: 4.0,
    };
    assert_eq!(placed(&library, trimmed), (2, 30.0, 2.5, 4.0));
    let too_long = ClipPlacement {
        length: 99.0,
        ..trimmed
    };
    assert_eq!(placed(&library, too_long), (2, 30.0, 2.5, 7.5));
    let too_late = ClipPlacement {
        offset: 99.0,
        length: 0.0,
        ..trimmed
    };
    assert_eq!(placed(&library, too_late), (2, 30.0, 9.0, 1.0));

    library.remove_clip(one).unwrap();
    assert_eq!(library.flow_clips(flow.id).unwrap().len(), 2);

    // Clips go when their track leaves the library, and with their flow.
    library.remove_source(source.id).unwrap();
    assert!(library.flow_clips(flow.id).unwrap().is_empty());
    library.delete_flow(flow.id).unwrap();
    assert!(library.flows().unwrap().is_empty());
}

#[test]
fn exported_playlists_can_be_read_back() {
    let dir = TempDir::new("export");
    let awkward = r#"One <&> "Two""#;
    write_tagged(
        &dir.0.join("one & two.wav"),
        awkward,
        "A & B",
        "House",
        "124",
        "8A",
    );
    write_tagged(&dir.0.join("café #3.wav"), "Three", "", "", "128", "Am");
    let mut library = Library::open_in_memory().unwrap();
    let source = library.add_folder(&dir.0).unwrap();
    library.scan(source.id, |_| {}).unwrap();
    let all = library.tracks(&TrackQuery::default()).unwrap();
    let id_of = |title: &str| all.iter().find(|t| t.title == title).unwrap().id;
    let name = r#"Set "A" & B"#;
    let playlist = library.create_playlist(name).unwrap();
    library
        .add_to_playlist(playlist.id, &[id_of("Three"), id_of(awkward)])
        .unwrap();
    let expected: Vec<PathBuf> = {
        let query = TrackQuery {
            playlist: Some(playlist.id),
            ..Default::default()
        };
        library
            .tracks(&query)
            .unwrap()
            .into_iter()
            .map(|t| t.path)
            .collect()
    };
    assert_eq!(expected.len(), 2);

    for file in ["set.m3u8", "set.xml"] {
        let file = dir.0.join(file);
        assert_eq!(library.export_playlist(playlist.id, &file).unwrap(), 2);
        let read = read_playlists(&file).unwrap();
        assert_eq!(read.len(), 1, "{}", file.display());
        assert_eq!(read[0].files, expected, "{}", file.display());
    }
    // rekordbox XML carries the playlist's name; an M3U is named by its file.
    assert_eq!(
        read_playlists(&dir.0.join("set.xml")).unwrap()[0].name,
        name
    );
    let xml = std::fs::read_to_string(dir.0.join("set.xml")).unwrap();
    assert!(xml.contains(r#"AverageBpm="128.00""#) && xml.contains(r#"Tonality="8A""#));

    assert!(
        library
            .export_playlist(playlist.id, &dir.0.join("set.txt"))
            .is_err()
    );
}
