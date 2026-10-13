//! Prints the detected tempo, energy and cue points of audio files.
//!
//!     cargo run -p djcraft-analysis --example tempo -- <file>...

use std::path::Path;
use std::time::Instant;

use djcraft_analysis::{TempoRange, detect_structure, detect_tempo};
use djcraft_engine::Track;

/// Tempo does not need treble, and a lower rate is quicker to decode.
const ANALYSIS_RATE: u32 = 22_050;

fn main() {
    for file in std::env::args().skip(1) {
        let started = Instant::now();
        let track = match Track::decode(Path::new(&file), ANALYSIS_RATE) {
            Ok(track) => track,
            Err(error) => {
                println!("{file}: {error}");
                continue;
            }
        };
        let decoded = started.elapsed();
        let samples = track.mono();
        let tempo = detect_tempo(&samples, ANALYSIS_RATE, TempoRange::default());
        if let Some(structure) = detect_structure(&samples, ANALYSIS_RATE, tempo) {
            let cues: Vec<String> = structure
                .cues
                .iter()
                .map(|cue| {
                    let seconds = cue.seconds.round() as u64;
                    format!(
                        "{}:{:02} (energy {})",
                        seconds / 60,
                        seconds % 60,
                        cue.energy
                    )
                })
                .collect();
            println!("energy {}  cues {}", structure.energy, cues.join(", "));
        }
        match tempo {
            Some(tempo) => println!(
                "{:7.2} BPM  first beat {:.3} s  confidence {:.2}  (decode {:.1?}, analyse {:.1?})  {}",
                tempo.bpm,
                tempo.first_beat_seconds,
                tempo.confidence,
                decoded,
                started.elapsed() - decoded,
                track.title,
            ),
            None => println!("no steady beat found  {}", track.title),
        }
    }
}
