//! Track analysis: signal processing on decoded audio, with no file or
//! database access. Callers decode a track, pass the samples in, and store
//! what comes back.

mod structure;
mod tempo;

pub use structure::{Cue, MAX_CUES, Structure, detect_structure};
pub use tempo::{Tempo, TempoRange, detect_tempo};
