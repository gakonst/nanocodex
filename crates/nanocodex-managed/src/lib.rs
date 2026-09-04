//! Native account-managed lifecycle backend for Nanocodex.
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(target_family = "wasm")]
compile_error!("nanocodex-managed is a native lifecycle backend");

mod auth;
mod builder;
mod client;
mod driver;
mod error;
mod sse;
mod types;
mod websocket;

#[cfg(feature = "tools")]
mod vm_host;

#[cfg(feature = "tools")]
mod attachment;

pub use auth::ManagedApiKey;
pub use builder::{Managed, ManagedBuilder, ManagedRequest, ManagedResponse, ManagedService};
pub use client::{ManagedClient, ManagedClientBuilder};
pub use driver::ManagedAgent;
pub use error::ManagedError;
pub use nanocodex_agent::{Model, ReasoningMode, Thinking};
pub use sse::{
    EventCursor, ManagedEventFuture, ManagedEventSource, ManagedEventStream, ManagedEvents,
};
pub use types::*;

#[cfg(feature = "tools")]
#[cfg_attr(docsrs, doc(cfg(feature = "tools")))]
pub use vm_host::{
    VmHostAllocationState, VmHostCommand, VmHostConnection, VmHostFence, VmHostProvision,
    VmHostRelease, VmHostScope, VmShape, connect_system_vm_host, validate_vm_factory_name,
};
