//! Reusable Hand publisher core, independent of account credentials and OS APIs.
//!
//! Platform adapters supply encoded video and speaker PCM through [`CaptureSource`].
//! The core owns media tasks, WebRTC peers and input ordering. Host authentication,
//! reconnection, capture permission and native input injection stay with the caller.
pub mod audio;
pub mod capture;
pub mod frames;
pub mod ice;
pub mod input;
pub mod video;

pub use capture::{Capture, CaptureSource, Task};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub mod preparation;
pub mod target;
