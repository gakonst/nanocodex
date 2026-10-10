//! Compatibility facade for the shared publisher transport.
#[cfg(all(test, target_os = "macos"))]
pub(crate) use nanocodex_remote::video::Video;
pub(crate) use nanocodex_remote::video::{Capture, Task, VideoSource};
