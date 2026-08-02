use std::{
    collections::VecDeque,
    fmt::Display,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use cpal::{
    Device, SampleFormat, Stream, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use tokio::sync::mpsc;

use crate::{CaptureError, LinearResampler};

const CAPTURE_QUEUE_CHUNKS: usize = 8;
const DEVICE_CALLBACK_CAPACITY: usize = 4_096;

/// Requested microphone conversion and chunking policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureConfig {
    /// Output sample rate after linear conversion.
    pub sample_rate_hz: u32,
    /// Fixed mono PCM16 samples in each output chunk.
    pub samples_per_chunk: usize,
}

/// One fixed-size mono PCM16 microphone chunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pcm16Chunk {
    /// Output sample rate.
    pub sample_rate_hz: u32,
    /// Native signed samples.
    pub samples: Box<[i16]>,
}

/// Synchronous capture gate shared with an owning lifecycle handle.
#[derive(Clone, Default)]
pub struct CaptureGate {
    active: Arc<AtomicBool>,
}

impl CaptureGate {
    /// Creates an enabled capture gate.
    #[must_use]
    pub fn enabled() -> Self {
        let gate = Self::default();
        gate.active.store(true, Ordering::Release);
        gate
    }

    /// Stops accepting microphone samples immediately.
    pub fn stop(&self) {
        self.active.store(false, Ordering::Release);
    }

    /// Returns whether callbacks may still publish samples.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
}

/// Owned default-device microphone stream.
pub struct CaptureStream {
    _stream: Stream,
    gate: CaptureGate,
}

impl CaptureStream {
    /// Opens and starts the default microphone with a new enabled gate.
    pub fn open(config: CaptureConfig) -> Result<(Self, mpsc::Receiver<Pcm16Chunk>), CaptureError> {
        Self::open_with_gate(config, CaptureGate::enabled())
    }

    /// Opens and starts the default microphone using an existing gate.
    pub fn open_with_gate(
        config: CaptureConfig,
        gate: CaptureGate,
    ) -> Result<(Self, mpsc::Receiver<Pcm16Chunk>), CaptureError> {
        if config.sample_rate_hz == 0 {
            return Err(CaptureError::InvalidConfig(
                "sample rate must be greater than zero",
            ));
        }
        if config.samples_per_chunk == 0 {
            return Err(CaptureError::InvalidConfig(
                "samples per chunk must be greater than zero",
            ));
        }
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or(CaptureError::NoInputDevice)?;
        let supported = device
            .default_input_config()
            .map_err(|error| backend("failed to read the default microphone format", error))?;
        let stream_config: StreamConfig = supported.clone().into();
        let (sender, receiver) = mpsc::channel(CAPTURE_QUEUE_CHUNKS);
        let stream = build_input(
            &device,
            supported.sample_format(),
            stream_config,
            config,
            sender,
            gate.clone(),
        )?;
        stream
            .play()
            .map_err(|error| backend("failed to start the microphone", error))?;
        Ok((
            Self {
                _stream: stream,
                gate,
            },
            receiver,
        ))
    }

    /// Stops accepting microphone samples synchronously.
    pub fn stop(&self) {
        self.gate.stop();
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Microphone {
    resampler: LinearResampler,
    output_rate: u32,
    samples_per_chunk: usize,
    channels: usize,
    interleaved_tail: Vec<f32>,
    mono: Vec<f32>,
    resampled: Vec<f32>,
    pending: VecDeque<f32>,
    sender: mpsc::Sender<Pcm16Chunk>,
    gate: CaptureGate,
}

impl Microphone {
    fn new(
        source_rate: u32,
        channels: u16,
        config: CaptureConfig,
        sender: mpsc::Sender<Pcm16Chunk>,
        gate: CaptureGate,
    ) -> Self {
        let channels = usize::from(channels);
        Self {
            resampler: LinearResampler::new(source_rate, config.sample_rate_hz),
            output_rate: config.sample_rate_hz,
            samples_per_chunk: config.samples_per_chunk,
            channels,
            interleaved_tail: Vec::with_capacity(DEVICE_CALLBACK_CAPACITY.saturating_mul(channels)),
            mono: Vec::with_capacity(DEVICE_CALLBACK_CAPACITY),
            resampled: Vec::with_capacity(DEVICE_CALLBACK_CAPACITY),
            pending: VecDeque::with_capacity(config.samples_per_chunk.saturating_mul(2)),
            sender,
            gate,
        }
    }

    fn push(&mut self, input: impl IntoIterator<Item = f32>) {
        if !self.gate.is_active() || self.channels == 0 {
            return;
        }
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

        while self.pending.len() >= self.samples_per_chunk {
            let samples = self
                .pending
                .drain(..self.samples_per_chunk)
                .map(f32_to_i16)
                .collect::<Vec<_>>()
                .into_boxed_slice();
            let chunk = Pcm16Chunk {
                sample_rate_hz: self.output_rate,
                samples,
            };
            match self.sender.try_send(chunk) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.pending.clear();
                    return;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    self.pending.clear();
                    self.gate.stop();
                    return;
                }
            }
        }
    }
}

fn build_input(
    device: &Device,
    format: SampleFormat,
    stream_config: StreamConfig,
    capture_config: CaptureConfig,
    sender: mpsc::Sender<Pcm16Chunk>,
    gate: CaptureGate,
) -> Result<Stream, CaptureError> {
    let sample_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels;
    let error = |error| tracing::error!(%error, "microphone stream failed");
    let stream = match format {
        SampleFormat::F32 => {
            let mut microphone =
                Microphone::new(sample_rate, channels, capture_config, sender, gate);
            device
                .build_input_stream(
                    &stream_config,
                    move |data: &[f32], _| microphone.push(data.iter().copied()),
                    error,
                    None,
                )
                .map_err(|error| backend("failed to build the microphone stream", error))?
        }
        SampleFormat::I16 => {
            let mut microphone =
                Microphone::new(sample_rate, channels, capture_config, sender, gate);
            device
                .build_input_stream(
                    &stream_config,
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
            let mut microphone =
                Microphone::new(sample_rate, channels, capture_config, sender, gate);
            device
                .build_input_stream(
                    &stream_config,
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
            return Err(CaptureError::Backend {
                operation: "unsupported microphone sample format",
                message: unsupported.to_string(),
            });
        }
    };
    Ok(stream)
}

fn backend(operation: &'static str, error: impl Display) -> CaptureError {
    CaptureError::Backend {
        operation,
        message: error.to_string(),
    }
}

fn f32_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
}

#[cfg(test)]
mod tests {
    use super::{CaptureConfig, CaptureGate, Microphone};

    #[test]
    fn saturated_capture_drops_stale_callback_audio() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let config = CaptureConfig {
            sample_rate_hz: 24_000,
            samples_per_chunk: 480,
        };
        let mut microphone = Microphone::new(24_000, 1, config, sender, CaptureGate::enabled());
        microphone.push(std::iter::repeat_n(0.25, 480 * 3));
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
        assert!(microphone.pending.is_empty());
    }

    #[test]
    fn stopped_gate_rejects_callback_audio_synchronously() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let gate = CaptureGate::enabled();
        let mut microphone = Microphone::new(
            24_000,
            1,
            CaptureConfig {
                sample_rate_hz: 24_000,
                samples_per_chunk: 480,
            },
            sender,
            gate.clone(),
        );
        gate.stop();
        microphone.push(std::iter::repeat_n(0.25, 480));
        assert!(receiver.try_recv().is_err());
    }
}
