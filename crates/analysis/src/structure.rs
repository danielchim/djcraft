//! Cue points and energy.
//!
//! A track is divided into sections of roughly even loudness. A cue point
//! marks where each section starts and carries that section's energy level,
//! so the cues trace the shape of the track: intro, build, drop, break.

use crate::Tempo;

/// The most cue points a track is given.
pub const MAX_CUES: usize = 8;

/// Sections are sought on boundaries this many bars apart, as most dance
/// music changes every 4, 8 or 16 bars.
const PHRASE_BARS: usize = 4;
/// Cue points are kept at least this many bars apart.
const MIN_SECTION_BARS: usize = 8;
/// How many bars either side of a boundary are compared.
const COMPARE_BARS: usize = 8;
/// A boundary counts only if loudness changes across it by this much.
const MIN_CHANGE_DB: f64 = 1.5;
/// Bars this far below the loudest are treated as silence around the music.
const SILENCE_BELOW_PEAK_DB: f64 = 40.0;
/// Without a tempo, the track is measured in windows of this length.
const FALLBACK_BAR_SECONDS: f64 = 2.0;
/// The loudness given energy 1 and energy 10, in dB below full scale.
const ENERGY_FLOOR_DB: f64 = -32.0;
const ENERGY_CEILING_DB: f64 = -9.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cue {
    pub seconds: f64,
    /// Energy of the section this cue starts, 1 to 10.
    pub energy: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Structure {
    /// Energy of the track as a whole, 1 to 10, judged by its louder half so
    /// that a quiet intro and outro do not pull it down.
    pub energy: u8,
    /// Up to [`MAX_CUES`] cue points, in order.
    pub cues: Vec<Cue>,
}

/// Finds a track's sections. With a tempo, cue points fall on bar lines
/// counted from the first beat; without one, on a two-second grid. Returns
/// `None` for silence.
pub fn detect_structure(
    samples: &[f32],
    sample_rate: u32,
    tempo: Option<Tempo>,
) -> Option<Structure> {
    let (bar_seconds, first_bar_seconds) = match tempo {
        Some(tempo) => (4.0 * 60.0 / tempo.bpm, tempo.first_beat_seconds),
        None => (FALLBACK_BAR_SECONDS, 0.0),
    };
    let frame = |seconds: f64| ((seconds * sample_rate as f64) as usize).min(samples.len());
    let bar_start = |bar: usize| first_bar_seconds + bar as f64 * bar_seconds;

    // Loudness of each bar, in dB below full scale.
    let mut loudness = Vec::new();
    loop {
        let (from, to) = (
            frame(bar_start(loudness.len())),
            frame(bar_start(loudness.len() + 1)),
        );
        if to <= from || to - from < frame(bar_seconds) / 2 {
            break;
        }
        let power: f64 = samples[from..to].iter().map(|s| (*s as f64).powi(2)).sum();
        loudness.push(10.0 * (power / (to - from) as f64 + 1e-12).log10());
    }
    let peak = loudness.iter().copied().fold(f64::MIN, f64::max);
    if loudness.is_empty() || peak < -90.0 {
        return None;
    }

    // The music runs from the first to the last bar that is not silent.
    let audible = |db: &f64| *db > peak - SILENCE_BELOW_PEAK_DB;
    let first = loudness.iter().position(audible)?;
    let last = loudness.iter().rposition(audible)? + 1;
    let mean = |from: usize, to: usize| {
        let bars = &loudness[from.min(last)..to.min(last)];
        bars.iter().sum::<f64>() / bars.len().max(1) as f64
    };

    // Every phrase boundary, with how much the loudness changes across it.
    let mut boundaries: Vec<(usize, f64)> = (first + PHRASE_BARS..last)
        .step_by(PHRASE_BARS)
        .filter(|bar| last - bar >= PHRASE_BARS)
        .map(|bar| {
            let before = mean(bar.saturating_sub(COMPARE_BARS).max(first), bar);
            let after = mean(bar, bar + COMPARE_BARS);
            (bar, (after - before).abs())
        })
        .filter(|(_, change)| *change >= MIN_CHANGE_DB)
        .collect();
    boundaries.sort_by(|a, b| b.1.total_cmp(&a.1));

    // Keep the biggest changes, spaced apart, after the start of the music.
    let mut starts = vec![first];
    for (bar, _) in boundaries {
        if starts.len() == MAX_CUES {
            break;
        }
        if starts
            .iter()
            .all(|kept| kept.abs_diff(bar) >= MIN_SECTION_BARS)
        {
            starts.push(bar);
        }
    }
    starts.sort_unstable();

    let cues = starts
        .iter()
        .enumerate()
        .map(|(index, &bar)| {
            let end = starts.get(index + 1).copied().unwrap_or(last);
            Cue {
                seconds: bar_start(bar).max(0.0),
                energy: energy_level(mean(bar, end)),
            }
        })
        .collect();

    let mut active: Vec<f64> = loudness[first..last].to_vec();
    active.sort_by(|a, b| b.total_cmp(a));
    let louder_half = &active[..active.len().div_ceil(2)];
    let energy = energy_level(louder_half.iter().sum::<f64>() / louder_half.len() as f64);
    Some(Structure { energy, cues })
}

/// Loudness on a scale of 1 to 10.
fn energy_level(db: f64) -> u8 {
    let position = (db - ENERGY_FLOOR_DB) / (ENERGY_CEILING_DB - ENERGY_FLOOR_DB);
    (1.0 + 9.0 * position.clamp(0.0, 1.0)).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 11_025;
    const BPM: f64 = 120.0;
    const FIRST_BEAT: f64 = 0.3;
    /// At 120 BPM a bar lasts two seconds.
    const BAR: f64 = 2.0;

    /// Noise at a given level for each `(bars, level)` section in turn,
    /// after `FIRST_BEAT` seconds of silence.
    fn track(sections: &[(usize, f32)]) -> Vec<f32> {
        let mut noise = 0x1234_5678u32;
        let mut samples = vec![0.0; (FIRST_BEAT * RATE as f64) as usize];
        for &(bars, level) in sections {
            for _ in 0..(bars as f64 * BAR * RATE as f64) as usize {
                noise ^= noise << 13;
                noise ^= noise >> 17;
                noise ^= noise << 5;
                samples.push(level * (noise as f32 / u32::MAX as f32 * 2.0 - 1.0));
            }
        }
        samples
    }

    fn tempo() -> Option<Tempo> {
        Some(Tempo {
            bpm: BPM,
            first_beat_seconds: FIRST_BEAT,
            confidence: 1.0,
        })
    }

    #[test]
    fn cues_mark_where_sections_start() {
        // Intro, drop, break, drop, outro.
        let samples = track(&[(16, 0.03), (32, 0.6), (16, 0.06), (32, 0.6), (16, 0.03)]);
        let structure = detect_structure(&samples, RATE, tempo()).unwrap();

        let bars: Vec<f64> = structure
            .cues
            .iter()
            .map(|cue| (cue.seconds - FIRST_BEAT) / BAR)
            .collect();
        let expected = [0.0, 16.0, 48.0, 64.0, 96.0];
        assert_eq!(bars.len(), expected.len(), "{bars:?}");
        for (bar, expected) in bars.iter().zip(expected) {
            assert!((bar - expected).abs() < 0.01, "{bars:?}");
        }

        let energy: Vec<u8> = structure.cues.iter().map(|cue| cue.energy).collect();
        assert!(energy[0] <= 3 && energy[4] <= 3, "{energy:?}");
        assert!(energy[1] >= 9 && energy[3] >= 9, "{energy:?}");
        assert!(energy[2] > energy[0] && energy[2] < energy[1], "{energy:?}");
        // The whole track is judged by its drops, not its intro.
        assert!(structure.energy >= 9);
    }

    #[test]
    fn a_steady_track_has_one_cue_at_its_start() {
        let samples = track(&[(64, 0.3)]);
        let structure = detect_structure(&samples, RATE, tempo()).unwrap();
        assert_eq!(structure.cues.len(), 1);
        assert!((structure.cues[0].seconds - FIRST_BEAT).abs() < 1e-9);
    }

    #[test]
    fn there_are_never_more_than_eight_cues() {
        let sections: Vec<(usize, f32)> = (0..24)
            .map(|i| (8, if i % 2 == 0 { 0.05 } else { 0.5 }))
            .collect();
        let structure = detect_structure(&track(&sections), RATE, tempo()).unwrap();
        assert_eq!(structure.cues.len(), MAX_CUES);
        assert!(
            structure
                .cues
                .windows(2)
                .all(|pair| pair[0].seconds < pair[1].seconds)
        );
    }

    #[test]
    fn works_without_a_tempo_and_rejects_silence() {
        let samples = track(&[(16, 0.03), (32, 0.6)]);
        let structure = detect_structure(&samples, RATE, None).unwrap();
        assert_eq!(structure.cues.len(), 2);
        // On a two-second grid from zero, the change is found within a bar.
        assert!((structure.cues[1].seconds - (FIRST_BEAT + 32.0)).abs() <= BAR);

        assert_eq!(
            detect_structure(&vec![0.0; RATE as usize * 60], RATE, None),
            None
        );
        assert_eq!(detect_structure(&[], RATE, None), None);
    }
}
