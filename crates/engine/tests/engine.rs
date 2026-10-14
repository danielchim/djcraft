use std::f32::consts::TAU;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use djcraft_engine::{Engine, Mixer, Track};
use djcraft_protocol::{Action, DeckAction, MixerAction, Rejection, Role};

const RATE: u32 = 48_000;
/// Frames per simulated audio callback.
const CALLBACK: usize = 480;

fn sine(frequency: f32, seconds: f32, rate: u32) -> Vec<f32> {
    (0..(seconds * rate as f32) as usize)
        .flat_map(|i| {
            let sample = 0.5 * (TAU * frequency * i as f32 / rate as f32).sin();
            [sample, sample]
        })
        .collect()
}

/// A track whose sample value is its own position, from 0.0 to 1.0.
fn ramp(seconds: f32) -> Vec<f32> {
    let frames = (seconds * RATE as f32) as usize;
    (0..frames)
        .flat_map(|i| [i as f32 / frames as f32; 2])
        .collect()
}

fn deck(action: DeckAction) -> Action {
    Action::Deck { deck: 0, action }
}

fn engine_with(samples: Vec<f32>) -> (Engine, Mixer) {
    let (mut engine, mixer) = Engine::headless(RATE);
    engine
        .load_track(0, Arc::new(Track::from_samples("test", RATE, samples)))
        .unwrap();
    (engine, mixer)
}

/// Renders `seconds` of audio and returns the left channel.
fn render(mixer: &mut Mixer, seconds: f32) -> Vec<f32> {
    let mut left = Vec::new();
    let mut buffer = vec![0f32; CALLBACK * 2];
    for _ in 0..(seconds * RATE as f32) as usize / CALLBACK {
        mixer.render(&mut buffer);
        left.extend(buffer.chunks(2).map(|frame| frame[0]));
    }
    left
}

fn frequency(samples: &[f32]) -> f32 {
    let crossings = samples
        .windows(2)
        .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
        .count();
    crossings as f32 * RATE as f32 / samples.len() as f32
}

fn position(engine: &mut Engine) -> f64 {
    engine.state().decks[0].position_seconds
}

#[test]
fn plays_forward_at_original_speed() {
    let (mut engine, mut mixer) = engine_with(sine(440.0, 3.0, RATE));
    engine.apply(Role::Host, deck(DeckAction::Play)).unwrap();
    let out = render(&mut mixer, 1.0);

    assert!((position(&mut engine) - 1.0).abs() < 0.01);
    assert!((frequency(&out[RATE as usize / 2..]) - 440.0).abs() < 4.0);
    assert!(engine.state().decks[0].playing);
}

#[test]
fn tempo_without_key_lock_shifts_pitch() {
    let (mut engine, mut mixer) = engine_with(sine(440.0, 4.0, RATE));
    engine
        .apply(Role::Host, deck(DeckAction::SetTempo { ratio: 1.25 }))
        .unwrap();
    engine.apply(Role::Host, deck(DeckAction::Play)).unwrap();
    let out = render(&mut mixer, 2.0);

    assert!((position(&mut engine) - 2.5).abs() < 0.02);
    assert!((frequency(&out[RATE as usize..]) - 550.0).abs() < 5.0);
}

#[test]
fn key_lock_changes_tempo_but_keeps_pitch() {
    let (mut engine, mut mixer) = engine_with(sine(440.0, 4.0, RATE));
    for action in [
        DeckAction::SetTempo { ratio: 1.25 },
        DeckAction::SetKeyLock { on: true },
        DeckAction::Play,
    ] {
        engine.apply(Role::Host, deck(action)).unwrap();
    }
    let out = render(&mut mixer, 2.0);

    // The reported position trails the read position by the stretcher's
    // latency, so allow for that.
    let position = position(&mut engine);
    assert!((2.2..=2.5).contains(&position), "position {position}");
    let pitch = frequency(&out[RATE as usize..]);
    assert!((pitch - 440.0).abs() < 5.0, "pitch {pitch}");
    let peak = out[RATE as usize..]
        .iter()
        .fold(0f32, |p, s| p.max(s.abs()));
    assert!((0.4..=0.6).contains(&peak), "peak {peak}");
}

#[test]
fn scratch_plays_in_reverse() {
    let (mut engine, mut mixer) = engine_with(ramp(2.0));
    engine
        .apply(Role::Host, deck(DeckAction::Seek { seconds: 1.0 }))
        .unwrap();
    engine
        .apply(Role::Host, deck(DeckAction::Scratch { speed: Some(-1.0) }))
        .unwrap();
    let out = render(&mut mixer, 0.5);

    assert!((position(&mut engine) - 0.5).abs() < 0.01);
    // The ramp reads 0.5 at one second and 0.25 at half a second.
    assert!((out[0] - 0.5).abs() < 0.01);
    assert!((out.last().unwrap() - 0.25).abs() < 0.01);
    assert!(out.windows(2).all(|pair| pair[1] <= pair[0]));
}

#[test]
fn scratch_follows_near_zero_speed() {
    let (mut engine, mut mixer) = engine_with(ramp(2.0));
    engine
        .apply(Role::Host, deck(DeckAction::Seek { seconds: 1.0 }))
        .unwrap();
    engine
        .apply(Role::Host, deck(DeckAction::Scratch { speed: Some(0.001) }))
        .unwrap();
    let out = render(&mut mixer, 1.0);

    // One second at a thousandth of normal speed moves one millisecond.
    assert!((position(&mut engine) - 1.001).abs() < 0.0001);
    assert!(out.iter().all(|s| (s - 0.5).abs() < 0.001));

    // Releasing the scratch hands control back to the paused transport.
    engine
        .apply(Role::Host, deck(DeckAction::Scratch { speed: None }))
        .unwrap();
    render(&mut mixer, 0.5);
    let stopped = position(&mut engine);
    render(&mut mixer, 0.5);
    assert_eq!(position(&mut engine), stopped);
}

#[test]
fn stops_at_the_end_of_the_track() {
    let (mut engine, mut mixer) = engine_with(sine(440.0, 0.1, RATE));
    engine.apply(Role::Host, deck(DeckAction::Play)).unwrap();
    let out = render(&mut mixer, 0.5);

    let state = engine.state();
    assert!(!state.decks[0].playing);
    assert!((state.decks[0].position_seconds - 0.1).abs() < 0.001);
    assert!(out[RATE as usize / 4..].iter().all(|s| *s == 0.0));
}

#[test]
fn volume_and_master_gain_scale_the_output() {
    let (mut engine, mut mixer) = engine_with(sine(440.0, 2.0, RATE));
    engine
        .apply(Role::Host, deck(DeckAction::SetVolume { level: 0.5 }))
        .unwrap();
    engine
        .apply(
            Role::Host,
            Action::Mixer {
                action: MixerAction::SetMasterGain { level: 0.5 },
            },
        )
        .unwrap();
    engine.apply(Role::Host, deck(DeckAction::Play)).unwrap();
    let out = render(&mut mixer, 1.0);

    let peak = out[RATE as usize / 2..]
        .iter()
        .fold(0f32, |p, s| p.max(s.abs()));
    assert!((peak - 0.125).abs() < 0.005, "peak {peak}");
}

#[test]
fn rejects_actions_by_role_and_value() {
    let (mut engine, _mixer) = Engine::headless(RATE);
    let master = Action::Mixer {
        action: MixerAction::SetMasterGain { level: 0.5 },
    };

    assert_eq!(
        engine.apply(Role::Viewer, deck(DeckAction::Play)),
        Err(Rejection::NotPermitted)
    );
    assert_eq!(
        engine.apply(Role::CoDj, master.clone()),
        Err(Rejection::NotPermitted)
    );
    assert_eq!(engine.apply(Role::CoDj, deck(DeckAction::Play)), Ok(()));
    assert_eq!(engine.apply(Role::Host, master), Ok(()));
    assert_eq!(
        engine.apply(
            Role::Host,
            Action::Deck {
                deck: 9,
                action: DeckAction::Play
            }
        ),
        Err(Rejection::UnknownDeck)
    );
    assert_eq!(
        engine.apply(Role::Host, deck(DeckAction::SetTempo { ratio: 3.0 })),
        Err(Rejection::InvalidValue)
    );
    assert_eq!(
        engine.apply(
            Role::Host,
            deck(DeckAction::Scratch {
                speed: Some(f64::NAN)
            })
        ),
        Err(Rejection::InvalidValue)
    );
}

/// Writes a mono 16-bit WAV file and returns its path.
fn write_wav(name: &str, rate: u32, samples: &[f32]) -> PathBuf {
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|s| ((s * i16::MAX as f32) as i16).to_le_bytes())
        .collect();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&data);

    let path = std::env::temp_dir().join(format!("djcraft-{}-{name}.wav", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    path
}

fn wait_for_load(engine: &mut Engine) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.state().decks[0].loading {
        assert!(Instant::now() < deadline, "load timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn loads_a_file_and_resamples_it() {
    let mono: Vec<f32> = sine(440.0, 1.0, 44_100).chunks(2).map(|f| f[0]).collect();
    let path = write_wav("tone", 44_100, &mono);

    let (mut engine, mut mixer) = Engine::headless(RATE);
    let load = DeckAction::Load {
        path: path.to_string_lossy().into_owned(),
    };
    engine.apply(Role::Host, deck(load)).unwrap();
    // No check that it reads as loading here: a small file on a fast
    // machine can be ready before the first look.
    wait_for_load(&mut engine);
    // The deck is handed the track before decoding has necessarily finished.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !engine.state().decks[0].track.as_ref().unwrap().fully_loaded {
        assert!(Instant::now() < deadline, "decode timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::fs::remove_file(&path).unwrap();

    let state = engine.state();
    assert_eq!(state.decks[0].error, None);
    let info = state.decks[0].track.as_ref().unwrap();
    assert!(info.title.ends_with("-tone"));
    assert!((info.duration_seconds - 1.0).abs() < 0.01);
    assert_eq!(info.loaded_seconds, info.duration_seconds);
    let track = engine.track(0).unwrap();
    assert!((track.peak(track.peak_count() / 2) - 0.5).abs() < 0.02);

    engine.apply(Role::Host, deck(DeckAction::Play)).unwrap();
    let out = render(&mut mixer, 0.5);
    assert!((frequency(&out[RATE as usize / 4..]) - 440.0).abs() < 4.0);
}

#[test]
fn reports_a_failed_load() {
    let (mut engine, _mixer) = Engine::headless(RATE);
    let load = DeckAction::Load {
        path: "/nonexistent/track.mp3".into(),
    };
    engine.apply(Role::Host, deck(load)).unwrap();
    wait_for_load(&mut engine);

    let state = engine.state();
    assert!(state.decks[0].track.is_none());
    assert!(
        state.decks[0]
            .error
            .as_ref()
            .unwrap()
            .contains("cannot open file")
    );
}

#[test]
fn a_track_is_playable_before_it_is_fully_decoded() {
    let mono: Vec<f32> = sine(440.0, 10.0, 44_100).chunks(2).map(|f| f[0]).collect();
    let path = write_wav("long", 44_100, &mono);

    let mut early = None;
    Track::load(&path, RATE, None, |track| {
        // Called from the decoder, part-way through the file.
        assert!(!track.is_complete());
        assert!(track.decoded_frames() > 0);
        assert!(track.decoded_frames() < track.frames());
        assert!((track.duration_seconds() - 10.0).abs() < 0.01);
        early = Some(track);
    })
    .unwrap();
    std::fs::remove_file(&path).unwrap();

    let track = early.unwrap();
    assert!(track.is_complete());
    assert_eq!(track.frames(), 10 * RATE as usize);
    assert_eq!(track.decoded_frames(), track.frames());
    assert_eq!(
        track.peak_count(),
        track.frames().div_ceil(track.frames_per_peak())
    );
}

#[test]
fn playback_waits_for_audio_that_has_not_been_decoded() {
    let mono: Vec<f32> = sine(440.0, 10.0, 44_100).chunks(2).map(|f| f[0]).collect();
    let path = write_wav("stall", 44_100, &mono);
    let (mut engine, mut mixer) = Engine::headless(RATE);

    // Everything inside the callback runs while the decoder is paused after
    // its first packet, so only a fraction of a second exists.
    Track::load(&path, RATE, None, |track| {
        let decoded = track.decoded_seconds();
        assert!(decoded < 1.0);
        engine.load_track(0, track).unwrap();
        engine.apply(Role::Host, deck(DeckAction::Play)).unwrap();
        render(&mut mixer, 1.0);

        let state = engine.state();
        assert!(state.decks[0].playing);
        assert!((state.decks[0].position_seconds - decoded).abs() < 0.001);
        assert!(!state.decks[0].track.as_ref().unwrap().fully_loaded);
    })
    .unwrap();
    std::fs::remove_file(&path).unwrap();

    // Once the rest has arrived, playback carries on from where it waited.
    let waited = position(&mut engine);
    render(&mut mixer, 1.0);
    assert!((position(&mut engine) - waited - 1.0).abs() < 0.01);
}

#[test]
fn a_cached_waveform_is_complete_as_soon_as_the_track_opens() {
    let mono: Vec<f32> = sine(440.0, 10.0, 44_100).chunks(2).map(|f| f[0]).collect();
    let path = write_wav("cached", 44_100, &mono);
    let cache = std::env::temp_dir().join(format!("djcraft-cache-{}", std::process::id()));

    // First load: nothing cached, so the waveform grows with the decode.
    let mut first = None;
    Track::load(&path, RATE, Some(&cache), |track| {
        assert!(track.peak_count() < 100);
        first = Some(track);
    })
    .unwrap();
    let first = first.unwrap();
    let peaks = first.peak_count();
    assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1);

    // Second load, under a new name: the whole waveform is there before
    // most of the audio is.
    let moved = path.with_file_name(format!("djcraft-{}-moved.wav", std::process::id()));
    std::fs::rename(&path, &moved).unwrap();
    Track::load(&moved, RATE, Some(&cache), |track| {
        assert!(!track.is_complete());
        assert_eq!(track.peak_count(), peaks);
        assert_eq!(track.frames(), first.frames());
        for i in 0..peaks {
            assert!((track.peak(i) - first.peak(i)).abs() < 0.01);
        }
    })
    .unwrap();

    // A damaged entry is ignored and replaced.
    let entry = std::fs::read_dir(&cache)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(&entry, b"not a waveform").unwrap();
    let mut rebuilt = None;
    Track::load(&moved, RATE, Some(&cache), |track| {
        assert!(track.peak_count() < 100);
        // Decoding stops if nobody keeps the track.
        rebuilt = Some(track);
    })
    .unwrap();
    assert!(std::fs::metadata(&entry).unwrap().len() > 1000);

    std::fs::remove_file(&moved).unwrap();
    std::fs::remove_dir_all(&cache).unwrap();
}

#[test]
fn a_waveform_cached_at_one_rate_serves_another() {
    // Loud for the first half, quiet for the second.
    let mono: Vec<f32> = sine(440.0, 10.0, 44_100)
        .chunks(2)
        .enumerate()
        .map(|(i, f)| if i < 220_500 { f[0] } else { f[0] * 0.25 })
        .collect();
    let path = write_wav("rates", 44_100, &mono);
    let cache = std::env::temp_dir().join(format!("djcraft-rates-{}", std::process::id()));

    // Analysis decodes at a low rate; the decks play at a high one.
    let analysed = Track::decode_cached(&path, 22_050, Some(&cache)).unwrap();
    assert!(analysed.is_complete());
    Track::load(&path, 48_000, Some(&cache), |track| {
        assert!(!track.is_complete());
        let peaks = track.peak_count();
        assert_eq!(peaks, (480_000usize).div_ceil(track.frames_per_peak()));
        assert!(track.frames().abs_diff(480_000) <= 1);
        let (loud, quiet) = (track.peak(peaks / 4), track.peak(peaks * 3 / 4));
        assert!((loud - analysed.peak(analysed.peak_count() / 4)).abs() < 0.01);
        assert!((quiet * 4.0 - loud).abs() < 0.02, "{loud} then {quiet}");
    })
    .unwrap();

    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir_all(&cache).unwrap();
}
