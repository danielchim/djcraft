//! djcraft audio engine. No UI code lives here: the engine takes
//! [`Action`]s from the control protocol and reports a [`SessionState`].

mod cache;
mod mixer;
mod output;
mod track;

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;

use djcraft_protocol::{
    Action, DeckAction, DeckState, MixerAction, Rejection, Role, SessionState, TrackInfo,
};

use mixer::{Command, Shared};

pub use mixer::{DECKS, MAX_SCRATCH_SPEED, MAX_TEMPO, MIN_TEMPO, Mixer};
pub use track::{CHANNELS, DecodeError, Track};

const COMMAND_QUEUE: usize = 1024;
const GARBAGE_QUEUE: usize = 64;

#[derive(Debug)]
pub enum EngineError {
    NoOutputDevice,
    Device(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::NoOutputDevice => f.write_str("no audio output device"),
            EngineError::Device(message) => write!(f, "audio device error: {message}"),
        }
    }
}

impl std::error::Error for EngineError {}

struct DeckControl {
    track: Option<Arc<Track>>,
    /// Identifies the load in progress, so a superseded one is discarded.
    loading: Option<u64>,
    error: Option<String>,
    tempo: f64,
    key_lock: bool,
    volume: f32,
}

struct Loaded {
    deck: usize,
    load_id: u64,
    result: Result<Arc<Track>, DecodeError>,
}

/// The control side of the engine. Lives on the UI thread; the matching
/// [`Mixer`] runs on the audio thread.
pub struct Engine {
    sample_rate: u32,
    commands: rtrb::Producer<Command>,
    garbage: rtrb::Consumer<Arc<Track>>,
    shared: Arc<Shared>,
    decks: [DeckControl; DECKS],
    master_gain: f32,
    next_load_id: u64,
    cache_dir: Option<PathBuf>,
    loaded_tx: mpsc::Sender<Loaded>,
    loaded_rx: mpsc::Receiver<Loaded>,
    _stream: Option<cpal::Stream>,
}

impl Engine {
    /// Starts the engine on the default audio output device.
    pub fn new() -> Result<Self, EngineError> {
        let (device, config) = output::default_device()?;
        let (mut engine, mixer) = Self::headless(config.sample_rate());
        engine._stream = Some(output::start(&device, config, mixer)?);
        Ok(engine)
    }

    /// Creates an engine with no audio device. The caller drives the returned
    /// [`Mixer`] itself, for offline rendering and tests.
    pub fn headless(sample_rate: u32) -> (Self, Mixer) {
        let (commands, command_rx) = rtrb::RingBuffer::new(COMMAND_QUEUE);
        let (garbage_tx, garbage) = rtrb::RingBuffer::new(GARBAGE_QUEUE);
        let shared = Arc::new(Shared::default());
        let mixer = Mixer::new(sample_rate, command_rx, garbage_tx, shared.clone());
        let (loaded_tx, loaded_rx) = mpsc::channel();
        let engine = Self {
            sample_rate,
            commands,
            garbage,
            shared,
            decks: std::array::from_fn(|_| DeckControl {
                track: None,
                loading: None,
                error: None,
                tempo: 1.0,
                key_lock: false,
                volume: 1.0,
            }),
            master_gain: 1.0,
            next_load_id: 0,
            cache_dir: None,
            loaded_tx,
            loaded_rx,
            _stream: None,
        };
        (engine, mixer)
    }

    /// Sets where waveforms are cached between loads. Off by default.
    pub fn set_cache_dir(&mut self, dir: impl Into<PathBuf>) {
        self.cache_dir = Some(dir.into());
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The track loaded on a deck, for drawing its waveform.
    pub fn track(&self, deck: usize) -> Option<&Arc<Track>> {
        self.decks.get(deck)?.track.as_ref()
    }

    /// Loads a track onto a deck. It may still be decoding.
    pub fn load_track(&mut self, deck: usize, track: Arc<Track>) -> Result<(), Rejection> {
        if deck >= DECKS {
            return Err(Rejection::UnknownDeck);
        }
        self.send(Command::Load {
            deck,
            track: track.clone(),
        })?;
        let control = &mut self.decks[deck];
        control.track = Some(track);
        control.loading = None;
        control.error = None;
        Ok(())
    }

    /// Applies an action on behalf of a participant with the given role.
    pub fn apply(&mut self, role: Role, action: Action) -> Result<(), Rejection> {
        if !role.allows(&action) {
            return Err(Rejection::NotPermitted);
        }
        match action {
            Action::Deck { deck, action } => {
                let deck = deck as usize;
                if deck >= DECKS {
                    return Err(Rejection::UnknownDeck);
                }
                self.apply_deck(deck, action)
            }
            Action::Mixer {
                action: MixerAction::SetMasterGain { level },
            } => {
                let level = unit_level(level)?;
                self.send(Command::MasterGain { level })?;
                self.master_gain = level;
                Ok(())
            }
        }
    }

    fn apply_deck(&mut self, deck: usize, action: DeckAction) -> Result<(), Rejection> {
        match action {
            DeckAction::Load { path } => {
                let load_id = self.next_load_id;
                self.next_load_id += 1;
                self.decks[deck].loading = Some(load_id);
                self.decks[deck].error = None;
                let sample_rate = self.sample_rate;
                let loaded = self.loaded_tx.clone();
                let cache_dir = self.cache_dir.clone();
                std::thread::spawn(move || {
                    let send = |result| {
                        let _ = loaded.send(Loaded {
                            deck,
                            load_id,
                            result,
                        });
                    };
                    // The track is handed over as soon as it is playable;
                    // decoding carries on in this thread.
                    let result = Track::load(
                        &PathBuf::from(path),
                        sample_rate,
                        cache_dir.as_deref(),
                        |track| send(Ok(track)),
                    );
                    if let Err(error) = result {
                        send(Err(error));
                    }
                });
                Ok(())
            }
            DeckAction::Play => self.send(Command::Play { deck }),
            DeckAction::Pause => self.send(Command::Pause { deck }),
            DeckAction::Seek { seconds } => {
                if !seconds.is_finite() {
                    return Err(Rejection::InvalidValue);
                }
                let frame = seconds * self.sample_rate as f64;
                self.send(Command::Seek { deck, frame })
            }
            DeckAction::SetTempo { ratio } => {
                if !(MIN_TEMPO..=MAX_TEMPO).contains(&ratio) {
                    return Err(Rejection::InvalidValue);
                }
                self.send(Command::Tempo { deck, ratio })?;
                self.decks[deck].tempo = ratio;
                Ok(())
            }
            DeckAction::SetKeyLock { on } => {
                self.send(Command::KeyLock { deck, on })?;
                self.decks[deck].key_lock = on;
                Ok(())
            }
            DeckAction::SetVolume { level } => {
                let level = unit_level(level)?;
                self.send(Command::Volume { deck, level })?;
                self.decks[deck].volume = level;
                Ok(())
            }
            DeckAction::Scratch { speed } => {
                if speed.is_some_and(|s| !(-MAX_SCRATCH_SPEED..=MAX_SCRATCH_SPEED).contains(&s)) {
                    return Err(Rejection::InvalidValue);
                }
                self.send(Command::Scratch { deck, speed })
            }
        }
    }

    fn send(&mut self, command: Command) -> Result<(), Rejection> {
        self.commands.push(command).map_err(|_| Rejection::Busy)
    }

    /// Finishes pending loads and frees tracks the audio thread has let go
    /// of. Called by [`Engine::state`].
    fn poll(&mut self) {
        while self.garbage.pop().is_ok() {}
        while let Ok(loaded) = self.loaded_rx.try_recv() {
            if self.decks[loaded.deck].loading != Some(loaded.load_id) {
                continue;
            }
            let outcome = match loaded.result {
                Ok(track) => self
                    .load_track(loaded.deck, track)
                    .map_err(|_| "engine busy".to_string()),
                Err(error) => Err(error.to_string()),
            };
            let control = &mut self.decks[loaded.deck];
            control.loading = None;
            control.error = outcome.err();
        }
    }

    /// The current state, as broadcast to every participant.
    pub fn state(&mut self) -> SessionState {
        self.poll();
        let sample_rate = self.sample_rate as f64;
        SessionState {
            decks: self
                .decks
                .iter()
                .zip(&self.shared.decks)
                .map(|(control, shared)| DeckState {
                    track: control.track.as_ref().map(|track| TrackInfo {
                        title: track.title.clone(),
                        duration_seconds: track.duration_seconds(),
                        loaded_seconds: track.decoded_seconds(),
                        fully_loaded: track.is_complete(),
                    }),
                    loading: control.loading.is_some(),
                    error: control.error.clone(),
                    playing: shared.playing(),
                    position_seconds: shared.position() / sample_rate,
                    tempo: control.tempo,
                    key_lock: control.key_lock,
                    volume: control.volume,
                })
                .collect(),
            master_gain: self.master_gain,
        }
    }
}

fn unit_level(level: f32) -> Result<f32, Rejection> {
    if (0.0..=1.0).contains(&level) {
        Ok(level)
    } else {
        Err(Rejection::InvalidValue)
    }
}
