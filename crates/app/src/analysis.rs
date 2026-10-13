//! Background analysis of library tracks.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};

use djcraft_analysis::{TempoRange, detect_tempo};
use djcraft_engine::Track;
use djcraft_library::{Library, TempoResult};

/// Raise this when tempo detection changes enough that every track should
/// be analysed again.
const TEMPO_VERSION: i64 = 1;
/// Tempo does not need treble, and a lower rate is quicker to decode.
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
    let library = Library::open(db_path)?;
    let pending = library.pending_tempo_analysis(TEMPO_VERSION)?;
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
    let (results, received) = mpsc::channel::<(i64, Option<TempoResult>)>();

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
        for (id, tempo) in received {
            library.store_tempo(id, TEMPO_VERSION, tempo)?;
            progress.done.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    })
}

/// A file that cannot be decoded simply has no tempo.
fn analyse(path: &Path) -> Option<TempoResult> {
    let track = Track::decode(path, ANALYSIS_RATE).ok()?;
    let tempo = detect_tempo(&track.mono(), ANALYSIS_RATE, TempoRange::default())?;
    Some(TempoResult {
        bpm: tempo.bpm,
        first_beat_seconds: tempo.first_beat_seconds,
        confidence: tempo.confidence as f64,
    })
}
