//! Track analysis: signal processing on decoded audio, with no file or
//! database access. Callers decode a track, pass the samples in, and store
//! what comes back.

mod tempo;

pub use tempo::{Tempo, TempoRange, detect_tempo};
