//! The audio-thread side of the engine.
//!
//! Nothing in here allocates, locks or blocks once a [`Mixer`] is built.
//! Commands arrive on a lock-free queue, state leaves through atomics, and
//! replaced tracks are sent back to the control side to be freed there.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use signalsmith_stretch::Stretch;

use crate::track::{CHANNELS, Track};

pub const DECKS: usize = 2;
pub const MIN_TEMPO: f64 = 0.5;
pub const MAX_TEMPO: f64 = 2.0;
pub const MAX_SCRATCH_SPEED: f64 = 4.0;

/// Frames rendered per internal block.
const BLOCK: usize = 512;
/// Time constant for speed changes on the varispeed path, to avoid clicks.
const SPEED_SMOOTHING_SECONDS: f64 = 0.004;

pub(crate) enum Command {
    Load { deck: usize, track: Arc<Track> },
    Play { deck: usize },
    Pause { deck: usize },
    Seek { deck: usize, frame: f64 },
    Tempo { deck: usize, ratio: f64 },
    KeyLock { deck: usize, on: bool },
    Volume { deck: usize, level: f32 },
    Scratch { deck: usize, speed: Option<f64> },
    MasterGain { level: f32 },
}

#[derive(Default)]
pub(crate) struct DeckShared {
    /// Audible position in frames, as `f64` bits.
    position: AtomicU64,
    playing: AtomicBool,
}

impl DeckShared {
    pub(crate) fn position(&self) -> f64 {
        f64::from_bits(self.position.load(Ordering::Relaxed))
    }

    pub(crate) fn playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
pub(crate) struct Shared {
    pub(crate) decks: [DeckShared; DECKS],
}

struct Deck {
    track: Option<Arc<Track>>,
    /// Read position in frames.
    position: f64,
    playing: bool,
    tempo: f64,
    key_lock: bool,
    volume: f32,
    scratch: Option<f64>,
    /// Current speed on the varispeed path; follows the target smoothly.
    speed: f64,
    speed_smoothing: f64,
    gain: f32,
    stretch: Stretch,
    stretch_input: Box<[f32]>,
    /// Latency of the stretcher, in input frames per unit of tempo.
    stretch_input_latency: f64,
    stretch_output_latency: f64,
    stretching: bool,
    /// Fraction of an input frame carried between stretched blocks.
    input_carry: f64,
}

impl Deck {
    fn new(sample_rate: u32) -> Self {
        let stretch = Stretch::preset_default(CHANNELS as u32, sample_rate);
        let max_input = (BLOCK as f64 * MAX_TEMPO) as usize + 1;
        Self {
            track: None,
            position: 0.0,
            playing: false,
            tempo: 1.0,
            key_lock: false,
            volume: 1.0,
            scratch: None,
            speed: 0.0,
            speed_smoothing: 1.0 - (-1.0 / (SPEED_SMOOTHING_SECONDS * sample_rate as f64)).exp(),
            gain: 1.0,
            stretch_input_latency: stretch.input_latency() as f64,
            stretch_output_latency: stretch.output_latency() as f64,
            stretch,
            stretch_input: vec![0.0; max_input * CHANNELS].into_boxed_slice(),
            stretching: false,
            input_carry: 0.0,
        }
    }

    /// Fills `out` with the deck's signal before gain. Returns false, leaving
    /// `out` untouched, when the deck is silent.
    fn render(&mut self, out: &mut [f32]) -> bool {
        let Some(track) = self.track.as_deref() else {
            return false;
        };
        let frames = out.len() / CHANNELS;
        // Only what has been decoded so far can be read. While the track is
        // still loading, playback that catches up with the decoder waits.
        let available = track.decoded_frames();
        let complete = track.is_complete();

        let stretch = self.key_lock
            && self.scratch.is_none()
            && self.playing
            && (self.tempo - 1.0).abs() > 1e-3;
        if stretch {
            if !self.stretching {
                self.stretch.reset();
                self.stretching = true;
                self.input_carry = 0.0;
            }
            let wanted = frames as f64 * self.tempo + self.input_carry;
            let count = wanted as usize;
            self.input_carry = wanted - count as f64;

            let start = (self.position.max(0.0) as usize).min(available);
            let readable = count.min(available - start);
            let input = &mut self.stretch_input[..count * CHANNELS];
            let (head, tail) = input.split_at_mut(readable * CHANNELS);
            track.read(start, head);
            tail.fill(0.0);
            self.stretch.process(&*input, &mut *out);
            self.position = (self.position + count as f64).min(available as f64);
            self.speed = self.tempo;
        } else {
            self.stretching = false;
            let target = self
                .scratch
                .unwrap_or(if self.playing { self.tempo } else { 0.0 });
            if target == 0.0 && self.speed.abs() < 1e-4 {
                self.speed = 0.0;
                return false;
            }
            for frame in out.chunks_exact_mut(CHANNELS) {
                self.speed += (target - self.speed) * self.speed_smoothing;
                frame.copy_from_slice(&interpolate(track, available, self.position));
                self.position += self.speed;
            }
            self.position = self.position.clamp(0.0, available as f64);
        }

        if complete && self.playing && self.scratch.is_none() && self.position >= available as f64 {
            self.playing = false;
        }
        true
    }

    /// The position currently being heard, allowing for stretcher latency.
    fn audible_position(&self) -> f64 {
        if self.stretching {
            let latency = self.stretch_input_latency + self.stretch_output_latency * self.tempo;
            (self.position - latency).max(0.0)
        } else {
            self.position
        }
    }
}

/// Cubic Hermite interpolation of the first `frames` frames of a track.
/// Positions outside that range read as silence.
fn interpolate(track: &Track, frames: usize, position: f64) -> [f32; CHANNELS] {
    let base = position.floor();
    let t = (position - base) as f32;
    let index = base as isize;
    let frames = frames as isize;
    let frame = |i: isize| -> [f32; CHANNELS] {
        if i < 0 || i >= frames {
            [0.0; CHANNELS]
        } else {
            let i = i as usize * CHANNELS;
            [track.sample(i), track.sample(i + 1)]
        }
    };
    let (p0, p1, p2, p3) = (frame(index - 1), frame(index), frame(index + 1), frame(index + 2));
    let mut out = [0.0; CHANNELS];
    for c in 0..CHANNELS {
        let a = -0.5 * p0[c] + 1.5 * p1[c] - 1.5 * p2[c] + 0.5 * p3[c];
        let b = p0[c] - 2.5 * p1[c] + 2.0 * p2[c] - 0.5 * p3[c];
        let d = 0.5 * (p2[c] - p0[c]);
        out[c] = ((a * t + b) * t + d) * t + p1[c];
    }
    out
}

/// Renders all decks to an interleaved stereo stream. Owned by the audio
/// callback; also usable offline.
pub struct Mixer {
    commands: rtrb::Consumer<Command>,
    garbage: rtrb::Producer<Arc<Track>>,
    shared: Arc<Shared>,
    decks: [Deck; DECKS],
    master_gain: f32,
    deck_buffer: Box<[f32]>,
}

impl Mixer {
    pub(crate) fn new(
        sample_rate: u32,
        commands: rtrb::Consumer<Command>,
        garbage: rtrb::Producer<Arc<Track>>,
        shared: Arc<Shared>,
    ) -> Self {
        Self {
            commands,
            garbage,
            shared,
            decks: std::array::from_fn(|_| Deck::new(sample_rate)),
            master_gain: 1.0,
            deck_buffer: vec![0.0; BLOCK * CHANNELS].into_boxed_slice(),
        }
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::Load { deck, track } => {
                let deck = &mut self.decks[deck];
                if let Some(old) = deck.track.replace(track) {
                    // Freeing memory is the control side's job. If its queue
                    // is full the old track is dropped here as a last resort.
                    let _ = self.garbage.push(old);
                }
                deck.position = 0.0;
                deck.playing = false;
                deck.scratch = None;
                deck.speed = 0.0;
                deck.stretching = false;
            }
            Command::Play { deck } => {
                let deck = &mut self.decks[deck];
                if let Some(track) = &deck.track {
                    if track.is_complete() && deck.position >= track.frames() as f64 {
                        deck.position = 0.0;
                    }
                    deck.playing = true;
                }
            }
            Command::Pause { deck } => self.decks[deck].playing = false,
            Command::Seek { deck, frame } => {
                let deck = &mut self.decks[deck];
                if let Some(track) = &deck.track {
                    deck.position = frame.clamp(0.0, track.decoded_frames() as f64);
                    deck.stretching = false;
                }
            }
            Command::Tempo { deck, ratio } => self.decks[deck].tempo = ratio,
            Command::KeyLock { deck, on } => self.decks[deck].key_lock = on,
            Command::Volume { deck, level } => self.decks[deck].volume = level,
            Command::Scratch { deck, speed } => self.decks[deck].scratch = speed,
            Command::MasterGain { level } => self.master_gain = level,
        }
    }

    /// Renders the next `out.len() / 2` frames of interleaved stereo.
    pub fn render(&mut self, out: &mut [f32]) {
        while let Ok(command) = self.commands.pop() {
            self.apply(command);
        }

        for block in out.chunks_mut(BLOCK * CHANNELS) {
            block.fill(0.0);
            let frames = block.len() / CHANNELS;
            for deck in &mut self.decks {
                let buffer = &mut self.deck_buffer[..block.len()];
                let target = deck.volume * self.master_gain;
                if !deck.render(buffer) {
                    deck.gain = target;
                    continue;
                }
                let step = (target - deck.gain) / frames as f32;
                for (out, input) in block
                    .chunks_exact_mut(CHANNELS)
                    .zip(buffer.chunks_exact(CHANNELS))
                {
                    deck.gain += step;
                    out[0] += input[0] * deck.gain;
                    out[1] += input[1] * deck.gain;
                }
                deck.gain = target;
            }
            for sample in block.iter_mut() {
                *sample = sample.clamp(-1.0, 1.0);
            }
        }

        for (deck, shared) in self.decks.iter().zip(&self.shared.decks) {
            shared
                .position
                .store(deck.audible_position().to_bits(), Ordering::Relaxed);
            shared.playing.store(deck.playing, Ordering::Relaxed);
        }
    }
}
