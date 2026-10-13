//! Tempo and beat-grid detection for music with a steady beat.
//!
//! The track is reduced to an onset envelope (how much new energy arrives in
//! each short slice of time). The envelope's autocorrelation gives a rough
//! tempo; folding the whole envelope at nearby beat lengths then finds the
//! one tempo and phase at which beats stay lined up from start to finish.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

/// Samples per analysis window.
const FRAME: usize = 1024;
/// The envelope has one value per this many seconds, whatever the sample
/// rate of the audio.
const ENVELOPE_STEP_SECONDS: f64 = 0.005;
/// Tracks shorter than this have too few beats to measure.
const MIN_SECONDS: f64 = 5.0;
/// The tempo listeners most often tap along to; used only to choose between
/// a tempo and its half or double.
const PREFERRED_BPM: f64 = 125.0;
/// Slices one beat is divided into when looking for where beats fall.
const PHASE_BINS: usize = 64;
/// Beats are placed by the bass drum, so their timing comes from below this
/// frequency when there is enough going on down there.
const BASS_LIMIT_HZ: f64 = 200.0;
/// How sharply bass onsets must gather before they are trusted for timing.
const BASS_CONFIDENCE: f32 = 0.15;
/// A drum hit registers as an onset this long after the middle of the
/// analysis window reaches it. Measured with synthetic drum hits at known
/// positions (see the tests); real kicks vary by a few milliseconds.
const ONSET_LAG_SECONDS: f64 = 0.015;

/// The tempos a track may be reported at. A beat at 70 BPM is also a beat at
/// 140, so the range decides which is meant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoRange {
    pub min_bpm: f64,
    pub max_bpm: f64,
}

impl Default for TempoRange {
    fn default() -> Self {
        Self {
            min_bpm: 70.0,
            max_bpm: 180.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tempo {
    pub bpm: f64,
    /// Time of the first beat. Later beats follow every `60 / bpm` seconds.
    /// Which beat starts a bar is not worked out.
    pub first_beat_seconds: f64,
    /// 0.0 to 1.0: how sharply onsets gather on the beat.
    pub confidence: f32,
}

/// Finds the tempo and beat positions of mono audio, assuming the tempo does
/// not change. Returns `None` for audio that is too short or has no rhythm
/// to measure.
pub fn detect_tempo(samples: &[f32], sample_rate: u32, range: TempoRange) -> Option<Tempo> {
    let seconds = samples.len() as f64 / sample_rate as f64;
    if seconds < MIN_SECONDS || range.min_bpm <= 0.0 || range.max_bpm <= range.min_bpm {
        return None;
    }
    let hop = ((sample_rate as f64 * ENVELOPE_STEP_SECONDS).round() as usize).max(1);
    let rate = sample_rate as f64 / hop as f64;
    let (envelope, bass) = onset_envelopes(samples, sample_rate, hop);
    if envelope.iter().all(|value| *value <= 0.0) {
        return None;
    }

    let coarse = coarse_tempo(&envelope, rate, range)?;
    // Sweep around the rough answer, then around the best of that sweep.
    let (bpm, _) = best_fold(&envelope, rate, coarse, 1.5, 0.01);
    let (bpm, fold) = best_fold(&envelope, rate, bpm, 0.01, 0.001);

    // Off-beat hi-hats can out-shout the kick across the whole spectrum, so
    // take the beat's position from the bass where it is clear.
    let bass_fold = self::fold(&bass, rate, bpm);
    let phase = if bass_fold.confidence >= BASS_CONFIDENCE {
        bass_fold.phase
    } else {
        fold.phase
    };

    let period = 60.0 / bpm;
    let window_centre = FRAME as f64 / 2.0 / sample_rate as f64;
    let first_beat = (phase * period + window_centre + ONSET_LAG_SECONDS).rem_euclid(period);
    Some(Tempo {
        bpm,
        first_beat_seconds: first_beat,
        confidence: fold.confidence,
    })
}

/// Spectral flux: for each step, the total rise in level across frequency
/// bands since the previous step, with the slowly varying average removed.
/// Returns the envelope for the whole spectrum and for the bass alone.
fn onset_envelopes(samples: &[f32], sample_rate: u32, hop: usize) -> (Vec<f32>, Vec<f32>) {
    let fft = FftPlanner::<f32>::new().plan_fft_forward(FRAME);
    let window: Vec<f32> = (0..FRAME)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FRAME as f32).cos())
        .collect();
    let mut buffer = vec![Complex::new(0.0, 0.0); FRAME];
    let mut scratch = vec![Complex::new(0.0, 0.0); fft.get_inplace_scratch_len()];
    let bins = FRAME / 2;
    let mut previous = vec![0f32; bins];

    let bass_bins =
        ((BASS_LIMIT_HZ * FRAME as f64 / sample_rate as f64).ceil() as usize).clamp(1, bins);

    let frames = samples.len().saturating_sub(FRAME) / hop + 1;
    let mut flux = Vec::with_capacity(frames);
    let mut bass_flux = Vec::with_capacity(frames);
    for frame in 0..frames {
        let start = frame * hop;
        for (slot, (sample, weight)) in buffer
            .iter_mut()
            .zip(samples[start..start + FRAME].iter().zip(&window))
        {
            *slot = Complex::new(sample * weight, 0.0);
        }
        fft.process_with_scratch(&mut buffer, &mut scratch);
        let (mut rise, mut bass_rise) = (0.0, 0.0);
        for (index, (bin, last)) in buffer[..bins].iter().zip(previous.iter_mut()).enumerate() {
            // Compressing the level stops loud bass from drowning out
            // quieter but sharper sounds such as hi-hats.
            let level = (1.0 + 100.0 * bin.norm()).ln();
            let change = (level - *last).max(0.0);
            rise += change;
            if index < bass_bins {
                bass_rise += change;
            }
            *last = level;
        }
        flux.push(rise);
        bass_flux.push(bass_rise);
    }
    (above_local_average(flux), above_local_average(bass_flux))
}

/// Keeps only what stands above the average of the surrounding half second.
fn above_local_average(mut flux: Vec<f32>) -> Vec<f32> {
    // The first frame "rises" from silence, which is not an onset.
    if let Some(first) = flux.first_mut() {
        *first = 0.0;
    }
    let radius = ((0.25 / ENVELOPE_STEP_SECONDS) as usize).max(1);
    let mut prefix = vec![0f64; flux.len() + 1];
    for (i, value) in flux.iter().enumerate() {
        prefix[i + 1] = prefix[i] + *value as f64;
    }
    (0..flux.len())
        .map(|i| {
            let (from, to) = (i.saturating_sub(radius), (i + radius + 1).min(flux.len()));
            let mean = (prefix[to] - prefix[from]) / (to - from) as f64;
            (flux[i] as f64 - mean).max(0.0) as f32
        })
        .collect()
}

/// A rough tempo from the envelope's autocorrelation, to within about one
/// BPM. Each candidate is scored by how well the envelope repeats after one,
/// two, three and four beats.
fn coarse_tempo(envelope: &[f32], rate: f64, range: TempoRange) -> Option<f64> {
    let max_lag = ((4.0 * 60.0 * rate / range.min_bpm).ceil() as usize + 2).min(envelope.len() / 2);
    let autocorrelation: Vec<f64> = (0..=max_lag)
        .map(|lag| {
            envelope[lag..]
                .iter()
                .zip(envelope)
                .map(|(a, b)| *a as f64 * *b as f64)
                .sum()
        })
        .collect();
    let at = |lag: f64| -> f64 {
        let index = lag.floor() as usize;
        match (autocorrelation.get(index), autocorrelation.get(index + 1)) {
            (Some(a), Some(b)) => a + (b - a) * (lag - index as f64),
            _ => 0.0,
        }
    };

    let mut best: Option<(f64, f64)> = None;
    let mut bpm = range.min_bpm;
    while bpm <= range.max_bpm {
        let lag = 60.0 * rate / bpm;
        let repeats: f64 = (1..=4).map(|beats| at(lag * beats as f64)).sum();
        // A gentle preference for middling tempos settles half/double ties.
        let octaves = (bpm / PREFERRED_BPM).log2();
        let score = repeats * (-0.5 * (octaves / 0.9).powi(2)).exp();
        if best.is_none_or(|(_, top)| score > top) {
            best = Some((bpm, score));
        }
        bpm += 0.25;
    }
    best.filter(|(_, score)| *score > 0.0).map(|(bpm, _)| bpm)
}

struct Fold {
    /// Where beats fall within a beat length, 0.0 to 1.0.
    phase: f64,
    sharpness: f64,
    confidence: f32,
}

/// Lays the envelope over itself one beat at a time. At the right tempo,
/// onsets pile up in the same place every beat; slightly off, they smear.
fn fold(envelope: &[f32], rate: f64, bpm: f64) -> Fold {
    let period = 60.0 * rate / bpm;
    let mut bins = [0f64; PHASE_BINS];
    for (index, value) in envelope.iter().enumerate() {
        let position = (index as f64 / period).fract();
        bins[(position * PHASE_BINS as f64) as usize % PHASE_BINS] += *value as f64;
    }
    let total: f64 = bins.iter().sum();
    let (peak, sharpness) = (0..PHASE_BINS)
        .map(|i| {
            let around =
                bins[(i + PHASE_BINS - 1) % PHASE_BINS] + bins[i] + bins[(i + 1) % PHASE_BINS];
            (i, around)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap_or((0, 0.0));

    // Place the peak between bins by the balance of its neighbours.
    let (before, after) = (
        bins[(peak + PHASE_BINS - 1) % PHASE_BINS],
        bins[(peak + 1) % PHASE_BINS],
    );
    let spread = before + bins[peak] + after;
    let offset = if spread > 0.0 {
        (after - before) / spread
    } else {
        0.0
    };
    let phase = ((peak as f64 + 0.5 + offset) / PHASE_BINS as f64).rem_euclid(1.0);
    // Three bins hold 3/64 of evenly spread onsets, and all of perfect ones.
    let even = 3.0 / PHASE_BINS as f64;
    let share = if total > 0.0 { sharpness / total } else { 0.0 };
    Fold {
        phase,
        sharpness,
        confidence: ((share - even) / (1.0 - even)).clamp(0.0, 1.0) as f32,
    }
}

/// The tempo within `centre ± reach` whose fold is sharpest.
fn best_fold(envelope: &[f32], rate: f64, centre: f64, reach: f64, step: f64) -> (f64, Fold) {
    let steps = (reach / step).round() as i64;
    (-steps..=steps)
        .map(|i| centre + i as f64 * step)
        .map(|bpm| (bpm, fold(envelope, rate, bpm)))
        .max_by(|a, b| a.1.sharpness.total_cmp(&b.1.sharpness))
        .expect("at least the centre tempo is tried")
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 22_050;

    /// A drum-machine pattern: a kick on every beat, a quieter hi-hat on the
    /// off-beat, and background noise, starting at `first_beat` seconds.
    fn pattern(bpm: f64, first_beat: f64, seconds: f64) -> Vec<f32> {
        let mut samples = vec![0f32; (seconds * RATE as f64) as usize];
        let mut noise = 0x2545_f491u32;
        let mut random = move || {
            noise ^= noise << 13;
            noise ^= noise >> 17;
            noise ^= noise << 5;
            noise as f32 / u32::MAX as f32 - 0.5
        };
        for sample in samples.iter_mut() {
            *sample = 0.02 * random();
        }
        let period = 60.0 / bpm;
        let mut beat = 0;
        loop {
            let kick = ((first_beat + beat as f64 * period) * RATE as f64) as usize;
            let hat = kick + (period / 2.0 * RATE as f64) as usize;
            if hat + 2000 >= samples.len() {
                break;
            }
            for i in 0..2000 {
                let t = i as f32 / RATE as f32;
                samples[kick + i] +=
                    0.8 * (-t * 30.0).exp() * (std::f32::consts::TAU * 60.0 * t).sin();
            }
            for i in 0..400 {
                let t = i as f32 / RATE as f32;
                samples[hat + i] += 0.25 * (-t * 200.0).exp() * random();
            }
            beat += 1;
        }
        samples
    }

    fn assert_tempo(bpm: f64, first_beat: f64) {
        let tempo = detect_tempo(&pattern(bpm, first_beat, 90.0), RATE, TempoRange::default())
            .unwrap_or_else(|| panic!("no tempo found for {bpm}"));
        assert!(
            (tempo.bpm - bpm).abs() < 0.02,
            "{bpm} BPM read as {}",
            tempo.bpm
        );

        // The grid may start on any beat, so compare within one beat length.
        let period = 60.0 / bpm;
        let error = (tempo.first_beat_seconds - first_beat).rem_euclid(period);
        let error = error.min(period - error);
        assert!(error < 0.008, "{bpm} BPM: first beat off by {error:.4} s");
        assert!(
            tempo.confidence > 0.3,
            "{bpm} BPM: confidence {}",
            tempo.confidence
        );
    }

    #[test]
    fn finds_tempo_and_first_beat() {
        assert_tempo(128.0, 0.25);
        assert_tempo(174.0, 0.10);
        assert_tempo(90.5, 0.40);
        assert_tempo(122.37, 0.0);
    }

    #[test]
    fn the_range_decides_between_half_and_double() {
        let samples = pattern(140.0, 0.2, 60.0);
        let half = TempoRange {
            min_bpm: 60.0,
            max_bpm: 100.0,
        };
        let tempo = detect_tempo(&samples, RATE, half).unwrap();
        assert!((tempo.bpm - 70.0).abs() < 0.02, "read as {}", tempo.bpm);
    }

    #[test]
    fn works_at_other_sample_rates() {
        let samples: Vec<f32> = pattern(126.0, 0.3, 60.0)
            .iter()
            .flat_map(|s| [*s, *s])
            .collect();
        let tempo = detect_tempo(&samples, RATE * 2, TempoRange::default()).unwrap();
        assert!((tempo.bpm - 126.0).abs() < 0.02, "read as {}", tempo.bpm);
    }

    #[test]
    fn gives_up_on_audio_without_a_beat() {
        assert_eq!(
            detect_tempo(&vec![0.0; RATE as usize * 30], RATE, TempoRange::default()),
            None
        );
        assert_eq!(
            detect_tempo(&pattern(128.0, 0.0, 3.0), RATE, TempoRange::default()),
            None
        );
    }
}
