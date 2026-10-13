use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig, SupportedStreamConfig};

use crate::EngineError;
use crate::mixer::Mixer;
use crate::track::CHANNELS;

/// Frames rendered per pass when the device format needs converting.
const CONVERT_FRAMES: usize = 1024;

pub(crate) fn default_device() -> Result<(cpal::Device, SupportedStreamConfig), EngineError> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or(EngineError::NoOutputDevice)?;
    let config = device
        .default_output_config()
        .map_err(|e| EngineError::Device(e.to_string()))?;
    Ok((device, config))
}

pub(crate) fn start(
    device: &cpal::Device,
    config: SupportedStreamConfig,
    mixer: Mixer,
) -> Result<cpal::Stream, EngineError> {
    let format = config.sample_format();
    let config: StreamConfig = config.into();
    let stream = match format {
        SampleFormat::F32 => build::<f32>(device, config, mixer),
        SampleFormat::I16 => build::<i16>(device, config, mixer),
        SampleFormat::U16 => build::<u16>(device, config, mixer),
        SampleFormat::I32 => build::<i32>(device, config, mixer),
        other => Err(EngineError::Device(format!(
            "unsupported sample format {other}"
        ))),
    }?;
    stream
        .play()
        .map_err(|e| EngineError::Device(e.to_string()))?;
    Ok(stream)
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut mixer: Mixer,
) -> Result<cpal::Stream, EngineError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let mut stereo = vec![0f32; CONVERT_FRAMES * CHANNELS];
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                for chunk in data.chunks_mut(CONVERT_FRAMES * channels) {
                    let frames = chunk.len() / channels;
                    let stereo = &mut stereo[..frames * CHANNELS];
                    mixer.render(stereo);
                    for (out, input) in chunk.chunks_mut(channels).zip(stereo.chunks(CHANNELS)) {
                        if channels == 1 {
                            out[0] = T::from_sample((input[0] + input[1]) * 0.5);
                        } else {
                            out[0] = T::from_sample(input[0]);
                            out[1] = T::from_sample(input[1]);
                            out[2..].fill(T::EQUILIBRIUM);
                        }
                    }
                }
            },
            |error| eprintln!("audio output error: {error}"),
            None,
        )
        .map_err(|e| EngineError::Device(e.to_string()))
}
