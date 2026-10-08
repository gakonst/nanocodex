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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) binding_task: Option<String>,
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
    /// Latest committed boundary of a native (for example Claude) backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) native_checkpoint: Option<PersistedNative>,
}

/// Portable form of [`ChildSnapshot::Native`], decoded only by its family.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PersistedNative {
    model: String,
    session_id: String,
    thinking: nanocodex_agent::Thinking,
    payload: String,
    has_conversation: bool,
}

impl PersistedNative {
    fn from_snapshot(snapshot: &ChildSnapshot) -> Option<Self> {
        match snapshot {
            ChildSnapshot::Native {
                model,
                session_id,
                thinking,
                payload,
                has_conversation,
            } => Some(Self {
                model: model.as_str().to_owned(),
                session_id: session_id.clone(),
                thinking: *thinking,
                payload: payload.clone(),
                has_conversation: *has_conversation,
            }),
            ChildSnapshot::Codex(_) => None,
        }
    }

    fn into_snapshot(self) -> std::io::Result<ChildSnapshot> {
        let model = self
            .model
            .parse::<nanocodex_agent::HarnessModel>()
            .map_err(|error| std::io::Error::other(format!("invalid journaled model: {error}")))?;
        Ok(ChildSnapshot::Native {
            model,
            session_id: self.session_id,
            thinking: self.thinking,
            payload: self.payload,
            has_conversation: self.has_conversation,
        })
    }
}

/// Adapts a durable root handle's journal to the registry's store contract.
pub(super) struct JournalStore(pub(super) Arc<dyn nanocodex_agent::backend::ChildJournalStore>);

impl SubagentStore for JournalStore {
    fn load<'a>(
        &'a self,
        _root_session_id: &'a str,
    ) -> SubagentStoreFuture<'a, std::io::Result<Option<String>>> {
        self.0.load()
    }

    fn save<'a>(
        &'a self,
        _root_session_id: &'a str,
        payload: String,
    ) -> SubagentStoreFuture<'a, std::io::Result<()>> {
        self.0.save(payload)
    }
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
turn was running. Your conversation was restored from its latest committed checkpoint: every \
step in your history completed, but a tool call that was still running at the restart is not \
shown and may or may not have taken effect. Inspect the current workspace state where that \
matters, do not repeat side effects that already happened, and continue your delegated task \
from where your history ends. Submit a result only once the whole delegated task is complete.";

pub(super) fn persist_agent(
    session: &ChildSession,
    checkpoint: Option<&ChildSnapshot>,
) -> PersistedAgent {
    let latest = checkpoint.or(session.stored_runtime.as_ref());
    let native_checkpoint = latest.and_then(PersistedNative::from_snapshot);
    let checkpoint = latest.and_then(|snapshot| match snapshot {
        ChildSnapshot::Codex(snapshot) => Some(snapshot.clone()),
        ChildSnapshot::Native { .. } => None,
    });
    PersistedAgent {
        descriptor: session.descriptor.clone(),
        binding_task: Some(session.binding_task.clone()),
        status: session.status.clone(),
        output_schema: session.output_schema.clone(),
        host_context: session.host_context.as_deref().map(str::to_owned),
        last_output: session.last_output.clone(),
        next_instruction_revision: session.next_instruction_revision,
        turn_in_flight: session.active || matches!(session.status, AgentStatus::Running),
        checkpoint,
        native_checkpoint,
    }
}

impl PersistedAgent {
    /// The journaled checkpoint for any backend family.
    pub(super) fn snapshot(&self) -> std::io::Result<Option<ChildSnapshot>> {
        if let Some(snapshot) = &self.checkpoint {
            return Ok(Some(ChildSnapshot::Codex(snapshot.clone())));
        }
        self.native_checkpoint
            .clone()
            .map(PersistedNative::into_snapshot)
            .transpose()
    }
}

pub(super) fn restored_session(
    agent: PersistedAgent,
) -> std::io::Result<(ChildSession, bool, bool)> {
    let contract = OutputContract::compile(&agent.output_schema)?;
    let snapshot = agent.snapshot()?;
    let recoverable = snapshot.is_some();
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
    let binding_task = agent.binding_task;
    let mut session = ChildSession::restored(
        agent.descriptor,
        agent.host_context.map(Arc::from),
        status,
        contract,
        agent.output_schema,
        snapshot,
        agent.next_instruction_revision,
        agent.last_output,
    );
    if let Some(task) = binding_task {
        session.binding_task = task;
    }
    Ok((session, in_flight && recoverable, !recoverable && !terminal))
}
