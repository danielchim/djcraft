# djcraft

An open-source DJ application written in Rust: a music library manager, a live
mixing engine, in-app radio for listening to other DJs' live mixes, and remote
back-to-back sessions. The interface is built with [GPUI](https://www.gpui.rs)
through Longbridge's `gpui-kit`.

djcraft is at an early stage. The library and a basic two-deck player work;
most of the mixing features do not exist yet. [docs/FEATURES.md](docs/FEATURES.md)
is the full plan, with the status of every item.

## What works today

- **Library:** add folders, scan them in the background, and browse every
  track in a searchable, sortable table. Folders are watched, so files you
  add, change or remove show up while the app is open. Tags are read from MP3, FLAC, WAV,
  AIFF, M4A, OGG and Opus files.
- **Playlists:** create playlists that mix tracks from any folder, and import
  them from a rekordbox XML export, a VirtualDJ folder, or M3U files.
- **Track info:** view a track's details and cover art, edit them, and look
  them up on MusicBrainz, the iTunes Store or Discogs, or by their sound
  with AcoustID. The lookup opens a popup that lists the matches, best
  first, and lets you choose which details to copy.
- **Tag tools:** write the analysed BPM, key and energy into the tags, find
  and replace, remove extra spaces, fix capitalisation, and rename files
  from their tags. Every change to a file, including Save in the info panel,
  is shown for review first, and any batch can be undone.
- **Decks:** two decks with tempo control and key lock. Each deck shows a
  scrolling waveform with the beat grid and bar lines, and a whole-track
  waveform that you click to jump. Tracks become playable within a second
  or two while the rest decodes.
- **Track browser:** the library and playlists sit under the decks; double-click
  a track to load it.
- **Analysis:** tracks are analysed in the background for BPM, beats, bars,
  key, energy and cue points, and their waveforms are made ahead of time.
  It assumes a steady tempo, four beats to the bar and a single key, as most
  dance music has. You choose the tempo range, how many decimals tempos
  show, and whether the tempo and key in a file's tags are kept over the
  measured ones.
- **Settings:** theme, start-up tab, start-up scanning, folder watching,
  analysis, where key and energy are written, the lookup provider, and your
  own AcoustID key and Discogs token.

The Radio tab, live broadcasting, remote sessions, effects, the mixer and plugins are planned but not built.

## Building

You need [Rust](https://rustup.rs). The toolchain version is pinned in
`rust-toolchain.toml` and is installed automatically on first build.

```sh
cargo run                       # start the app
cargo run -- one.mp3 two.mp3    # start with tracks on decks 1 and 2
cargo test --workspace
```

djcraft has only been built and run on macOS so far. On Linux, GPUI and the
audio layer need the development packages listed in
[.github/workflows/ci.yml](.github/workflows/ci.yml).

## Keyboard shortcuts

| Shortcut | Action |
|---|---|
| Cmd+1, Cmd+2, Cmd+3 | Library, Live, Radio |
| Cmd+, | Settings |
| Esc | Close settings |
| Cmd+Q | Quit |

## Where your data lives

| What | macOS location |
|---|---|
| Library database and settings | `~/Library/Application Support/djcraft` |
| Waveforms | `~/Library/Application Support/djcraft/waveforms` |

Set `DJCRAFT_DATA_DIR` to use a different library, for example while testing.
Your music files are only changed when you press Save in the track info panel.

## Project layout

| Crate | Purpose |
|---|---|
| `crates/protocol` | The control protocol: every user action and the session state |
| `crates/engine` | Audio engine: decoding, decks, mixing, time-stretch. No UI code |
| `crates/analysis` | Signal analysis on decoded audio: tempo, key, energy and cue points |
| `crates/library` | Library database, scanning, tags, playlists, import, lookups |
| `crates/app` | The desktop application |

## Licence

MIT; see [LICENSE](LICENSE). Dependencies are checked against an allow-list
with `cargo deny check licenses`, and nothing under the GPL is linked in.
