# Working on djcraft

djcraft is an open-source DJ application in Rust with a GPUI interface. Read
`README.md` for what it does and `docs/FEATURES.md` for the plan.

## Commands

```sh
cargo build                                  # whole workspace
cargo test --workspace
cargo fmt
cargo clippy --workspace --all-targets
cargo deny check licenses sources            # needs cargo-deny installed
cargo run -p djcraft-library --example scan -- <library.db> <folder>
cargo run -p djcraft-library --example lookup -- <artist> <title>
cargo run -p djcraft-analysis --example tempo -- <audio file>...
```

Run the tests, `cargo fmt` and the licence check before finishing a change.
Three `chunks_exact` lints in `crates/engine/src/mixer.rs` are known and left
alone.

## Layout

- `crates/protocol`: control protocol types. Pure data, no I/O.
- `crates/engine`: audio engine. Must not depend on any UI crate.
- `crates/analysis`: DSP on decoded samples (tempo, key, energy and cue
  points). No file or database access. The app decodes, analyses and stores.
- `crates/library`: SQLite library, scanning, tags, playlists, import, lookups.
- `crates/app`: the GPUI application. `shell.rs` is the window, `library_view.rs`
  the Library tab and the track browser, `flow_view.rs` the Flow timeline editor, `settings_view.rs` the settings dialog.

## Rules

- **Licence.** The project is MIT. Do not add a dependency under the GPL or
  AGPL. New licences must be added to `deny.toml` deliberately, with a reason.
- **Audio thread.** Code in `crates/engine/src/mixer.rs` runs on the audio
  thread: no allocation, locks, file access or logging. Commands arrive on a
  lock-free queue, state leaves through atomics, and memory is freed on the
  control side.
- **Actions go through the protocol.** Anything a user can do to a deck or the
  mixer is an `Action` in `crates/protocol`, applied with `Engine::apply` under
  a `Role`. The desktop UI is one client of that protocol, not a special case.
- **Schema changes are migrations.** The library database has a version
  (`PRAGMA user_version`). Add a new step in `Library::init`. Never edit an
  existing one.
- **The user's files.** Scanning and importing only read. Music files are
  written only in `crates/library/src/tags.rs`. The app plans each change
  and shows it to the user for review. Then it calls
  `Library::apply_changes`, which records the old values for undo.
- **Keep `docs/FEATURES.md` current.** Mark items `[~]` or `[x]` as they land,
  and say in brackets what is still missing.
- **Tests.** Engine and library behaviour is tested without an audio device
  or a window (`Engine::headless`, `Library::open_in_memory`). Add tests there
  for new behaviour. The app crate has almost none.

## Things that are easy to get wrong

- `gpui-kit` re-exports GPUI as `gpui_kit::*`, the widgets as
  `gpui_kit::component` and the icons as `gpui_kit::assets`. The app must be
  started with `AllAssets` or most icons will not draw.
- GPUI is pre-1.0. Versions are pinned, and the Rust toolchain is pinned in
  `rust-toolchain.toml` because GPUI needs a recent compiler.
- Dependencies and the engine crate are compiled optimised even in debug
  builds (see `Cargo.toml`). Decoding and DSP are unusably slow otherwise.
- When you run the app for testing, set `DJCRAFT_DATA_DIR`. The app then
  does not touch the real library.
- The app's two library views (the Library tab and the browser under the
  decks) each hold their own database connection. Each view refreshes at the
  time it comes into view.
