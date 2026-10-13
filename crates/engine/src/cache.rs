//! On-disk cache of track waveforms, so a track that has been loaded before
//! shows its whole waveform at once. Strictly best effort: a missing, stale
//! or damaged entry is ignored and rebuilt.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"DJCW";
const VERSION: u32 = 1;
const HEADER_LEN: usize = 24;
/// Bytes hashed from each end of a file to identify it.
const KEY_SAMPLE: u64 = 64 * 1024;

pub(crate) struct CachedWaveform {
    /// Exact track length at the cached sample rate.
    pub(crate) frames: usize,
    pub(crate) peaks: Vec<f32>,
}

/// Where the waveform for one audio file is cached.
pub(crate) struct Entry {
    path: PathBuf,
}

impl Entry {
    /// Identifies `audio` by its size and its first and last bytes, so the
    /// entry survives the file being moved or renamed.
    pub(crate) fn for_file(cache_dir: &Path, audio: &Path) -> io::Result<Self> {
        let mut file = File::open(audio)?;
        let len = file.metadata()?.len();
        let mut hash = Fnv::new();
        hash.write(&len.to_le_bytes());
        let mut buffer = Vec::new();
        file.by_ref().take(KEY_SAMPLE).read_to_end(&mut buffer)?;
        hash.write(&buffer);
        if len > KEY_SAMPLE {
            buffer.clear();
            file.seek(SeekFrom::Start(len - KEY_SAMPLE.min(len)))?;
            file.read_to_end(&mut buffer)?;
            hash.write(&buffer);
        }
        Ok(Self {
            path: cache_dir.join(format!("{:016x}.waveform", hash.0)),
        })
    }

    pub(crate) fn read(&self, sample_rate: u32, frames_per_peak: usize) -> Option<CachedWaveform> {
        let bytes = fs::read(&self.path).ok()?;
        let (header, peaks) = bytes.split_at_checked(HEADER_LEN)?;
        let u32_at = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
        let frames = u64::from_le_bytes(header[16..24].try_into().unwrap()) as usize;
        let valid = &header[..4] == MAGIC
            && u32_at(4) == VERSION
            && u32_at(8) == sample_rate
            && u32_at(12) as usize == frames_per_peak
            && peaks.len() == frames.div_ceil(frames_per_peak);
        valid.then(|| CachedWaveform {
            frames,
            peaks: peaks.iter().map(|peak| *peak as f32 / 255.0).collect(),
        })
    }

    pub(crate) fn write(
        &self,
        sample_rate: u32,
        frames_per_peak: usize,
        frames: usize,
        peaks: impl Iterator<Item = f32>,
    ) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(HEADER_LEN + frames.div_ceil(frames_per_peak));
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(frames_per_peak as u32).to_le_bytes());
        bytes.extend_from_slice(&(frames as u64).to_le_bytes());
        bytes.extend(peaks.map(|peak| (peak.clamp(0.0, 1.0) * 255.0).round() as u8));

        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        // Write to one side and rename, so a reader never sees half a file.
        let partial = self
            .path
            .with_extension(format!("{}.partial", std::process::id()));
        fs::write(&partial, bytes)?;
        fs::rename(&partial, &self.path)
    }
}

/// FNV-1a, 64-bit. Written out here because the key must not change between
/// Rust versions, which the standard library's hasher does not promise.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ *byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}
