# djcraft feature list

djcraft is an open-source DJ application: a music library manager, a live mixing
engine at rekordbox Performance-mode parity, in-app radio for listening to other
DJs' live mixes, and remote collaboration.

Status key: `[ ]` to do, `[~]` in progress, `[x]` done.

## Project decisions

- **Licence:** MIT. No GPL code is linked into the app.
- **Stack:** Rust, with GPUI for the desktop UI (via Longbridge's `gpui-kit`).
- **Plugins:** JavaScript/TypeScript on an embedded QuickJS runtime, sandboxed
  with per-plugin permissions.
- **Interface:** inspired by djay Pro's layout and interaction style.
- **Infrastructure:** bring your own. Sync, streaming and directories run on
  storage and servers the user chooses. No mandatory djcraft-operated service.
- **Architecture:** the audio engine and session logic are UI-free crates. Every
  user action is a message in a documented control protocol, and the desktop UI
  uses the same protocol as remote clients.

## Milestone 0: foundations

- [x] Cargo workspace with separate crates: `engine`, `library`, `protocol`, `app`
- [x] Licence allow-list enforced in CI with `cargo-deny`
- [x] Spike: GPUI window drawing a scrolling waveform while the engine plays a
      time-stretched track
- [~] Confirm `gpui-kit` builds and runs on macOS, Windows and Linux (builds
      and passes the tests on all three in CI; the app has only been run on
      macOS)
- [x] Real-time-safe engine design: no allocation, locking or UI calls on the
      audio thread; lock-free queues between engine and UI
- [x] Deck playback that follows an external speed and position signal,
      including reverse and near-zero speed (needed for jog-wheel scratching
      and DVS)
- [x] Control protocol v1: action messages, state broadcast, roles
      (host, co-DJ, viewer)

## Milestone 1: music library management

### Sources

- [x] Local files and folders, with watch-for-changes (watching has only
      been tried on macOS)
- [ ] Network shares
- [ ] External sources as optional plugins (YouTube and similar through a
      user-installed `yt-dlp`); see Plugins

### Analysis

- [~] BPM and beat grid detection (constant tempo and four beats to the bar
      only; tested on synthetic audio only, so accuracy on real tracks is
      not measured yet)
- [~] Key detection (one key per track, major or minor; tested on
      synthetic music only, so accuracy on real tracks is not measured yet)
- [x] Waveform generation (overview and detail)
- [x] Analysis cache stored alongside the library database
- [x] Tempo range limits (minimum and maximum BPM) and precision setting
- [x] Option to keep an existing tempo or key instead of overwriting it

### Harmonic mixing and energy

Feature set modelled on Mixed In Key 10.

- [~] Camelot wheel notation, with a choice of notation (Camelot, musical,
      Open Key) and optional zero-padding (`08A`) (keys in tags are read in
      any of the three and shown as Camelot, as are detected keys; no choice
      of notation yet)
- [ ] Key segments: key changes within a track, each with a confidence value
- [ ] Tuning detection for tracks not pitched to A440
- [~] Energy level per track on a 1–10 scale (from loudness, so loudly
      mastered tracks crowd the top of the scale)
- [~] Energy segments along the track, drawn on the waveform (each cue point
      carries its section's energy; shown on Flow clips, not on deck
      waveforms)
- [ ] Danceability score
- [~] Automatic cue points (up to 8) placed at energy and structure changes
      (detected and stored; shown and editable in Flow's player; no export
      to other DJ software yet)
- [ ] Cue point export to rekordbox, Serato and Traktor, with an option not to
      overwrite existing cues
- [ ] Audio quality warnings: clipping, low volume, low-bitrate source
- [ ] On-screen piano to check a detected key by ear, playable from MIDI
- [~] Sort and filter by compatible key and by energy for set building (the
      Camelot wheel in the Library's side panel narrows the list to a key
      and the keys that mix with it; nothing by energy yet)

### Tags

- [x] Read and write tags across MP3, FLAC, AAC, WAV, OGG (also Opus and
      AIFF; title, artist, album, genre, BPM, key, comment and grouping are
      tested in each, apart from raw .aac files)
- [~] Audio fingerprint lookup (Chromaprint/AcoustID) (fingerprints are made
      by a pure-Rust Chromaprint port and sent to AcoustID with the user's own
      key; a successful match is untested, as no key was at hand)
- [~] Metadata match from MusicBrainz and Discogs (MusicBrainz, the iTunes
      Store and Discogs, searched from a popup that tidies untidy tags, ranks
      the results and prefers original releases; Discogs needs the user's own
      token and is untested against the live service)
- [x] Review-and-diff screen before any tag is written
- [x] Batch tag alignment with undo (writes the analysed BPM, key and energy
      into the tags of every track in the list; any batch of tag changes can
      be undone)
- [x] Choose where key and energy are written: key field, comment, grouping,
      or as a prefix/suffix on title or artist
- [x] Tag clean-up tools: find and replace across the library, remove extra
      spaces, fix capitalisation
- [x] Rename files from tags using a format template (files stay in their
      folder)

### Flow (set planning)

- [~] Timeline editor with four lanes, matching the decks: place tracks,
      move them between lanes, and trim either end (built; planning only, no
      audio)
- [x] Trim snaps to the track's bars; a moved clip snaps to other clips' edges
- [~] Transition notes where one track comes in over another: tempo change
      and whether the keys mix
- [~] Add tracks by double-click (appended with a typical overlap) or by
      dragging onto a lane
- [ ] Export a flow to a playlist, or load it onto the decks in order
- [~] Player in the editor for one track at a time: whole-track waveform,
      cue point flags, jump between cues, add and delete cues (it plays
      through its own output, not the decks' headphone cue)
- [x] Opens on a new flow; saved flows are picked from a dropdown
- [ ] Preview transitions in the editor (needs sync and crossfade)
- [~] Waveforms and cue points on clips (cue points and energy are drawn,
      and trims snap to cue points; no waveforms)
- [x] Delete or Backspace removes the selected clip
- [~] Stretch a clip to another tempo by dragging its edge, with a Stretch
      toggle (Shift+M) and Alt for the other mode; snaps to whole BPM and to
      the tempo of overlapping clips (planning only: the key is assumed
      locked, and nothing is heard)

### Organisation

- [x] Track browser with virtualised table, sorting, column chooser, search
- [~] Playlists and folders (folders inside folders, rename, move, reorder
      from a track's menu; no drag and drop yet)
- [x] Smart playlists (rule-based) (title, artist, album, genre, BPM,
      energy, year, key or keys that mix, and date added; every rule or any)
- [x] Auto-classification by genre, energy and era (automatic groups in the
      Library sidebar, from tags and analysis)
- [x] Key/BPM compatibility hints for the next track ("Suggest next" in the
      browser under the decks, for the track loaded last)
- [x] Duplicate detection (same artist and title with about the same length,
      or the same file size; copies leave the library, files stay on disk)
- [x] Missing-file relocation (missing tracks are kept and marked; files
      that move within a folder are followed by themselves)

### Interoperability

- [~] Import rekordbox XML (playlists and their tracks; cues, grids, BPM and
      key in the file are not read yet)
- [ ] Import an existing rekordbox library database
- [~] Export rekordbox XML (one playlist at a time, with BPM and key; no
      cue points or beat grids)
- [x] Export M3U playlists
- [ ] Export to USB for Pioneer players
- [~] Import VirtualDJ playlists (`.vdjfolder` and `.m3u`; written from the
      documented format, not yet tried on a real VirtualDJ folder)
- [x] Import M3U playlists
- [ ] Serato and Traktor metadata import (later)

### Sync

- [ ] Library and playlist sync through user-chosen storage (Dropbox, Syncthing,
      S3, WebDAV)
- [ ] Shared playlists using conflict-free merging, no server required
- [ ] No device limit

## Milestone 2: two-deck live mix

### Decks

- [x] Two decks with scrolling and overview waveforms
- [~] Play/pause, cue, jump to start (play and pause only)
- [~] Tempo fader with selectable ranges (slower and faster buttons in 1%
      steps over one fixed range; no fader)
- [x] Master tempo (key lock)
- [ ] Beat sync and quantize
- [ ] Hot cues (8 per deck)
- [ ] Memory cues
- [ ] Manual loop in/out, auto beat loop, loop halve/double
- [ ] Beat jump

### Mixer

- [~] Trim, channel fader, crossfader with selectable curves (the engine
      and protocol have a volume per deck and a master gain; no controls on
      screen, and no trim or crossfader)
- [ ] 3-band EQ
- [ ] Channel filter
- [ ] Headphone cue with cue/master mix
- [ ] Master limiter and level meters
- [ ] Audio device and output routing settings

### Core effects

- [ ] Beat FX: delay, echo, reverb, flanger, phaser, filter
- [ ] Sound Color FX: filter, noise, crush
- [ ] Beat-synced FX time, wet/dry, FX channel assign

### Recording

- [ ] Record the master output to file
- [ ] Tracklist saved with each recording

## Milestone 3: rekordbox parity

### Decks and transport

- [ ] Four decks
- [ ] Slip mode
- [ ] Reverse play
- [ ] Loop move
- [ ] 16 hot cues per deck
- [~] Key shift and key sync (a block in Flow can be shifted by semitones
      for planning; the decks cannot shift key yet)

### Mixer

- [ ] EQ isolator mode
- [ ] Headphone split cue

### Effects

- [ ] Beat FX: ping pong, roll, slip roll, reverse roll, trans, pan, pitch,
      spiral, helix, vinyl brake, stretch
- [ ] Sound Color FX: dub echo, space, sweep, jet, pitch
- [ ] Release FX: echo out, vinyl brake, back spin
- [ ] Multi-FX and single-FX unit modes

### Pads

- [ ] Hot cue
- [ ] Pad FX
- [ ] Beat jump
- [ ] Beat loop
- [ ] Sampler
- [ ] Keyboard
- [ ] Key shift

### Stems and sampler

- [ ] Stem separation (vocals, drums, bass, other), model downloaded on first use
- [ ] Per-stem mute, solo and level
- [ ] Sampler with banks, one-shot and loop modes
- [ ] Vocal position markers on the waveform

### Step sequencer

- [ ] Record sampler pad hits into a pattern, quantised to the beat clock
- [ ] Pattern lengths of 1, 2 and 4 bars, with overdub and per-slot erase
- [ ] Pattern playback synced to the master deck, with per-slot mute
- [ ] Metronome while recording
- [ ] Save and load patterns per sampler bank

### Controllers

- [ ] MIDI input and LED output
- [ ] djcraft mapping format: declarative file plus optional plugin script
- [ ] Built-in mappings for common DDJ controllers
- [ ] MIDI Learn
- [ ] Import mapping files from the user's own rekordbox install

## Milestone 4: radio

### Broadcast

- [ ] "Go live" from the master output
- [ ] Stream to a user-chosen server (Icecast-compatible or self-hosted)
- [ ] Now-playing metadata and artwork
- [ ] Station profile: name, genre, description

### Directory

- [ ] Open directory format that lists links and metadata only, never audio
- [ ] Self-hostable directory server
- [ ] Subscribe to multiple directories in the app
- [ ] Takedown and reporting process for the default directory

### Listen

- [ ] Browse live stations by genre and listener count
- [ ] In-app player with live tracklist
- [ ] Follow DJs and get notified when they go live
- [ ] Auto-DJ mode: play a playlist with automatic transitions when nobody is
      mixing

## Milestone 5: remote collab

- [ ] Session create and join by link or code
- [ ] Peer-to-peer connection over WebRTC, with a self-hostable rendezvous service
- [ ] Host renders audio; guests send control actions through the protocol
- [ ] Low-latency monitor stream of the mix to guests
- [ ] Guests see live deck state: waveforms, positions, cues, mixer
- [ ] Roles and permissions: host, co-DJ, viewer
- [ ] Guest track requests from the host's library
- [ ] Session chat

## Milestone 6: phone and tablet

- [ ] Web client that joins a session by link or QR code
- [ ] Touch layout for deck control, mixer and FX
- [ ] Remote control of your own set (same mechanism, host-owned role)
- [ ] Library browsing and track loading from the device
- [ ] Native apps (later, if the web client is not enough)

## Milestone 7: DVS (timecode vinyl)

The decoder is written for djcraft; GPL decoders such as xwax are not linked.

- [ ] Audio input routing: assign sound card inputs to decks
- [ ] Timecode decoder: speed, direction and position from the control signal
- [ ] Support for common control vinyl and CD formats
- [ ] Absolute, relative and internal (through) modes per deck
- [ ] Calibration screen: signal scope, level, phase and noise threshold
- [ ] Needle-drop handling and lead-in detection
- [ ] Signal-loss fallback to internal playback

## Plugins

Plugins are a cross-cutting feature. The runtime lands with Milestone 1 (music
sources need it first); each later milestone adds its own extension points.

### Runtime

- [ ] Embedded JavaScript runtime built on QuickJS (through the `rquickjs`
      crate; both MIT)
- [ ] TypeScript support: types stripped at install time, with published type
      definitions for the plugin API
- [ ] One isolated context per plugin, with memory and execution-time limits
- [ ] No ambient access: network, files and library data are host functions
      granted per permission
- [ ] Async host API (promises) so plugins never block the UI
- [ ] Plugins run off the audio thread, always

### Packaging

- [ ] Plugin manifest: name, version, API version, permissions, entry point
- [ ] Install from a folder, an archive or a URL
- [ ] Permission prompt on install, with per-plugin enable/disable
- [ ] Versioned plugin API with a compatibility check on load
- [ ] Hot reload for plugin development
- [ ] Plugin log and error view

### Plugin store

- [~] "+" button beside the library's source tabs that opens the plugins page
      (page exists; nothing can be installed yet)
- [ ] Open store index format: a list of plugins with name, version,
      permissions, download link and checksum
- [ ] Self-hostable store; subscribe to more than one store in the app
- [ ] Browse and search plugins by kind (source, metadata, controller, …)
- [ ] Install, update and remove from the store, with the permission prompt
- [ ] Checksum verification on download, and signed listings
- [ ] Source plugins appear as tabs beside Local in the library

### Extension points

- [ ] Music sources: search, browse, resolve to a playable stream or file
- [ ] Metadata and tag providers
- [ ] Library importers and exporters
- [ ] Custom analysis columns and smart-playlist rules
- [ ] Controller mappings as scripts, with access to MIDI in/out and deck state
- [ ] Radio directory providers
- [ ] Commands, menu items and keyboard shortcuts
- [ ] UI panels built from a declarative description that the host renders
      (plugins do not draw directly)

### Other plugin kinds

- [ ] External-process plugins for wrapping existing tools or code under
      licences that cannot be linked (`yt-dlp`, GPL decoders), spoken to over a
      message protocol
- [ ] Audio effect plugins: host the CLAP format; VST3 and Audio Units later.
      JavaScript is not used for audio processing

## Interface direction

The interface is inspired by djay Pro's approach: few visible controls, large
waveforms, and options tucked into popovers.

- [ ] Mode switcher: classic two-turntable view, 2-deck waveform view, 4-deck
      view, automix view, single-deck prep view
- [ ] Horizontal and vertical waveform layouts
- [~] Library docked under the decks, expandable to full window, with a source
      sidebar (local, playlists, external sources, radio) (folders and
      playlists are there; not expandable yet)
- [ ] Deck tool tabs under each deck: cues, loops, beat jump, slicer, keyboard,
      stems, FX
- [ ] Popover panels for per-deck options: BPM, key, beat grid, EQ, crossfader
- [~] Drag and drop from library to deck, deck to deck, and into playlists
      (tracks can be dragged from the library onto Flow's lanes only)
- [ ] First-run setup wizard: theme, waveform style, jog wheels on or off,
      import an existing library
- [~] Light and dark themes, plus user themes (light, dark and match-system;
      no user themes)
- [ ] Keyboard shortcut editor
- [ ] Separate metering window
- [ ] Layouts that scale from a laptop screen to a large display, with the same
      structure reused by the tablet client

## Out of scope for now

- Video output and VJ features
- Lyrics
- Lighting control
- CDJ network link (Pro DJ Link)
- Controller jog-wheel screens and on-device waveforms
- Licensed streaming services (Beatport, Tidal and similar)

## Known constraints

- **Remote latency:** guests feel roughly 50–200 ms between an action and the
  sound. Mixing and effects work; scratching remotely does not.
- **GPUI:** pre-1.0 with frequent breaking changes; pin exact versions.
- **External sources:** downloading from YouTube breaks its terms of service, so
  this stays an optional plugin the user enables.
- **Public broadcasting:** streaming copyrighted music needs licences in most
  countries. Responsibility sits with whoever operates the stream host or a
  public directory.
