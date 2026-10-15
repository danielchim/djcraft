//! Key detection.
//!
//! The track's spectrum is folded into the twelve notes of the octave, giving
//! how much of each note the track holds whatever octave it is played in.
//! That is compared with how much of each note music in each of the 24 major
//! and minor keys typically holds; the best match is the key.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

/// Analysis windows are at least this long. Bass notes a semitone apart are
/// only a few hertz apart, and telling them apart takes a long window.
const FRAME_SECONDS: f64 = 0.6;
/// Notes are read between these frequencies: from C2, below which a window
/// cannot separate semitones, up to where little but overtones and
/// percussion is left.
const LOWEST_HZ: f64 = 65.0;
const HIGHEST_HZ: f64 = 2000.0;
/// Parts of the spectrum this far below a window's strongest part are noise.
const FLOOR: f32 = 0.01;
/// Windows quieter than this, as a share of the loudest, are left out, so
/// that silence and fades add nothing.
const QUIET_WINDOW: f32 = 0.05;
/// Each semitone is split in three so that a track tuned sharp or flat can
/// be told from one in tune.
const SLOTS_PER_SEMITONE: usize = 3;
const SLOTS: usize = 12 * SLOTS_PER_SEMITONE;

/// How much of each note, counted in semitones above the tonic, listeners
/// expect in a major and a minor key. From Krumhansl and Kessler's
/// probe-tone experiments (1982).
const MAJOR: [f32; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR: [f32; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    /// The key's home note in semitones above C: 0 is C, 9 is A.
    pub tonic: u8,
    pub major: bool,
    /// 0.0 to 1.0: how closely the track's notes fit the key.
    pub confidence: f32,
}

/// Finds the key of mono audio, taking the track to stay in one key.
/// Returns `None` for audio that is too short or holds no notes.
pub fn detect_key(samples: &[f32], sample_rate: u32) -> Option<Key> {
    let chroma = chroma(samples, sample_rate)?;
    let mut best: Option<Key> = None;
    for tonic in 0..12 {
        for (major, profile) in [(true, &MAJOR), (false, &MINOR)] {
            let mut expected = [0f32; 12];
            for (interval, weight) in profile.iter().enumerate() {
                expected[(tonic + interval) % 12] = *weight;
            }
            let confidence = correlation(&chroma, &expected);
            if best.is_none_or(|key| confidence > key.confidence) {
                best = Some(Key {
                    tonic: tonic as u8,
                    major,
                    confidence,
                });
            }
        }
    }
    best.filter(|key| key.confidence > 0.0)
}

/// How much of each of the twelve notes the audio holds, from C upwards.
fn chroma(samples: &[f32], sample_rate: u32) -> Option<[f32; 12]> {
    let frame = ((FRAME_SECONDS * sample_rate as f64) as usize).next_power_of_two();
    if samples.len() < frame {
        return None;
    }
    let hop = frame / 2;
    let fft = FftPlanner::<f32>::new().plan_fft_forward(frame);
    let window: Vec<f32> = (0..frame)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / frame as f32).cos())
        .collect();
    let mut buffer = vec![Complex::new(0.0, 0.0); frame];
    let mut scratch = vec![Complex::new(0.0, 0.0); fft.get_inplace_scratch_len()];

    let hz_per_bin = sample_rate as f64 / frame as f64;
    let first = ((LOWEST_HZ / hz_per_bin).ceil() as usize).max(1);
    let last = ((HIGHEST_HZ / hz_per_bin) as usize).min(frame / 2 - 2);
    if last <= first {
        return None;
    }
    let mut levels = vec![0f32; last + 2];

    // One row of slots per window, with the window's level, so that quiet
    // windows can be dropped once the loudest is known.
    let mut windows: Vec<([f32; SLOTS], f32)> = Vec::new();
    for start in (0..=samples.len() - frame).step_by(hop) {
        for (slot, (sample, weight)) in buffer
            .iter_mut()
            .zip(samples[start..start + frame].iter().zip(&window))
        {
            *slot = Complex::new(sample * weight, 0.0);
        }
        fft.process_with_scratch(&mut buffer, &mut scratch);
        for (level, bin) in levels.iter_mut().zip(&buffer) {
            *level = bin.norm();
        }
        let strongest = levels[first..=last].iter().copied().fold(0.0, f32::max);
        let mut slots = [0f32; SLOTS];
        if strongest > 0.0 {
            for bin in first..=last {
                let (below, level, above) = (levels[bin - 1], levels[bin], levels[bin + 1]);
                // A note shows as a peak; what lies between peaks is the
                // spread of drums and noise.
                if level < strongest * FLOOR || level <= below || level < above {
                    continue;
                }
                // The peak's true place lies between bins: fit a curve
                // through it and its neighbours.
                let (a, b, c) = (below.max(1e-12).ln(), level.ln(), above.max(1e-12).ln());
                let bend = a - 2.0 * b + c;
                let offset = if bend < 0.0 {
                    (0.5 * (a - c) / bend).clamp(-0.5, 0.5)
                } else {
                    0.0
                };
                let hz = (bin as f64 + offset as f64) * hz_per_bin;
                // Semitones above the C below A440.
                let semitones = 12.0 * (hz / 440.0).log2() + 9.0;
                let slot = (semitones * SLOTS_PER_SEMITONE as f64).round() as i64;
                slots[slot.rem_euclid(SLOTS as i64) as usize] += level;
            }
        }
        windows.push((slots, strongest));
    }

    let loudest = windows.iter().map(|(_, level)| *level).fold(0.0, f32::max);
    if loudest <= 0.0 {
        return None;
    }
    let mut slots = [0f32; SLOTS];
    for (window, level) in &windows {
        if *level >= loudest * QUIET_WINDOW {
            for (total, value) in slots.iter_mut().zip(window) {
                *total += value;
            }
        }
    }

    // Notes gather in one slot of each semitone's three: the middle one for
    // a track in tune, another for one tuned sharp or flat. Centre the
    // semitones on whichever holds the most.
    let tuning = (0..SLOTS_PER_SEMITONE)
        .max_by(|a, b| {
            let held = |offset: usize| -> f32 {
                slots.iter().skip(offset).step_by(SLOTS_PER_SEMITONE).sum()
            };
            held(*a).total_cmp(&held(*b))
        })
        .unwrap_or(0);
    let mut chroma = [0f32; 12];
    for (note, total) in chroma.iter_mut().enumerate() {
        let centre = note * SLOTS_PER_SEMITONE + tuning;
        *total = slots[(centre + SLOTS - 1) % SLOTS]
            + slots[centre % SLOTS]
            + slots[(centre + 1) % SLOTS];
    }
    // A track tuned so flat that its notes fall in the last slot of the
    // semitone below has been centred a semitone low.
    if tuning == SLOTS_PER_SEMITONE - 1 {
        chroma.rotate_right(1);
    }
    chroma.iter().any(|total| *total > 0.0).then_some(chroma)
}

/// Pearson correlation: 1.0 when two sets of values rise and fall together.
fn correlation(a: &[f32; 12], b: &[f32; 12]) -> f32 {
    let mean = |values: &[f32; 12]| values.iter().sum::<f32>() / 12.0;
    let (mean_a, mean_b) = (mean(a), mean(b));
    let (mut both, mut spread_a, mut spread_b) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        both += (x - mean_a) * (y - mean_b);
        spread_a += (x - mean_a).powi(2);
        spread_b += (y - mean_b).powi(2);
    }
    let spread = (spread_a * spread_b).sqrt();
    if spread > 0.0 { both / spread } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 22_050;

    /// A note with overtones, like a plucked or bowed string. `note` counts
    /// semitones from C4; `cents` detunes it.
    fn add_note(out: &mut [f32], rate: u32, note: i32, cents: f64, level: f32) {
        let hz = 261.6256 * 2f64.powf((note as f64 + cents / 100.0) / 12.0);
        for (i, sample) in out.iter_mut().enumerate() {
            let t = i as f64 / rate as f64;
            for harmonic in 1..=4 {
                let partial = (std::f64::consts::TAU * hz * harmonic as f64 * t).sin();
                *sample += level * partial as f32 / (harmonic * harmonic) as f32;
            }
        }
    }

    /// A deterministic stand-in for random numbers, -1.0 to 1.0.
    fn noise(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*seed >> 8) as f32 / (1 << 23) as f32 - 1.0
    }

    /// A chord progression that settles a key: tonic, subdominant, dominant,
    /// tonic, twice over, each chord with its root in the bass and a burst
    /// of noise on every beat for drums.
    fn progression(rate: u32, tonic: i32, major: bool, cents: f64) -> Vec<f32> {
        let third = if major { 4 } else { 3 };
        let chord_seconds = 2.0;
        let chord_len = (chord_seconds * rate as f64) as usize;
        let mut samples = Vec::new();
        let mut seed = 7;
        for degree in [0, 5, 7, 0, 0, 5, 7, 0] {
            let root = tonic + degree;
            let mut chord = vec![0f32; chord_len];
            add_note(&mut chord, rate, root - 24, cents, 0.3);
            for interval in [0, third, 7] {
                add_note(&mut chord, rate, root + interval - 12, cents, 0.15);
            }
            let beat = chord_len / 4;
            for (i, sample) in chord.iter_mut().enumerate() {
                let into_beat = (i % beat) as f32 / rate as f32;
                *sample += 0.3 * noise(&mut seed) * (-into_beat * 40.0).exp();
            }
            samples.extend(chord);
        }
        samples
    }

    fn key_of(samples: &[f32], rate: u32) -> (u8, bool) {
        let key = detect_key(samples, rate).expect("a key");
        (key.tonic, key.major)
    }

    #[test]
    fn finds_every_major_and_minor_key() {
        for tonic in 0..12 {
            for major in [true, false] {
                let samples = progression(RATE, tonic, major, 0.0);
                assert_eq!(
                    key_of(&samples, RATE),
                    (tonic as u8, major),
                    "tonic {tonic}, major {major}"
                );
            }
        }
    }

    #[test]
    fn follows_tracks_tuned_sharp_or_flat() {
        for cents in [-45.0, -30.0, -15.0, 15.0, 30.0, 45.0] {
            for (tonic, major) in [(9, false), (0, true), (6, true)] {
                let samples = progression(RATE, tonic, major, cents);
                assert_eq!(
                    key_of(&samples, RATE),
                    (tonic as u8, major),
                    "tonic {tonic}, {cents} cents"
                );
            }
        }
    }

    #[test]
    fn works_at_other_sample_rates() {
        for rate in [11_025, 44_100, 48_000] {
            assert_eq!(key_of(&progression(rate, 2, false, 0.0), rate), (2, false));
        }
    }

    #[test]
    fn a_quiet_ending_in_another_key_is_left_out() {
        let mut samples = progression(RATE, 4, false, 0.0);
        // A long fade-out tail, far quieter than the track.
        let tail: Vec<f32> = progression(RATE, 5, true, 0.0)
            .into_iter()
            .chain(progression(RATE, 5, true, 0.0))
            .map(|sample| sample * 0.01)
            .collect();
        samples.extend(tail);
        assert_eq!(key_of(&samples, RATE), (4, false));
    }

    #[test]
    fn music_fits_its_key_far_better_than_noise_does() {
        let music = detect_key(&progression(RATE, 7, true, 0.0), RATE).unwrap();
        assert!(music.confidence > 0.7, "{}", music.confidence);

        let mut seed = 99;
        let hiss: Vec<f32> = (0..RATE * 20).map(|_| 0.5 * noise(&mut seed)).collect();
        let confidence = detect_key(&hiss, RATE).map_or(0.0, |key| key.confidence);
        assert!(confidence < 0.5, "{confidence}");
    }

    #[test]
    fn gives_up_on_silence_and_on_audio_that_is_too_short() {
        assert_eq!(detect_key(&vec![0.0; RATE as usize * 10], RATE), None);
        assert_eq!(
            detect_key(&progression(RATE, 0, true, 0.0)[..4000], RATE),
            None
        );
        assert_eq!(detect_key(&[], RATE), None);
    }
}
