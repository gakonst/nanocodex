#[cfg(not(target_family = "wasm"))]
#[path = "native.rs"]
mod platform;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
#[path = "web.rs"]
mod platform;

pub(crate) use platform::{AgentFactory, AgentSend};
pub(super) use platform::{ServiceFactory, spawn_driver};
