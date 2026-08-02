use tokio::sync::mpsc;

use crate::CaptureError;

/// Requested microphone conversion and chunking policy.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureConfig {
    /// Output sample rate after conversion.
    pub sample_rate_hz: u32,
    /// Fixed mono PCM16 samples in each output chunk.
    pub samples_per_chunk: usize,
}

/// One fixed-size mono PCM16 microphone chunk.
#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pcm16Chunk {
    /// Output sample rate.
    pub sample_rate_hz: u32,
    /// Native signed samples.
    pub samples: Box<[i16]>,
}

/// Synchronous capture gate shared with an owning lifecycle handle.
#[doc(hidden)]
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

/// Unsupported-platform default-device capture placeholder.
#[doc(hidden)]
pub struct CaptureStream;

impl CaptureStream {
    /// Returns the typed platform availability error.
    pub fn open(
        _config: CaptureConfig,
    ) -> Result<(Self, mpsc::Receiver<Pcm16Chunk>), CaptureError> {
        Err(CaptureError::UnsupportedPlatform)
    }

    /// Returns the typed platform availability error.
    pub fn open_with_gate(
        _config: CaptureConfig,
        _gate: CaptureGate,
    ) -> Result<(Self, mpsc::Receiver<Pcm16Chunk>), CaptureError> {
        Err(CaptureError::UnsupportedPlatform)
    }

    /// No-op on unsupported platforms.
    pub const fn stop(&self) {}
}
