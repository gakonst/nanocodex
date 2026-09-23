//! Reusable Hand publisher core, independent of managed-host and OS APIs.
//!
//! Platform adapters supply encoded video and speaker PCM through [`CaptureSource`].
//! Hosts supply checked credentials, capture permission and native input injection.
//! The core owns media, input lease policy, authenticated signaling, reconnection
//! and publisher lifecycle.
pub mod audio;
pub mod audio_duplex;
pub mod capture;
mod diagnostics;
pub mod frames;
pub mod ice;
pub mod input;
mod media_guard;
mod playout;
pub mod video;
mod video_packets;

pub use capture::{Capture, CaptureSource, Task};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub mod preparation;
pub mod target;

pub mod runtime;
mod tls;
