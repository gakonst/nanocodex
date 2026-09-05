#[cfg(target_family = "wasm")]
#[path = "platform/host.rs"]
mod implementation;
#[cfg(not(target_family = "wasm"))]
#[path = "platform/native.rs"]
mod implementation;

pub(super) use implementation::FactoryPlatform;
