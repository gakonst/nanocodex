//! Platform APIs compiled into the Rust Nanocodex Hand. No companion executable.
#![deny(unsafe_code)]
/// Native platform capture or input failure.
pub type Error = Box<dyn std::error::Error + Send + Sync>;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::request;
