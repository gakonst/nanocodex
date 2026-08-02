use tokio::sync::mpsc;

use crate::CaptureError;

/// Requested microphone conversion and chunking policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureConfig {
    /// Output sample rate after conversion.
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
pub struct CaptureGate;

impl CaptureGate {
    /// Creates an enabled capture gate.
    #[must_use]
    pub const fn enabled() -> Self {
        Self
    }

    /// Stops accepting microphone samples immediately.
    pub const fn stop(&self) {}

    /// Returns whether callbacks may still publish samples.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        false
    }
}

/// Platform availability boundary for default-device capture.
pub struct CaptureStream;

impl CaptureStream {
    /// Reports the platforms that provide default-device capture.
    pub fn open(
        _config: CaptureConfig,
    ) -> Result<(Self, mpsc::Receiver<Pcm16Chunk>), CaptureError> {
        Err(CaptureError::UnsupportedPlatform)
    }

    /// Reports the platforms that provide default-device capture.
    pub fn open_with_gate(
        _config: CaptureConfig,
        _gate: CaptureGate,
    ) -> Result<(Self, mpsc::Receiver<Pcm16Chunk>), CaptureError> {
        Err(CaptureError::UnsupportedPlatform)
    }

    /// Completes the platform availability boundary.
    pub const fn stop(&self) {}
}
