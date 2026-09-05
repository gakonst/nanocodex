#[cfg(target_family = "wasm")]
#[path = "delay/host.rs"]
mod implementation;
#[cfg(not(target_family = "wasm"))]
#[path = "delay/native.rs"]
mod implementation;

pub(crate) use implementation::{RetryDelay, RetryFuture};
