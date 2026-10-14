//! Tempo and beat-grid detection for music with a steady beat.
//!
//! The track is reduced to an onset envelope (how much new energy arrives in
//! each short slice of time). The envelope's autocorrelation gives a rough
//! tempo; folding the whole envelope at nearby beat lengths then finds the
//! one tempo and phase at which beats stay lined up from start to finish.
//! Last, the beat that starts each bar is found from where accents and
//! changes in loudness fall.

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
/// Beats in a bar. Music in other time signatures still gets a beat grid,
/// but its bar lines will be wrong.
pub const BEATS_PER_BAR: usize = 4;
/// How far either side of a beat its accent is looked for.
const ACCENT_REACH_SECONDS: f64 = 0.015;

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
    pub first_beat_seconds: f64,
    /// Which of the first [`BEATS_PER_BAR`] beats starts a bar, counting
    /// the first beat as 0.
    pub downbeat: u8,
    /// 0.0 to 1.0: how sharply onsets gather on the beat.
    pub confidence: f32,
}

impl Tempo {
    pub fn beat_seconds(&self) -> f64 {
        60.0 / self.bpm
    }

    /// Time of the first beat that starts a bar.
    pub fn first_bar_seconds(&self) -> f64 {
        self.first_beat_seconds + self.downbeat as f64 * self.beat_seconds()
    }
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
    // Where on the envelope something heard at `seconds` shows.
    let envelope_index = |seconds: f64| (seconds - window_centre - ONSET_LAG_SECONDS) * rate;
    let downbeat = downbeat(
        samples,
        sample_rate,
        &envelope,
        envelope_index,
        first_beat,
        period,
    );
    Some(Tempo {
        bpm,
        first_beat_seconds: first_beat,
        downbeat,
        confidence: fold.confidence,
    })
}

/// Which of the first [`BEATS_PER_BAR`] beats starts a bar. Two things mark
/// it: drums and new sounds tend to hit hardest at the start of a bar, and
/// sections (a break, a drop) begin on one. Each candidate is scored on both,
/// as a share of what the average candidate scores, so neither drowns out
/// the other.
fn downbeat(
    samples: &[f32],
    sample_rate: u32,
    envelope: &[f32],
    envelope_index: impl Fn(f64) -> f64,
    first_beat: f64,
    period: f64,
) -> u8 {
    let seconds = samples.len() as f64 / sample_rate as f64;
    let beats = ((seconds - first_beat) / period).floor().max(0.0) as usize;
    if beats < 2 * BEATS_PER_BAR {
        return 0;
    }
    let beat_time = |beat: usize| first_beat + beat as f64 * period;

    // How hard each beat hits.
    let reach = (ACCENT_REACH_SECONDS / ENVELOPE_STEP_SECONDS).ceil() as usize;
    let accent: Vec<f64> = (0..beats)
        .map(|beat| {
            let centre = envelope_index(beat_time(beat)).round();
            if centre < 0.0 {
                return 0.0;
            }
            let centre = centre as usize;
            envelope
                .get(centre.saturating_sub(reach)..(centre + reach + 1).min(envelope.len()))
                .map_or(0.0, |near| near.iter().copied().fold(0.0, f32::max) as f64)
        })
        .collect();

    // How loud each beat is, and how much that changes a bar either side.
    let loudness: Vec<f64> = (0..beats)
        .map(|beat| {
            let from = (beat_time(beat) * sample_rate as f64) as usize;
            let to = ((beat_time(beat + 1) * sample_rate as f64) as usize).min(samples.len());
            let part = &samples[from.min(to)..to];
            let power: f64 = part.iter().map(|s| (*s as f64).powi(2)).sum();
            10.0 * (power / part.len().max(1) as f64 + 1e-12).log10()
        })
        .collect();
    let mean = |from: usize, to: usize| loudness[from..to].iter().sum::<f64>() / (to - from) as f64;
    let change: Vec<f64> = (0..beats)
        .map(|beat| {
            if beat < BEATS_PER_BAR || beat + BEATS_PER_BAR > beats {
                return 0.0;
            }
            (mean(beat, beat + BEATS_PER_BAR) - mean(beat - BEATS_PER_BAR, beat)).abs()
        })
        .collect();

    let scores = |values: &[f64]| -> [f64; BEATS_PER_BAR] {
        let mut sums = [0.0; BEATS_PER_BAR];
        for (beat, value) in values.iter().enumerate() {
            sums[beat % BEATS_PER_BAR] += value;
        }
        let average = sums.iter().sum::<f64>() / BEATS_PER_BAR as f64;
        if average > 0.0 {
            sums.map(|sum| sum / average)
        } else {
            [0.0; BEATS_PER_BAR]
        }
    };
    let (accent, change) = (scores(&accent), scores(&change));
    (0..BEATS_PER_BAR)
        .max_by(|a, b| (accent[*a] + change[*a]).total_cmp(&(accent[*b] + change[*b])))
        .unwrap_or(0) as u8
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

    /// The drum pattern with bars: a crash on the first beat of every bar,
    /// the first bar starting on beat `downbeat`, and the hi-hats doubling
    /// in level from the ninth bar on, as a new section would.
    fn bars(bpm: f64, first_beat: f64, downbeat: usize, seconds: f64) -> Vec<f32> {
        let mut samples = pattern(bpm, first_beat, seconds);
        let period = 60.0 / bpm;
        let mut noise = 0x1234_5678u32;
        let mut random = move || {
            noise ^= noise << 13;
            noise ^= noise >> 17;
            noise ^= noise << 5;
            noise as f32 / u32::MAX as f32 - 0.5
        };
        let mut bar = 0;
        loop {
            let start = first_beat + (downbeat + bar * BEATS_PER_BAR) as f64 * period;
            let at = (start * RATE as f64) as usize;
            if at + RATE as usize >= samples.len() {
                break;
            }
            for i in 0..RATE as usize / 2 {
                let t = i as f32 / RATE as f32;
                samples[at + i] += 0.3 * (-t * 8.0).exp() * random();
            }
            if bar >= 8 {
                for beat in 0..BEATS_PER_BAR {
                    let hat = ((start + (beat as f64 + 0.5) * period) * RATE as f64) as usize;
                    for i in 0..400.min(samples.len().saturating_sub(hat)) {
                        let t = i as f32 / RATE as f32;
                        samples[hat + i] += 0.25 * (-t * 200.0).exp() * random();
                    }
                }
            }
            bar += 1;
        }
        samples
    }

    #[test]
    fn finds_the_beat_that_starts_each_bar() {
        for (bpm, first_beat, downbeat) in [
            (128.0, 0.2, 0),
            (124.0, 0.1, 1),
            (174.0, 0.3, 2),
            (100.0, 0.05, 3),
        ] {
            let tempo = detect_tempo(
                &bars(bpm, first_beat, downbeat, 60.0),
                RATE,
                TempoRange::default(),
            )
            .unwrap();
            // The grid may begin a beat later than the music if its first
            // beat is too close to the start to measure.
            let period = 60.0 / bpm;
            let expected = first_beat + downbeat as f64 * period;
            let error =
                (tempo.first_bar_seconds() - expected).rem_euclid(BEATS_PER_BAR as f64 * period);
            let error = error.min(BEATS_PER_BAR as f64 * period - error);
            assert!(
                error < 0.01,
                "{bpm} BPM, downbeat {downbeat}: bar off by {error:.3} s"
            );
            assert!(tempo.first_bar_seconds() >= tempo.first_beat_seconds);
        }
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
