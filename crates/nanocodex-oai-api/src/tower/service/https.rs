#[cfg(target_family = "wasm")]
#[path = "https/host.rs"]
mod implementation;
#[cfg(not(target_family = "wasm"))]
#[path = "https/native.rs"]
mod implementation;

pub(super) use implementation::run;
