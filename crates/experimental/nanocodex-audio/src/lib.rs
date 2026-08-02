#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod capture;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[path = "capture_unsupported.rs"]
mod capture;
mod resample;

pub use capture::{CaptureConfig, CaptureGate, CaptureStream, Pcm16Chunk};
pub use resample::LinearResampler;

/// Failure to configure or operate the default microphone.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// The host has no default microphone.
    #[error("no default microphone is available")]
    NoInputDevice,
    /// Capture policy contains an invalid value.
    #[error("invalid microphone capture policy: {0}")]
    InvalidConfig(&'static str),
    /// The native audio backend rejected an operation.
    #[error("{operation}: {message}")]
    Backend {
        /// Audio operation reported by the backend.
        operation: &'static str,
        /// Backend diagnostic.
        message: String,
    },
    /// Default-device capture targets macOS and Windows.
    #[error("default microphone capture is available on macOS and Windows")]
    UnsupportedPlatform,
}
