//! Durable subagent task-tree journal attached to a root agent handle.
//!
//! A root built with durability carries a [`SubagentStore`] on its
//! [`crate::AgentHandle`]. Any subagent registry whose tools are installed into
//! that root journals the root's task tree there, restores it after a restart
//! and resumes interrupted children, without per-host wiring.

use std::{future::Future, pin::Pin};

/// Boxed store operation.
#[cfg(not(target_family = "wasm"))]
pub type SubagentStoreFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// Boxed store operation.
#[cfg(target_family = "wasm")]
pub type SubagentStoreFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Host persistence for one opaque subagent journal value per root session.
///
/// Values are subagent-runtime-owned JSON. Stores return them verbatim; a save
/// must atomically replace the previous value for the same root.
/// On WebAssembly hosts the store is still shared through `Send + Sync`
/// capabilities, so JavaScript-backed stores wrap their single-threaded handles.
pub trait SubagentStore: Send + Sync {
    /// Loads the latest journal for a root session.
    fn load<'a>(
        &'a self,
        root_session_id: &'a str,
    ) -> SubagentStoreFuture<'a, std::io::Result<Option<String>>>;
    /// Atomically replaces the journal for a root session.
    fn save<'a>(
        &'a self,
        root_session_id: &'a str,
        payload: String,
    ) -> SubagentStoreFuture<'a, std::io::Result<()>>;
}
