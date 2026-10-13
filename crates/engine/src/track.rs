use std::fmt;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;

use crate::cache;

/// The engine mixes in stereo; every track is converted on load.
pub const CHANNELS: usize = 2;

const FRAMES_PER_PEAK: usize = 256;
const RESAMPLE_CHUNK: usize = 1024;

/// A decoded track, held in memory at the engine's sample rate.
///
/// A track can be shared while it is still being decoded: one loader thread
/// appends audio, and the audio thread and UI read whatever has arrived so
/// far. Samples are stored as atomics so that this needs no locks.
pub struct Track {
    pub title: String,
    pub sample_rate: u32,
    /// Interleaved stereo, as `f32` bits.
    samples: Box<[AtomicU32]>,
    /// Peak amplitude of each block of `FRAMES_PER_PEAK` frames, as `f32` bits.
    peaks: Box<[AtomicU32]>,
    /// Peaks already known from the waveform cache, ahead of decoding.
    cached_peaks: usize,
    /// Length reported until decoding finishes and the real one is known.
    estimated_frames: usize,
    decoded_frames: AtomicUsize,
    complete: AtomicBool,
}

#[derive(Debug)]
pub struct DecodeError(String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DecodeError {}

fn err(context: &str, source: impl fmt::Display) -> DecodeError {
    DecodeError(format!("{context}: {source}"))
}

/// Length of `frames` at rate `from` once converted to rate `to`.
fn scale_frames(frames: u64, from: u32, to: u32) -> usize {
    (frames * to as u64).div_ceil(from as u64) as usize
}

fn zeroed(len: usize) -> Box<[AtomicU32]> {
    (0..len).map(|_| AtomicU32::new(0)).collect()
}

impl Track {
    /// An empty track with room for `capacity` frames.
    fn with_capacity(title: String, sample_rate: u32, estimated_frames: usize, capacity: usize) -> Self {
        Self {
            title,
            sample_rate,
            samples: zeroed(capacity * CHANNELS),
            peaks: zeroed(capacity.div_ceil(FRAMES_PER_PEAK)),
            cached_peaks: 0,
            estimated_frames,
            decoded_frames: AtomicUsize::new(0),
            complete: AtomicBool::new(false),
        }
    }

    /// A complete track from interleaved stereo samples.
    pub fn from_samples(title: impl Into<String>, sample_rate: u32, samples: Vec<f32>) -> Self {
        assert_eq!(samples.len() % CHANNELS, 0);
        let frames = samples.len() / CHANNELS;
        let track = Self::with_capacity(title.into(), sample_rate, frames, frames);
        track.append(&samples);
        track.finish();
        track
    }

    /// Adds decoded audio, dropping anything beyond the track's capacity.
    /// Only the one thread loading the track may call this.
    fn append(&self, samples: &[f32]) {
        let start = self.decoded_frames.load(Ordering::Relaxed);
        let capacity = self.samples.len() / CHANNELS;
        let frames = (samples.len() / CHANNELS).min(capacity - start);
        let end = start + frames;
        for (slot, sample) in self.samples[start * CHANNELS..end * CHANNELS]
            .iter()
            .zip(samples)
        {
            slot.store(sample.to_bits(), Ordering::Relaxed);
        }
        // Recompute every peak block the new audio touches.
        for block in start / FRAMES_PER_PEAK..end.div_ceil(FRAMES_PER_PEAK) {
            let range = block * FRAMES_PER_PEAK * CHANNELS
                ..((block + 1) * FRAMES_PER_PEAK).min(end) * CHANNELS;
            let peak = self.samples[range]
                .iter()
                .map(|slot| f32::from_bits(slot.load(Ordering::Relaxed)).abs())
                .fold(0f32, f32::max);
            self.peaks[block].store(peak.min(1.0).to_bits(), Ordering::Relaxed);
        }
        self.decoded_frames.store(end, Ordering::Release);
    }

    fn finish(&self) {
        self.complete.store(true, Ordering::Release);
    }

    /// Frames decoded so far; only these can be played.
    pub fn decoded_frames(&self) -> usize {
        self.decoded_frames.load(Ordering::Acquire)
    }

    /// Whether the whole track has been decoded.
    pub fn is_complete(&self) -> bool {
        self.complete.load(Ordering::Acquire)
    }

    /// Track length in frames. An estimate until decoding completes.
    pub fn frames(&self) -> usize {
        let decoded = self.decoded_frames();
        if self.is_complete() {
            // Re-read in case the last append landed between the two loads.
            self.decoded_frames()
        } else {
            self.estimated_frames.max(decoded)
        }
    }

    pub fn duration_seconds(&self) -> f64 {
        self.frames() as f64 / self.sample_rate as f64
    }

    pub fn decoded_seconds(&self) -> f64 {
        self.decoded_frames() as f64 / self.sample_rate as f64
    }

    /// One sample by index into the interleaved buffer.
    pub(crate) fn sample(&self, index: usize) -> f32 {
        f32::from_bits(self.samples[index].load(Ordering::Relaxed))
    }

    /// Copies interleaved samples starting at `frame` into `out`.
    pub(crate) fn read(&self, frame: usize, out: &mut [f32]) {
        let start = frame * CHANNELS;
        let slots = &self.samples[start..start + out.len()];
        for (out, slot) in out.iter_mut().zip(slots) {
            *out = f32::from_bits(slot.load(Ordering::Relaxed));
        }
    }

    pub fn frames_per_peak(&self) -> usize {
        FRAMES_PER_PEAK
    }

    /// Number of waveform peaks available so far: all of them if the
    /// waveform was cached, otherwise as many as have been decoded.
    pub fn peak_count(&self) -> usize {
        self.decoded_frames()
            .div_ceil(FRAMES_PER_PEAK)
            .max(self.cached_peaks)
    }

    /// Peak amplitude, 0.0 to 1.0, of one block of `frames_per_peak` frames.
    pub fn peak(&self, index: usize) -> f32 {
        f32::from_bits(self.peaks[index].load(Ordering::Relaxed))
    }

    /// Decodes a whole file, resampled to `sample_rate`. Blocking.
    pub fn decode(path: &Path, sample_rate: u32) -> Result<Arc<Self>, DecodeError> {
        let mut track = None;
        Self::load(path, sample_rate, None, |opened| track = Some(opened))?;
        Ok(track.expect("load succeeded without opening a track"))
    }

    /// Decodes a file, calling `opened` with the track as soon as it can be
    /// played and then filling in the rest. Blocking; run it on its own
    /// thread. Stops early if every other reference to the track is dropped.
    ///
    /// With a `cache_dir`, the waveform of a track loaded before is available
    /// in full from the start, and new waveforms are saved there.
    pub fn load(
        path: &Path,
        sample_rate: u32,
        cache_dir: Option<&Path>,
        opened: impl FnOnce(Arc<Self>),
    ) -> Result<(), DecodeError> {
        let entry = cache_dir.and_then(|dir| cache::Entry::for_file(dir, path).ok());
        let cached = entry
            .as_ref()
            .and_then(|entry| entry.read(sample_rate, FRAMES_PER_PEAK));
        let save = |track: &Self| {
            if let (Some(entry), None) = (&entry, &cached) {
                let peaks = (0..track.peak_count()).map(|i| track.peak(i));
                // Best effort: without it the next load just decodes again.
                let _ = entry.write(sample_rate, FRAMES_PER_PEAK, track.frames(), peaks);
            }
        };
        let title = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut source = Source::open(path)?;
        let mut packet = Vec::new();
        let source_rate = source
            .next(&mut packet)?
            .ok_or_else(|| DecodeError("no audio decoded".into()))?;
        let mut converter = Converter::new(source_rate, sample_rate)?;
        let mut converted = Vec::new();

        let source_frames = match (&cached, source.frames) {
            (Some(cached), _) => Some(cached.frames),
            (None, Some(frames)) => Some(scale_frames(frames, source_rate, sample_rate)),
            (None, None) => None,
        };
        let Some(estimated) = source_frames else {
            // Without a length there is nothing to size the buffer by, so
            // decode everything before handing the track over.
            let mut samples = Vec::new();
            loop {
                converter.push(&packet, &mut samples)?;
                if source.next(&mut packet)?.is_none() {
                    break;
                }
            }
            converter.finish(&mut samples)?;
            let track = Self::from_samples(title, sample_rate, samples);
            save(&track);
            opened(Arc::new(track));
            return Ok(());
        };

        // Headers are not always exact; leave a second of headroom.
        let capacity = estimated + sample_rate as usize;
        let mut track = Self::with_capacity(title, sample_rate, estimated, capacity);
        if let Some(cached) = &cached {
            for (slot, peak) in track.peaks.iter().zip(&cached.peaks) {
                slot.store(peak.to_bits(), Ordering::Relaxed);
            }
            track.cached_peaks = cached.peaks.len().min(track.peaks.len());
        }
        let track = Arc::new(track);
        let mut opened = Some(opened);
        loop {
            converted.clear();
            converter.push(&packet, &mut converted)?;
            track.append(&converted);
            if let Some(opened) = opened.take() {
                opened(track.clone());
            } else if Arc::strong_count(&track) == 1 {
                return Ok(());
            }
            // Once the track is playing, a damaged file just ends early.
            match source.next(&mut packet) {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        converted.clear();
        converter.finish(&mut converted)?;
        track.append(&converted);
        track.finish();
        save(&track);
        Ok(())
    }
}

/// An audio file being decoded packet by packet.
struct Source {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    /// Length from the file header, in frames at the source rate.
    frames: Option<u64>,
    interleaved: Vec<f32>,
    decoded_any: bool,
}

impl Source {
    fn open(path: &Path) -> Result<Self, DecodeError> {
        let file = File::open(path).map_err(|e| err("cannot open file", e))?;
        let stream = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(extension);
        }
        let format = symphonia::default::get_probe()
            .probe(&hint, stream, Default::default(), Default::default())
            .map_err(|e| err("unsupported format", e))?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| DecodeError("no audio track".into()))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|params| params.audio())
            .ok_or_else(|| DecodeError("no audio codec parameters".into()))?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|e| err("unsupported codec", e))?;
        Ok(Self {
            track_id: track.id,
            frames: track.num_frames,
            format,
            decoder,
            interleaved: Vec::new(),
            decoded_any: false,
        })
    }

    /// Decodes the next packet into `out` as interleaved stereo and returns
    /// its sample rate, or `None` at the end of the file.
    fn next(&mut self, out: &mut Vec<f32>) -> Result<Option<u32>, DecodeError> {
        out.clear();
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(None),
                // A truncated file still yields everything decoded so far.
                Err(SymphoniaError::IoError(_)) if self.decoded_any => return Ok(None),
                Err(e) => return Err(err("read failed", e)),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let buffer = match self.decoder.decode(&packet) {
                Ok(buffer) => buffer,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(e) => return Err(err("decode failed", e)),
            };
            let channels = buffer.spec().channels().count();
            buffer.copy_to_vec_interleaved(&mut self.interleaved);
            if self.interleaved.is_empty() {
                continue;
            }
            for frame in self.interleaved.chunks_exact(channels) {
                let right = if channels > 1 { frame[1] } else { frame[0] };
                out.extend_from_slice(&[frame[0], right]);
            }
            self.decoded_any = true;
            return Ok(Some(buffer.spec().rate()));
        }
    }
}

/// Converts a stream of stereo audio to the engine's sample rate.
struct Converter {
    /// `None` when the rates already match.
    resampler: Option<Fft<f32>>,
    pending: Vec<f32>,
    output: Vec<f32>,
    /// Frames of resampler start-up delay still to drop from the output.
    to_trim: usize,
    from: u32,
    to: u32,
    frames_in: usize,
    frames_out: usize,
}

impl Converter {
    fn new(from: u32, to: u32) -> Result<Self, DecodeError> {
        let mut resampler = None;
        let mut output = Vec::new();
        let mut to_trim = 0;
        if from != to {
            let fft = Fft::<f32>::new(
                from as usize,
                to as usize,
                RESAMPLE_CHUNK,
                CHANNELS,
                FixedSync::Input,
            )
            .map_err(|e| err("cannot resample", e))?;
            output = vec![0.0; fft.output_frames_max() * CHANNELS];
            to_trim = fft.output_delay();
            resampler = Some(fft);
        }
        Ok(Self {
            resampler,
            pending: Vec::new(),
            output,
            to_trim,
            from,
            to,
            frames_in: 0,
            frames_out: 0,
        })
    }

    /// Resamples the first `valid` frames of one chunk taken from `pending`,
    /// appending the result to `out`.
    fn process(&mut self, valid: usize, out: &mut Vec<f32>) -> Result<(), DecodeError> {
        let resampler = self.resampler.as_mut().expect("process without a resampler");
        let chunk = resampler.input_frames_next();
        let output_frames = self.output.len() / CHANNELS;
        let input = InterleavedSlice::new(&self.pending[..chunk * CHANNELS], CHANNELS, chunk)
            .map_err(|e| err("cannot resample", e))?;
        let mut output = InterleavedSlice::new_mut(&mut self.output, CHANNELS, output_frames)
            .map_err(|e| err("cannot resample", e))?;
        let indexing = Indexing {
            input_offset: 0,
            output_offset: 0,
            active_channels_mask: None,
            partial_len: (valid < chunk).then_some(valid),
        };
        let (_, produced) = resampler
            .process_into_buffer(&input, &mut output, Some(&indexing))
            .map_err(|e| err("cannot resample", e))?;
        let skip = self.to_trim.min(produced);
        self.to_trim -= skip;
        out.extend_from_slice(&self.output[skip * CHANNELS..produced * CHANNELS]);
        self.frames_out += produced - skip;
        Ok(())
    }

    /// Feeds interleaved stereo in and appends converted audio to `out`.
    fn push(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), DecodeError> {
        let Some(resampler) = &self.resampler else {
            out.extend_from_slice(input);
            return Ok(());
        };
        let chunk = resampler.input_frames_next();
        self.frames_in += input.len() / CHANNELS;
        self.pending.extend_from_slice(input);
        while self.pending.len() >= chunk * CHANNELS {
            self.process(chunk, out)?;
            self.pending.drain(..chunk * CHANNELS);
        }
        Ok(())
    }

    /// Flushes what the resampler is still holding once the input has ended.
    fn finish(&mut self, out: &mut Vec<f32>) -> Result<(), DecodeError> {
        let Some(resampler) = &self.resampler else {
            return Ok(());
        };
        let chunk = resampler.input_frames_next();
        let expected = scale_frames(self.frames_in as u64, self.from, self.to);
        let start = out.len() / CHANNELS;
        let before = self.frames_out;

        let mut valid = self.pending.len() / CHANNELS;
        self.pending.resize(chunk * CHANNELS, 0.0);
        while self.frames_out < expected {
            self.process(valid, out)?;
            // After the last real audio, keep feeding silence.
            self.pending.fill(0.0);
            valid = 0;
        }
        out.truncate((start + expected.saturating_sub(before)) * CHANNELS);
        self.frames_out = expected;
        Ok(())
    }
}
