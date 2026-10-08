//! Durable subagent task trees.
//!
//! A root built with durability exposes a [`SubagentStore`] on its
//! [`nanocodex_agent::AgentHandle`]. When that root installs this crate's tools,
//! the registry restores the root's journaled tree, resumes children whose turn
//! was interrupted, and then journals one versioned, self-contained value per
//! root after every lifecycle change: topology, identities, roles and tasks,
//! output contracts, statuses, accepted outputs, the input of a running turn and
//! each child's latest committed conversation boundary for every harness.
//!
//! Hosts need no subagent-specific wiring: attaching durability to the root
//! builder is sufficient. [`crate::Registry::set_store`] and
//! [`crate::Registry::restore`] remain available for hosts that persist trees
//! without root durability.

use super::{
    model::{AgentDescriptor, AgentId, AgentStatus},
    runtime::{ChildSession, OutputContract},
};
use nanocodex_agent::{ChildRuntimeSnapshot, ChildSnapshot, HarnessModel, Thinking};
pub use nanocodex_agent::{SubagentStore, SubagentStoreFuture};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

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
    /// Input of the running turn, replayed when it is resumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) turn_input: Option<String>,
    /// Latest committed Responses boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) checkpoint: Option<ChildRuntimeSnapshot>,
    /// Latest committed provider-native boundary, such as a Claude child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) native_checkpoint: Option<PersistedNativeCheckpoint>,
}

/// Credential-free native checkpoint owned and decoded by its backend family.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PersistedNativeCheckpoint {
    model: HarnessModel,
    session_id: String,
    thinking: Thinking,
    payload: String,
    has_conversation: bool,
}

/// Outcome of restoring one root's subagent tree.
#[derive(Clone, Debug, Default, Serialize)]
pub struct RestoreReport {
    /// Agents restored into the task tree.
    pub restored: usize,
    /// Agents whose turn was running at the last journal write.
    pub interrupted: Vec<AgentId>,
    /// Agents that cannot run again because no checkpoint exists.
    pub unrecoverable: Vec<AgentId>,
}

pub(super) const RESUME_MESSAGE: &str = "The sub-agent runtime restarted while your previous \
turn was running. Your conversation was restored from its latest committed checkpoint, so \
recent tool calls may not appear in your history. Inspect the current workspace state before \
acting, do not repeat side effects that already happened, and continue your delegated task.";

/// Continuation sent to a child whose turn was interrupted by a restart.
pub(super) fn resume_message(turn_input: Option<&str>) -> String {
    match turn_input {
        // A resumed turn that is interrupted again already carries the header.
        Some(input) if input.starts_with(RESUME_MESSAGE) => input.to_owned(),
        Some(input) => format!(
            "{RESUME_MESSAGE}\n\nThe interrupted turn was started by this input:\n\n{input}"
        ),
        None => RESUME_MESSAGE.to_owned(),
    }
}

fn persisted_checkpoint(
    snapshot: &ChildSnapshot,
) -> (
    Option<ChildRuntimeSnapshot>,
    Option<PersistedNativeCheckpoint>,
) {
    match snapshot {
        ChildSnapshot::Codex(snapshot) => (Some(snapshot.clone()), None),
        ChildSnapshot::Native {
            model,
            session_id,
            thinking,
            payload,
            has_conversation,
        } => (
            None,
            Some(PersistedNativeCheckpoint {
                model: *model,
                session_id: session_id.clone(),
                thinking: *thinking,
                payload: payload.clone(),
                has_conversation: *has_conversation,
            }),
        ),
    }
}

impl PersistedAgent {
    pub(super) fn snapshot(&self) -> Option<ChildSnapshot> {
        self.checkpoint
            .clone()
            .map(ChildSnapshot::Codex)
            .or_else(|| {
                self.native_checkpoint
                    .clone()
                    .map(|native| ChildSnapshot::Native {
                        model: native.model,
                        session_id: native.session_id,
                        thinking: native.thinking,
                        payload: native.payload,
                        has_conversation: native.has_conversation,
                    })
            })
    }
}

pub(super) fn persist_agent(
    session: &ChildSession,
    checkpoint: Option<&ChildSnapshot>,
) -> PersistedAgent {
    let (checkpoint, native_checkpoint) = checkpoint
        .or(session.stored_runtime.as_ref())
        .map_or((None, None), persisted_checkpoint);
    let turn_in_flight = session.active || matches!(session.status, AgentStatus::Running);
    PersistedAgent {
        descriptor: session.descriptor.clone(),
        status: session.status.clone(),
        output_schema: session.output_schema.clone(),
        host_context: session.host_context.as_deref().map(str::to_owned),
        last_output: session.last_output.clone(),
        next_instruction_revision: session.next_instruction_revision,
        turn_in_flight,
        turn_input: turn_in_flight.then(|| session.turn_input.clone()).flatten(),
        checkpoint,
        native_checkpoint,
    }
}

pub(super) struct RestoredAgent {
    pub(super) session: ChildSession,
    pub(super) checkpoint: Option<ChildSnapshot>,
    /// The interrupted turn's input, present only when it must be resumed.
    pub(super) resume: Option<Option<String>>,
    pub(super) unrecoverable: bool,
}

pub(super) fn restored_session(agent: PersistedAgent) -> std::io::Result<RestoredAgent> {
    let contract = OutputContract::compile(&agent.output_schema)?;
    let checkpoint = agent.snapshot();
    let recoverable = checkpoint.is_some();
    let terminal = matches!(agent.status, AgentStatus::Closing | AgentStatus::Closed);
    let in_flight = !terminal
        && (agent.turn_in_flight
            || matches!(agent.status, AgentStatus::Running | AgentStatus::Pending));
    let status = if terminal {
        AgentStatus::Closed
    } else if in_flight && !recoverable {
        AgentStatus::Failed {
            error: "subagent could not be restored after a runtime restart: no checkpoint was \
                    available"
                .to_owned(),
        }
    } else if in_flight {
        AgentStatus::Interrupted
    } else {
        agent.status
    };
    // A child without a committed conversation replays its assignment when it
    // is rehydrated, which already carries the first turn's input.
    let turn_input = agent.turn_input.filter(|_| {
        checkpoint
            .as_ref()
            .is_some_and(ChildSnapshot::has_conversation)
    });
    let session = ChildSession::restored(
        agent.descriptor,
        agent.host_context.map(Arc::from),
        status,
        contract,
        agent.output_schema,
        checkpoint.clone(),
        agent.next_instruction_revision,
        agent.last_output,
    );
    Ok(RestoredAgent {
        session,
        checkpoint,
        resume: (in_flight && recoverable).then_some(turn_input),
        unrecoverable: !recoverable && !terminal,
    })
}
