//! Platform APIs compiled into the Rust Nanocodex Hand. No companion executable.
#![deny(unsafe_code)]
/// Native platform capture or input failure.
pub type Error = Box<dyn std::error::Error + Send + Sync>;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod capture;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{main_display_index, request};

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::{ensure_interactive_session, request, video_command};

#[cfg(target_os = "windows")]
mod windows_audio;
#[cfg(target_os = "windows")]
pub use windows_audio::capture_audio;
