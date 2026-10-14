//! Background analysis of library tracks.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};

use djcraft_analysis::{TempoRange, detect_key, detect_structure, detect_tempo};
use djcraft_engine::Track;
use djcraft_library::{Camelot, CuePoint, KeyResult, Library, TempoResult};

/// Raise this when the analysis changes enough that every track should be
/// analysed again. Version 2 added energy and cue points; version 3, key.
const ANALYSIS_VERSION: i64 = 3;
/// Tempo and key do not need treble, and a lower rate is quicker to decode.
const ANALYSIS_RATE: u32 = 22_050;
const MAX_WORKERS: usize = 4;

#[derive(Default)]
pub struct Progress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
}

/// Analyses every track that needs it and stores the results. Blocking, and
/// busy on several threads; run it off the UI thread.
pub fn analyse_pending(db_path: &Path, progress: &Progress) -> djcraft_library::Result<()> {
    let mut library = Library::open(db_path)?;
    let pending = library.pending_tempo_analysis(ANALYSIS_VERSION)?;
    progress.done.store(0, Ordering::Relaxed);
    progress.total.store(pending.len(), Ordering::Relaxed);
    if pending.is_empty() {
        return Ok(());
    }

    // Leave half the machine free: the user may be mixing meanwhile.
    let workers = std::thread::available_parallelism()
        .map(|cores| cores.get() / 2)
        .unwrap_or(1)
        .clamp(1, MAX_WORKERS);
    let queue = Mutex::new(VecDeque::from(pending));
    let (results, received) = mpsc::channel::<(i64, Analysis)>();

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let results = results.clone();
            let queue = &queue;
            scope.spawn(move || {
                loop {
                    let next = queue.lock().ok().and_then(|mut queue| queue.pop_front());
                    let Some((id, path)) = next else {
                        break;
                    };
                    if results.send((id, analyse(&path))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(results);
        // Results are written from this one thread as they arrive.
        for (id, analysis) in received {
            library.store_structure(id, analysis.energy, &analysis.cues)?;
            library.store_key(id, analysis.key)?;
            // Last, as this is what marks the track as analysed.
            library.store_tempo(id, ANALYSIS_VERSION, analysis.tempo)?;
            progress.done.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    })
}

#[derive(Default)]
struct Analysis {
    tempo: Option<TempoResult>,
    key: Option<KeyResult>,
    energy: Option<u8>,
    cues: Vec<CuePoint>,
}

/// A file that cannot be decoded simply has nothing to report.
fn analyse(path: &Path) -> Analysis {
    let Ok(track) = Track::decode(path, ANALYSIS_RATE) else {
        return Analysis::default();
    };
    let samples = track.mono();
    let tempo = detect_tempo(&samples, ANALYSIS_RATE, TempoRange::default());
    // Cue points sit on the bar lines of a tempo worth trusting.
    let structure = detect_structure(
        &samples,
        ANALYSIS_RATE,
        tempo.filter(|t| t.confidence >= 0.1),
    );
    Analysis {
        tempo: tempo.map(|tempo| TempoResult {
            bpm: tempo.bpm,
            first_beat_seconds: tempo.first_beat_seconds,
            confidence: tempo.confidence as f64,
        }),
        key: detect_key(&samples, ANALYSIS_RATE).map(|key| KeyResult {
            key: Camelot::from_tonic(key.tonic, key.major),
            confidence: key.confidence as f64,
        }),
        energy: structure.as_ref().map(|structure| structure.energy),
        cues: structure
            .map(|structure| structure.cues)
            .unwrap_or_default()
            .into_iter()
            .map(|cue| CuePoint {
                seconds: cue.seconds,
                energy: cue.energy,
            })
            .collect(),
    }
}
