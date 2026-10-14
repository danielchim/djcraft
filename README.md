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
- **Track info:** view a track's details and cover art, edit them (changes are
  written to the file's tags), and look them up on MusicBrainz or the iTunes
  Store.
- **Decks:** two decks with scrolling waveforms, tempo control and key lock.
  Tracks become playable within a second or two while the rest decodes, and
  waveforms are cached between runs.
- **Track browser:** the library and playlists sit under the decks; double-click
  a track to load it.
- **Tempo analysis:** tracks are analysed in the background for BPM and beat
  positions. It assumes a steady tempo, as most dance music has.
- **Settings:** theme, start-up tab, start-up scanning and lookup provider.

The Radio tab, live broadcasting, remote sessions, effects, the mixer, key and
energy analysis and plugins are planned but not built.

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
| Waveform cache | `~/Library/Caches/djcraft` |

Set `DJCRAFT_DATA_DIR` to use a different library, for example while testing.
Your music files are only changed when you press Save in the track info panel.

## Project layout

| Crate | Purpose |
|---|---|
| `crates/protocol` | The control protocol: every user action and the session state |
| `crates/engine` | Audio engine: decoding, decks, mixing, time-stretch. No UI code |
| `crates/analysis` | Signal analysis on decoded audio: tempo and beat positions |
| `crates/library` | Library database, scanning, tags, playlists, import, lookups |
| `crates/app` | The desktop application |

## Licence

MIT; see [LICENSE](LICENSE). Dependencies are checked against an allow-list
with `cargo deny check licenses`, and nothing under the GPL is linked in.
