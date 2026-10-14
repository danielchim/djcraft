//! Acoustic fingerprints, which identify a recording by its sound rather
//! than its tags. Made the way Chromaprint's `fpcalc` makes them, so that
//! AcoustID can look them up.

use rusty_chromaprint::{Configuration, FingerprintCompressor, Fingerprinter};

/// AcoustID matches on the start of a track; more adds nothing.
const FINGERPRINT_SECONDS: usize = 120;
/// Shorter audio has too little to identify it by.
const MIN_SECONDS: f64 = 3.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Fingerprint {
    /// The compressed fingerprint in URL-safe base64, as AcoustID takes it.
    pub encoded: String,
    /// Length of the whole track in whole seconds, which AcoustID also needs.
    pub duration_seconds: u32,
}

/// Fingerprints mono audio. Returns `None` for audio too short to identify.
pub fn fingerprint(samples: &[f32], sample_rate: u32) -> Option<Fingerprint> {
    let duration = samples.len() as f64 / sample_rate as f64;
    if duration < MIN_SECONDS || sample_rate == 0 {
        return None;
    }
    // The algorithm AcoustID's own tools use by default.
    let config = Configuration::preset_test2();
    let mut printer = Fingerprinter::new(&config);
    printer.start(sample_rate, 1).ok()?;
    let take = samples
        .len()
        .min(FINGERPRINT_SECONDS * sample_rate as usize);
    let pcm: Vec<i16> = samples[..take]
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect();
    printer.consume(&pcm);
    printer.finish();
    let raw = printer.fingerprint();
    if raw.is_empty() {
        return None;
    }
    let compressed = FingerprintCompressor::from(&config).compress(raw);
    Some(Fingerprint {
        encoded: base64_url(&compressed),
        duration_seconds: duration.round() as u32,
    })
}

/// Base64 with the URL-safe alphabet and no padding, as `fpcalc` writes it.
fn base64_url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = chunk.iter().enumerate().fold(0u32, |value, (i, byte)| {
            value | (*byte as u32) << (16 - 8 * i)
        });
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(value >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A few notes with overtones, so there is something to fingerprint.
    fn music(rate: u32, seconds: f32, shift: f32) -> Vec<f32> {
        (0..(seconds * rate as f32) as usize)
            .map(|i| {
                let t = i as f32 / rate as f32;
                let note = [220.0, 277.2, 329.6, 440.0][(t * 2.0) as usize % 4] * shift;
                0.4 * (std::f32::consts::TAU * note * t).sin()
                    + 0.2 * (std::f32::consts::TAU * note * 2.0 * t).sin()
            })
            .collect()
    }

    #[test]
    fn encodes_base64_the_url_safe_way() {
        assert_eq!(base64_url(b""), "");
        assert_eq!(base64_url(b"f"), "Zg");
        assert_eq!(base64_url(b"fo"), "Zm8");
        assert_eq!(base64_url(b"foo"), "Zm9v");
        assert_eq!(base64_url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn the_same_sound_gives_the_same_fingerprint() {
        let one = fingerprint(&music(11_025, 20.0, 1.0), 11_025).unwrap();
        let again = fingerprint(&music(11_025, 20.0, 1.0), 11_025).unwrap();
        let other = fingerprint(&music(11_025, 20.0, 1.5), 11_025).unwrap();
        assert_eq!(one, again);
        assert_ne!(one.encoded, other.encoded);
        assert_eq!(one.duration_seconds, 20);
        // Compressed fingerprints start with the algorithm number.
        assert!(one.encoded.starts_with("AQ"), "{}", one.encoded);
        assert_eq!(fingerprint(&music(11_025, 1.0, 1.0), 11_025), None);
    }
}
