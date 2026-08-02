use std::{
    collections::VecDeque,
    fmt::Display,
    sync::{Arc, Mutex},
    time::Duration,
};

use cpal::{
    Device, SampleFormat, Stream, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use nanocodex::oai::realtime::{REALTIME_SAMPLE_RATE, RealtimeAudio};
use nanocodex_audio::{CaptureConfig, CaptureError, CaptureStream, LinearResampler, Pcm16Chunk};
use tokio::sync::mpsc;

use crate::{AudioConfig, AudioError};

const PLAYBACK_INITIAL_HEADROOM: Duration = Duration::from_secs(1);

pub(crate) struct VoiceAudio {
    _input: CaptureStream,
    _output: Stream,
    playback: Playback,
}

impl VoiceAudio {
    pub(crate) fn open(
        policy: AudioConfig,
    ) -> Result<(Self, mpsc::Receiver<Pcm16Chunk>), AudioError> {
        if policy.maximum_playback_buffer().is_zero() {
            return Err(AudioError::InvalidConfig(
                "maximum playback buffer must be greater than zero",
            ));
        }
        if policy.playback_prebuffer() > policy.maximum_playback_buffer() {
            return Err(AudioError::InvalidConfig(
                "playback prebuffer cannot exceed maximum playback buffer",
            ));
        }

        let host = cpal::default_host();
        let output_device = host
            .default_output_device()
            .ok_or(AudioError::NoOutputDevice)?;

        let output_supported = output_device
            .default_output_config()
            .map_err(|error| backend("failed to read the default speaker format", error))?;
        let output_config: StreamConfig = output_supported.clone().into();
        let (input, microphone_rx) = CaptureStream::open(CaptureConfig {
            sample_rate_hz: REALTIME_SAMPLE_RATE,
            samples_per_chunk: 480,
        })
        .map_err(map_capture)?;
        let playback = Playback::new(output_config.sample_rate.0, output_config.channels, policy)?;
        let output = build_output(
            &output_device,
            output_supported.sample_format(),
            output_config,
            Arc::clone(&playback.buffer),
        )?;

        output
            .play()
            .map_err(|error| backend("failed to start audio output", error))?;
        Ok((
            Self {
                _input: input,
                _output: output,
                playback,
            },
            microphone_rx,
        ))
    }

    pub(crate) fn play(&mut self, audio: &RealtimeAudio) {
        self.playback.push(audio);
    }

    pub(crate) fn interrupt(&mut self) {
        self.playback.clear();
    }
}

struct Playback {
    buffer: Arc<Mutex<PlaybackBuffer>>,
    resampler: LinearResampler,
    resampled: Vec<f32>,
    maximum_frames: usize,
}

struct PlaybackBuffer {
    samples: VecDeque<f32>,
    prebuffer_frames: usize,
    buffering: bool,
}

impl Playback {
    fn new(sample_rate: u32, channels: u16, policy: AudioConfig) -> Result<Self, AudioError> {
        let channels = usize::from(channels);
        if channels == 0 {
            return Err(AudioError::InvalidConfig(
                "speaker channel count must be greater than zero",
            ));
        }
        let prebuffer_frames = duration_frames(sample_rate, policy.playback_prebuffer());
        let maximum_frames = duration_frames(sample_rate, policy.maximum_playback_buffer());
        if maximum_frames == 0 {
            return Err(AudioError::InvalidConfig(
                "maximum playback buffer is shorter than one device sample",
            ));
        }
        let initial_capacity = maximum_frames
            .min(prebuffer_frames.max(duration_frames(sample_rate, PLAYBACK_INITIAL_HEADROOM)));
        Ok(Self {
            buffer: Arc::new(Mutex::new(PlaybackBuffer {
                // Keep mono frames and fan out in the device callback instead
                // of duplicating every sample under this shared lock.
                samples: VecDeque::with_capacity(initial_capacity),
                prebuffer_frames,
                buffering: true,
            })),
            resampler: LinearResampler::new(REALTIME_SAMPLE_RATE, sample_rate),
            resampled: Vec::new(),
            maximum_frames,
        })
    }

    fn push(&mut self, audio: &RealtimeAudio) {
        let source = audio.as_bytes().chunks_exact(2).map(|sample| {
            let sample = i16::from_le_bytes([sample[0], sample[1]]);
            f32::from(sample) / f32::from(i16::MAX)
        });
        self.resampler.push_into(source, &mut self.resampled);
        let Ok(mut buffer) = self.buffer.lock() else {
            return;
        };
        let retained_frames = self.resampled.len().min(self.maximum_frames);
        let overflow = buffer
            .samples
            .len()
            .saturating_add(retained_frames)
            .saturating_sub(self.maximum_frames);
        let discarded = overflow.min(buffer.samples.len());
        buffer.samples.drain(..discarded);
        buffer.samples.extend(
            self.resampled
                .iter()
                .skip(self.resampled.len().saturating_sub(retained_frames))
                .copied(),
        );
    }

    fn clear(&mut self) {
        self.resampler.clear();
        self.resampled.clear();
        if let Ok(mut buffer) = self.buffer.lock() {
            buffer.samples.clear();
            buffer.buffering = true;
        }
    }
}

fn build_output(
    device: &Device,
    format: SampleFormat,
    config: StreamConfig,
    samples: Arc<Mutex<PlaybackBuffer>>,
) -> Result<Stream, AudioError> {
    let channels = usize::from(config.channels);
    let error = |error| tracing::error!(%error, "speaker stream failed");
    let stream = match format {
        SampleFormat::F32 => {
            let samples = Arc::clone(&samples);
            device
                .build_output_stream(
                    &config,
                    move |output: &mut [f32], _| {
                        fill_output(output, &samples, channels, |value| value);
                    },
                    error,
                    None,
                )
                .map_err(|error| backend("failed to build the speaker stream", error))?
        }
        SampleFormat::I16 => {
            let samples = Arc::clone(&samples);
            device
                .build_output_stream(
                    &config,
                    move |output: &mut [i16], _| {
                        fill_output(output, &samples, channels, f32_to_i16);
                    },
                    error,
                    None,
                )
                .map_err(|error| backend("failed to build the speaker stream", error))?
        }
        SampleFormat::U16 => {
            let samples = Arc::clone(&samples);
            device
                .build_output_stream(
                    &config,
                    move |output: &mut [u16], _| {
                        fill_output(output, &samples, channels, |value| {
                            ((value.clamp(-1.0, 1.0) + 1.0) * 32_767.5).round() as u16
                        });
                    },
                    error,
                    None,
                )
                .map_err(|error| backend("failed to build the speaker stream", error))?
        }
        unsupported => {
            return Err(AudioError::Backend {
                operation: "unsupported speaker sample format",
                message: unsupported.to_string(),
            });
        }
    };
    Ok(stream)
}

fn fill_output<T>(
    output: &mut [T],
    buffer: &Mutex<PlaybackBuffer>,
    channels: usize,
    convert: impl Fn(f32) -> T,
) {
    let Ok(mut buffer) = buffer.try_lock() else {
        for sample in output {
            *sample = convert(0.0);
        }
        return;
    };
    if buffer.buffering {
        if buffer.samples.len() < buffer.prebuffer_frames {
            for sample in output {
                *sample = convert(0.0);
            }
            return;
        }
        buffer.buffering = false;
    }
    for frame in output.chunks_mut(channels) {
        let Some(value) = buffer.samples.pop_front() else {
            buffer.buffering = true;
            for sample in frame {
                *sample = convert(0.0);
            }
            continue;
        };
        for sample in frame {
            *sample = convert(value);
        }
    }
}

fn duration_frames(sample_rate: u32, duration: Duration) -> usize {
    duration
        .as_nanos()
        .saturating_mul(u128::from(sample_rate))
        .checked_div(1_000_000_000)
        .and_then(|samples| usize::try_from(samples).ok())
        .unwrap_or(usize::MAX)
}

fn backend(operation: &'static str, error: impl Display) -> AudioError {
    AudioError::Backend {
        operation,
        message: error.to_string(),
    }
}

fn map_capture(error: CaptureError) -> AudioError {
    match error {
        CaptureError::NoInputDevice => AudioError::NoInputDevice,
        CaptureError::InvalidConfig(message) => AudioError::InvalidConfig(message),
        CaptureError::Backend { operation, message } => AudioError::Backend { operation, message },
        CaptureError::UnsupportedPlatform => AudioError::UnsupportedPlatform,
    }
}

fn f32_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex, time::Duration};

    use super::{PlaybackBuffer, duration_frames, fill_output};

    #[test]
    fn playback_waits_for_prebuffer_before_starting() {
        let buffer = Mutex::new(PlaybackBuffer {
            samples: VecDeque::from([0.25, 0.5]),
            prebuffer_frames: 3,
            buffering: true,
        });
        let mut output = [1.0; 2];

        fill_output(&mut output, &buffer, 1, |sample| sample);

        assert_eq!(output, [0.0, 0.0]);
        assert_eq!(buffer.lock().unwrap().samples.len(), 2);
    }

    #[test]
    fn playback_rebuffers_after_an_underrun() {
        let buffer = Mutex::new(PlaybackBuffer {
            samples: VecDeque::from([0.25, 0.5, 0.75]),
            prebuffer_frames: 3,
            buffering: true,
        });
        let mut output = [1.0; 4];

        fill_output(&mut output, &buffer, 1, |sample| sample);

        assert_eq!(output, [0.25, 0.5, 0.75, 0.0]);
        assert!(buffer.lock().unwrap().buffering);
    }

    #[test]
    fn playback_fans_mono_frames_out_to_device_channels() {
        let buffer = Mutex::new(PlaybackBuffer {
            samples: VecDeque::from([0.25, 0.5]),
            prebuffer_frames: 2,
            buffering: true,
        });
        let mut output = [1.0; 4];

        fill_output(&mut output, &buffer, 2, |sample| sample);

        assert_eq!(output, [0.25, 0.25, 0.5, 0.5]);
    }

    #[test]
    fn duration_policy_maps_to_device_frames() {
        assert_eq!(duration_frames(48_000, Duration::from_millis(120)), 5_760);
    }
}
