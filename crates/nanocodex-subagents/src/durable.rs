//! Durable subagent task trees.
//!
//! A host that persists its root agent can also persist that root's subagent
//! tree by installing a [`SubagentStore`]. The registry then journals one
//! versioned, self-contained value per root session after every lifecycle
//! change: topology, identities, roles and tasks, output contracts, statuses,
//! accepted outputs, and each child's latest committed conversation boundary.
//!
//! After a process restart or Durable Object eviction, the host calls
//! [`Registry::restore`] for the recovered root session and then
//! [`Registry::resume_interrupted`] once the root handle can rehydrate children.
//! Restored children are non-resident until used; children whose turn was in
//! flight are continued from their latest committed checkpoint.

use super::{
    model::{AgentDescriptor, AgentId, AgentStatus},
    runtime::{ChildSession, OutputContract},
};
use nanocodex_agent::{ChildRuntimeSnapshot, ChildSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

/// Boxed store operation.
#[cfg(not(target_family = "wasm"))]
pub type SubagentStoreFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// Boxed store operation.
#[cfg(target_family = "wasm")]
pub type SubagentStoreFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Host persistence for one opaque subagent journal value per root session.
///
/// Values are Rust-owned JSON. Hosts store and return them verbatim; a save
/// must atomically replace the previous value for the same root.
/// On WebAssembly hosts the registry is still shared through `Send + Sync`
/// tool objects, so JavaScript-backed stores wrap their single-threaded handles.
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

/// In-memory [`SubagentStore`], useful for tests and single-process hosts that
/// rebuild their runtime without losing the process.
#[derive(Clone, Default)]
pub struct MemorySubagentStore {
    values: Arc<Mutex<HashMap<String, String>>>,
}

impl MemorySubagentStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl SubagentStore for MemorySubagentStore {
    fn load<'a>(
        &'a self,
        root_session_id: &'a str,
    ) -> SubagentStoreFuture<'a, std::io::Result<Option<String>>> {
        Box::pin(async move {
            Ok(self
                .values
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(root_session_id)
                .cloned())
        })
    }

    fn save<'a>(
        &'a self,
        root_session_id: &'a str,
        payload: String,
    ) -> SubagentStoreFuture<'a, std::io::Result<()>> {
        Box::pin(async move {
            self.values
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(root_session_id.to_owned(), payload);
            Ok(())
        })
    }
}

pub(super) const JOURNAL_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub(super) struct PersistedScope {
    pub(super) version: u32,
    pub(super) agents: Vec<PersistedAgent>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct PersistedAgent {
    pub(super) descriptor: AgentDescriptor,
    pub(super) status: AgentStatus,
    pub(super) output_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) host_context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) last_output: Option<Value>,
    #[serde(default)]
    pub(super) next_instruction_revision: u64,
    /// Whether a turn was running when this value was written.
    #[serde(default)]
    pub(super) turn_in_flight: bool,
    /// Latest committed boundary; absent for native backends without a
    /// portable checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) checkpoint: Option<ChildRuntimeSnapshot>,
}

/// Outcome of restoring one root's subagent tree.
#[derive(Clone, Debug, Default, Serialize)]
pub struct RestoreReport {
    /// Agents restored into the task tree.
    pub restored: usize,
    /// Agents whose turn was running at the last journal write.
    pub interrupted: Vec<AgentId>,
    /// Agents that cannot run again because no portable checkpoint exists.
    pub unrecoverable: Vec<AgentId>,
}

pub(super) const RESUME_MESSAGE: &str = "The sub-agent runtime restarted while your previous \
turn was running. Your conversation was restored from its latest committed checkpoint, so \
recent tool calls may not appear in your history. Inspect the current workspace state before \
acting, do not repeat side effects that already happened, and continue your delegated task.";

pub(super) fn persist_agent(
    session: &ChildSession,
    checkpoint: Option<&ChildSnapshot>,
) -> PersistedAgent {
    let checkpoint = checkpoint
        .or(session.stored_runtime.as_ref())
        .and_then(|snapshot| match snapshot {
            ChildSnapshot::Codex(snapshot) => Some(snapshot.clone()),
            ChildSnapshot::Native { .. } => None,
        });
    PersistedAgent {
        descriptor: session.descriptor.clone(),
        status: session.status.clone(),
        output_schema: session.output_schema.clone(),
        host_context: session.host_context.as_deref().map(str::to_owned),
        last_output: session.last_output.clone(),
        next_instruction_revision: session.next_instruction_revision,
        turn_in_flight: session.active || matches!(session.status, AgentStatus::Running),
        checkpoint,
    }
}

pub(super) fn restored_session(
    agent: PersistedAgent,
) -> std::io::Result<(ChildSession, bool, bool)> {
    let contract = OutputContract::compile(&agent.output_schema)?;
    let recoverable = agent.checkpoint.is_some();
    let terminal = matches!(agent.status, AgentStatus::Closing | AgentStatus::Closed);
    let in_flight = !terminal
        && (agent.turn_in_flight
            || matches!(agent.status, AgentStatus::Running | AgentStatus::Pending));
    let status = if terminal {
        AgentStatus::Closed
    } else if in_flight && !recoverable {
        AgentStatus::Failed {
            error: "subagent could not be restored after a runtime restart: no portable \
                    checkpoint was available"
                .to_owned(),
        }
    } else if in_flight {
        AgentStatus::Interrupted
    } else {
        agent.status
    };
    let session = ChildSession::restored(
        agent.descriptor,
        agent.host_context.map(Arc::from),
        status,
        contract,
        agent.output_schema,
        agent.checkpoint.map(ChildSnapshot::Codex),
        agent.next_instruction_revision,
        agent.last_output,
    );
    Ok((session, in_flight && recoverable, !recoverable && !terminal))
}
