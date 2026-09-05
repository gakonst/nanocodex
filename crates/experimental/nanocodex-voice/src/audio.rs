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
use tokio::sync::mpsc;

use crate::{AudioConfig, AudioError};

const INPUT_FRAME_SAMPLES: usize = 480;
const MICROPHONE_QUEUE_FRAMES: usize = 8;
const DEVICE_CALLBACK_CAPACITY: usize = 4_096;
const PLAYBACK_INITIAL_HEADROOM: Duration = Duration::from_secs(1);

pub(crate) struct VoiceAudio {
    _input: Stream,
    _output: Stream,
    playback: Playback,
}

impl VoiceAudio {
    pub(crate) fn open(
        policy: AudioConfig,
    ) -> Result<(Self, mpsc::Receiver<RealtimeAudio>), AudioError> {
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
        let input_device = host
            .default_input_device()
            .ok_or(AudioError::NoInputDevice)?;
        let output_device = host
            .default_output_device()
            .ok_or(AudioError::NoOutputDevice)?;

        let input_supported = input_device
            .default_input_config()
            .map_err(|error| backend("failed to read the default microphone format", error))?;
        let output_supported = output_device
            .default_output_config()
            .map_err(|error| backend("failed to read the default speaker format", error))?;
        let input_config: StreamConfig = input_supported.clone().into();
        let output_config: StreamConfig = output_supported.clone().into();

        // Keep capture close to live speech. If transport stops consuming,
        // retaining seconds of stale microphone audio is worse than dropping
        // it and resuming from the newest callback.
        let (microphone_tx, microphone_rx) = mpsc::channel(MICROPHONE_QUEUE_FRAMES);
        let input = build_input(
            &input_device,
            input_supported.sample_format(),
            input_config,
            microphone_tx,
        )?;
        let playback = Playback::new(output_config.sample_rate.0, output_config.channels, policy)?;
        let output = build_output(
            &output_device,
            output_supported.sample_format(),
            output_config,
            Arc::clone(&playback.buffer),
        )?;

        input
            .play()
            .map_err(|error| backend("failed to start the microphone", error))?;
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
        let source = audio.as_bytes().as_chunks::<2>().0.iter().map(|sample| {
            let sample = i16::from_le_bytes(*sample);
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

struct Microphone {
    resampler: LinearResampler,
    channels: usize,
    interleaved_tail: Vec<f32>,
    mono: Vec<f32>,
    resampled: Vec<f32>,
    pending: VecDeque<f32>,
    sender: mpsc::Sender<RealtimeAudio>,
}

impl Microphone {
    fn new(sample_rate: u32, channels: u16, sender: mpsc::Sender<RealtimeAudio>) -> Self {
        let channels = usize::from(channels);
        Self {
            resampler: LinearResampler::new(sample_rate, REALTIME_SAMPLE_RATE),
            channels,
            interleaved_tail: Vec::with_capacity(DEVICE_CALLBACK_CAPACITY.saturating_mul(channels)),
            mono: Vec::with_capacity(DEVICE_CALLBACK_CAPACITY),
            resampled: Vec::with_capacity(DEVICE_CALLBACK_CAPACITY),
            pending: VecDeque::with_capacity(INPUT_FRAME_SAMPLES * 2),
            sender,
        }
    }

    fn push(&mut self, input: impl IntoIterator<Item = f32>) {
        self.interleaved_tail.extend(input);
        let complete = self.interleaved_tail.len() / self.channels * self.channels;
        self.mono.clear();
        self.mono.extend(
            self.interleaved_tail[..complete]
                .chunks_exact(self.channels)
                .map(|frame| frame.iter().copied().sum::<f32>() / self.channels as f32),
        );
        self.interleaved_tail.drain(..complete);
        self.resampler
            .push_into(self.mono.iter().copied(), &mut self.resampled);
        self.pending.extend(self.resampled.iter().copied());

        while self.pending.len() >= INPUT_FRAME_SAMPLES {
            let audio = RealtimeAudio::from_samples(
                self.pending.drain(..INPUT_FRAME_SAMPLES).map(f32_to_i16),
            );
            match self.sender.try_send(audio) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // Do not turn transport backpressure into delayed speech
                    // or an unbounded callback-owned queue.
                    self.pending.clear();
                    return;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    self.pending.clear();
                    return;
                }
            }
        }
    }
}

struct LinearResampler {
    step: f64,
    position: f64,
    source: Vec<f32>,
}

impl LinearResampler {
    fn new(source_rate: u32, destination_rate: u32) -> Self {
        Self {
            step: f64::from(source_rate) / f64::from(destination_rate),
            position: 0.0,
            source: Vec::new(),
        }
    }

    fn push_into(&mut self, input: impl IntoIterator<Item = f32>, output: &mut Vec<f32>) {
        self.source.extend(input);
        output.clear();
        while self.position + 1.0 < self.source.len() as f64 {
            let index = self.position.floor() as usize;
            let fraction = (self.position - index as f64) as f32;
            output.push(
                self.source[index] + (self.source[index + 1] - self.source[index]) * fraction,
            );
            self.position += self.step;
        }
        let consumed = self.position.floor() as usize;
        if consumed > 0 {
            self.source.drain(..consumed.min(self.source.len()));
            self.position -= consumed as f64;
        }
    }

    fn clear(&mut self) {
        self.position = 0.0;
        self.source.clear();
    }
}

fn build_input(
    device: &Device,
    format: SampleFormat,
    config: StreamConfig,
    sender: mpsc::Sender<RealtimeAudio>,
) -> Result<Stream, AudioError> {
    let sample_rate = config.sample_rate.0;
    let channels = config.channels;
    let error = |error| tracing::error!(%error, "microphone stream failed");
    let stream = match format {
        SampleFormat::F32 => {
            let mut microphone = Microphone::new(sample_rate, channels, sender);
            device
                .build_input_stream(
                    &config,
                    move |data: &[f32], _| microphone.push(data.iter().copied()),
                    error,
                    None,
                )
                .map_err(|error| backend("failed to build the microphone stream", error))?
        }
        SampleFormat::I16 => {
            let mut microphone = Microphone::new(sample_rate, channels, sender);
            device
                .build_input_stream(
                    &config,
                    move |data: &[i16], _| {
                        microphone.push(
                            data.iter()
                                .map(|sample| f32::from(*sample) / f32::from(i16::MAX)),
                        );
                    },
                    error,
                    None,
                )
                .map_err(|error| backend("failed to build the microphone stream", error))?
        }
        SampleFormat::U16 => {
            let mut microphone = Microphone::new(sample_rate, channels, sender);
            device
                .build_input_stream(
                    &config,
                    move |data: &[u16], _| {
                        microphone.push(
                            data.iter()
                                .map(|sample| f32::from(*sample) / 32_767.5 - 1.0),
                        );
                    },
                    error,
                    None,
                )
                .map_err(|error| backend("failed to build the microphone stream", error))?
        }
        unsupported => {
            return Err(AudioError::Backend {
                operation: "unsupported microphone sample format",
                message: unsupported.to_string(),
            });
        }
    };
    Ok(stream)
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

fn f32_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex, time::Duration};

    use super::{
        INPUT_FRAME_SAMPLES, LinearResampler, Microphone, PlaybackBuffer, duration_frames,
        fill_output,
    };

    #[test]
    fn resamples_across_chunk_boundaries_without_reallocating_the_destination() {
        let mut downsample = LinearResampler::new(48_000, 24_000);
        let mut output = Vec::with_capacity(4);
        downsample.push_into([0.0, 0.25], &mut output);
        assert_eq!(output, vec![0.0]);
        let capacity = output.capacity();
        downsample.push_into([0.5, 0.75, 1.0], &mut output);
        assert_eq!(output, vec![0.5]);
        assert_eq!(output.capacity(), capacity);

        let mut upsample = LinearResampler::new(24_000, 48_000);
        upsample.push_into([0.0], &mut output);
        assert!(output.is_empty());
        upsample.push_into([1.0], &mut output);
        assert_eq!(output, vec![0.0, 0.5]);
        upsample.push_into([2.0], &mut output);
        assert_eq!(output, vec![1.0, 1.5]);
    }

    #[test]
    fn resampling_is_invariant_to_callback_boundaries() {
        let source = (0..4_410)
            .map(|index| (index as f32 / 100.0).sin())
            .collect::<Vec<_>>();
        let mut contiguous = LinearResampler::new(44_100, 24_000);
        let mut expected = Vec::new();
        contiguous.push_into(source.iter().copied(), &mut expected);

        let mut chunked = LinearResampler::new(44_100, 24_000);
        let mut actual = Vec::new();
        let mut output = Vec::new();
        for chunk in source.chunks(127) {
            chunked.push_into(chunk.iter().copied(), &mut output);
            actual.extend_from_slice(&output);
        }

        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn saturated_microphone_drops_stale_capture() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let mut microphone = Microphone::new(24_000, 1, sender);

        microphone.push(std::iter::repeat_n(0.25, INPUT_FRAME_SAMPLES * 3));

        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
        assert!(microphone.pending.is_empty());
    }

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
