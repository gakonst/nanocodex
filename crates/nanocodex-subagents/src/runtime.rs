// Derived from clabby/tact@1d9ccaefd1d8613dab020812af04a91cd9b4c52c (Apache-2.0).
// Modified for Nanocodex's reusable native/WASM extension runtime.

//! Async child-agent sessions, turns, and lifecycle orchestration.

use super::diagnostics::{CompletionError, CompletionErrorCode};
use super::{
    capacity::{Capacity, TurnCapacity},
    harness::{self, HarnessHandle},
    message::MessageThreads,
    model::{
        AgentDescriptor, AgentId, AgentMessage, AgentMessageUpdate, AgentStatus, AgentThread,
        AgentUpdate, MessageDeliveryState, MessageDisposition, MessageId, MessagePriority,
        MessagePurpose, MessageSender, ScopedAgentUpdate, SubagentRuntimeId, ThreadId,
    },
    platform::{self, Task, timeout_at},
    task_tree::TaskTree,
};
use futures_util::future::join_all;
use jsonschema::Validator;
use nanocodex_agent::{
    AgentEvents, AgentHandle, ChildRuntimeSnapshot, Nanocodex, NanocodexError,
    Result as AgentResult, TurnResult,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Arc, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};
use web_time::Instant;

pub(super) struct ChildSession {
    pub(super) descriptor: AgentDescriptor,
    pub(super) host_context: Option<Arc<str>>,
    pub(super) event_task: Option<Task<()>>,
    pub(super) harness: Option<HarnessHandle>,
    pub(super) harness_task: Option<Task<()>>,
    pub(super) status: AgentStatus,
    pub(super) active: bool,
    pub(super) output_validator: Validator,
    pub(super) output_schema: Value,
    pub(super) stored_runtime: Option<ChildRuntimeSnapshot>,
    pub(super) next_turn_token: u64,
    pub(super) active_turn_token: Option<u64>,
    pub(super) steering: bool,
    pub(super) submitted_output: Option<Value>,
    pub(super) last_output: Option<Value>,
    pub(super) last_used: u64,
    pub(super) evicted: bool,
}

pub(super) struct OutputContract {
    validator: Validator,
    schema: String,
}

impl OutputContract {
    pub(super) fn compile(schema: &Value) -> std::io::Result<Self> {
        let validator = jsonschema::validator_for(schema)
            .map_err(|error| std::io::Error::other(format!("invalid output_schema: {error}")))?;
        let schema = serde_json::to_string_pretty(schema)
            .map_err(|error| std::io::Error::other(format!("could not render schema: {error}")))?;
        Ok(Self { validator, schema })
    }
}

pub(super) fn completion_instructions(schema: &str, turn_token: u64) -> String {
    format!(
        "Your contractual result is not prose. Before finishing, call `submit_result` exactly \
         once with `{{ turn_token: {turn_token}, output: ... }}` and a JSON value matching the \
         output schema below. Use the callable `submit_result` entry in your actual tool catalog. Do not assume \
         a Code Mode `tools.submit_result` binding exists unless that catalog exposes it. \
         Pass objects and arrays directly as JSON values; do not serialize them into JSON strings. \
         If validation rejects the value, correct it and retry. A turn \
         that ends without an accepted result fails.\n\nOutput schema:\n{schema}"
    )
}

/// Bounded, versioned durable child state. No live host capabilities are serialized.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentCheckpoint {
    pub version: u32,
    pub root_session_id: String,
    pub next_agent_id: u64,
    pub children: Vec<ChildCheckpoint>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChildCheckpoint {
    pub descriptor: AgentDescriptor,
    pub runtime: Option<ChildRuntimeSnapshot>,
    pub output_schema: Value,
    pub next_turn_token: u64,
    pub status: AgentStatus,
    pub last_output: Option<Value>,
    pub host_context: Option<String>,
}

const MAX_CHECKPOINT_CHILDREN: usize = 1024;
pub const MAX_SUBAGENT_CHECKPOINT_BYTES: usize = 16 * 1024 * 1024;

impl SubagentCheckpoint {
    /// Validates the complete document before any driver or host resource is opened.
    pub fn validate(&self, root_session_id: &str) -> std::io::Result<()> {
        if self.version != 1 || self.root_session_id != root_session_id {
            return Err(std::io::Error::other(
                "unsupported subagent checkpoint version or root identity",
            ));
        }
        if self.children.len() > MAX_CHECKPOINT_CHILDREN
            || serde_json::to_vec(self)
                .map_err(std::io::Error::other)?
                .len()
                > MAX_SUBAGENT_CHECKPOINT_BYTES
        {
            return Err(std::io::Error::other("subagent checkpoint exceeds limits"));
        }
        let mut topology = RegistryState::default();
        topology.restore(
            root_session_id,
            self.children
                .iter()
                .map(|child| child.descriptor.clone())
                .collect(),
        )?;
        topology
            .scope_mut(root_session_id)
            .topology
            .restore_next_agent_id(self.next_agent_id)?;
        for child in &self.children {
            if child.next_turn_token == u64::MAX
                || child
                    .host_context
                    .as_ref()
                    .is_some_and(|context| context.is_empty() || context.len() > 4096)
            {
                return Err(std::io::Error::other("invalid child token or host context"));
            }
            match &child.runtime {
                Some(runtime) => {
                    if runtime.session_id != child.descriptor.session_id {
                        return Err(std::io::Error::other(
                            "child checkpoint session identity mismatch",
                        ));
                    }
                    runtime.validate().map_err(std::io::Error::other)?;
                }
                // Legacy descriptor-only children are archival, never reusable.
                None if matches!(child.status, AgentStatus::Closed | AgentStatus::Interrupted) => {}
                None => {
                    return Err(std::io::Error::other(
                        "reusable child is missing its runtime checkpoint",
                    ));
                }
            }
            if child.runtime.is_some()
                && !matches!(child.status, AgentStatus::Closed | AgentStatus::Closing)
                && let Some(parent) = child.descriptor.parent
                && self.children.iter().any(|candidate| {
                    candidate.descriptor.id == parent && candidate.runtime.is_none()
                })
            {
                return Err(std::io::Error::other(
                    "reusable child has a parent without a runtime checkpoint",
                ));
            }
            let contract = OutputContract::compile(&child.output_schema)?;
            if let Some(output) = &child.last_output
                && !contract.validator.is_valid(output)
            {
                return Err(std::io::Error::other(
                    "checkpoint last result violates child output schema",
                ));
            }
            if let AgentStatus::Completed { output } = &child.status
                && !contract.validator.is_valid(output)
            {
                return Err(std::io::Error::other(
                    "checkpoint completed result violates child output schema",
                ));
            }
            if let Some(parent) = child.descriptor.parent
                && self.children.iter().any(|candidate| {
                    candidate.descriptor.id == parent
                        && matches!(candidate.status, AgentStatus::Closed | AgentStatus::Closing)
                })
                && !matches!(child.status, AgentStatus::Closed | AgentStatus::Closing)
            {
                return Err(std::io::Error::other("reusable child has a closed parent"));
            }
        }
        Ok(())
    }
}

pub struct Registry {
    session_handles: std::sync::RwLock<HashMap<String, AgentHandle>>,
    spawn_router: std::sync::RwLock<Option<Arc<dyn crate::SpawnRouter>>>,
    id: SubagentRuntimeId,
    state: tokio::sync::Mutex<RegistryState>,
    pub(super) updates: mpsc::UnboundedSender<ScopedAgentUpdate>,
    revision: watch::Sender<u64>,
    capacity: Capacity,
    max_resident: AtomicUsize,
    residency_lock: tokio::sync::Mutex<()>,
    message_lock: tokio::sync::Mutex<()>,
}

#[derive(Default)]
pub(super) struct RegistryState {
    root_by_session: HashMap<String, String>,
    scopes: HashMap<String, AgentScope>,
    next_access: u64,
}

#[derive(Default)]
struct AgentScope {
    topology: TaskTree,
    sessions: HashMap<AgentId, ChildSession>,
    messages: MessageThreads,
    closing: bool,
}

pub(super) struct AgentReservation {
    pub(super) root_session_id: String,
    pub(super) id: AgentId,
    pub(super) parent: Option<AgentId>,
}

pub(super) struct CloseRequest {
    pub(super) root_session_id: String,
    pub(super) ids: Vec<AgentId>,
    pub(super) harnesses: Vec<HarnessHandle>,
    pub(super) status_updates: Vec<(AgentId, AgentStatus)>,
}

pub(super) struct ClosedSessions {
    pub(super) summaries: Vec<AgentSummary>,
    pub(super) harness_tasks: Vec<Task<()>>,
    pub(super) event_tasks: Vec<Task<()>>,
}

pub(super) struct BatchStartup {
    registry: Arc<Registry>,
    cleanup: Option<(String, Vec<AgentId>)>,
}

impl BatchStartup {
    pub(super) fn track(&mut self, root_session_id: &str, id: AgentId) {
        let (_, ids) = self
            .cleanup
            .get_or_insert_with(|| (root_session_id.to_owned(), Vec::new()));
        ids.push(id);
    }

    pub(super) async fn rollback(mut self) {
        let cleanup = self.cleanup.take();
        let registry = Arc::clone(&self.registry);
        drop(self);
        if let Some((root_session_id, ids)) = cleanup {
            registry.close_batch(&root_session_id, ids).await;
        }
    }

    pub(super) fn commit(mut self) {
        self.cleanup = None;
    }
}

impl Drop for BatchStartup {
    fn drop(&mut self) {
        let Some((root_session_id, ids)) = self.cleanup.take() else {
            return;
        };
        let registry = Arc::clone(&self.registry);
        drop(platform::spawn(async move {
            registry.close_batch(&root_session_id, ids).await;
        }));
    }
}

#[derive(Clone, Serialize)]
pub struct AgentSummary {
    pub agent_id: AgentId,
    pub role: String,
    pub task: String,
    pub parent_agent_id: Option<AgentId>,
    pub status: AgentStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_output: Option<Value>,
}

#[derive(Serialize)]
pub struct AgentDirectoryEntry {
    pub agent_id: AgentId,
    pub role: String,
    pub task: String,
    pub parent_agent_id: Option<AgentId>,
    pub status: AgentStatus,
    pub can_message: bool,
    pub can_manage: bool,
}

#[derive(Debug, Serialize)]
pub struct MessageReceipt {
    pub message_id: MessageId,
    pub thread_id: ThreadId,
    pub from: MessageSender,
    pub to_agent_id: AgentId,
    pub disposition: MessageDisposition,
}

struct PreparedMessage {
    root_session_id: String,
    message: AgentMessage,
    harness: HarnessHandle,
}

pub(super) struct DelegationChange {
    target: AgentId,
    previous_task: String,
}

pub(super) struct TurnSteer {
    id: AgentId,
    previous_token: u64,
    token: u64,
}

struct ResidentEviction {
    id: AgentId,
    harness: HarnessHandle,
}

impl TurnSteer {
    pub(super) const fn token(&self) -> u64 {
        self.token
    }
}

impl RegistryState {
    const fn next_access(&mut self) -> u64 {
        self.next_access = self.next_access.wrapping_add(1);
        self.next_access
    }

    fn submit_result(
        &mut self,
        session_id: &str,
        turn_token: u64,
        output: Value,
    ) -> std::io::Result<bool> {
        let root_session_id = self.root_session_id(session_id).to_owned();
        let scope = self.scopes.get_mut(&root_session_id).ok_or_else(|| {
            CompletionError::new(
                CompletionErrorCode::NotChild,
                false,
                "submit_result is only available to subagents",
                "Return root answers as assistant text.",
            )
        })?;
        let id = scope
            .topology
            .agent_for_session(session_id)
            .ok_or_else(|| {
                CompletionError::new(
                    CompletionErrorCode::NotChild,
                    false,
                    "submit_result is only available to subagents",
                    "Return root answers as assistant text.",
                )
            })?;
        let session = scope
            .sessions
            .get_mut(&id)
            .ok_or_else(|| std::io::Error::other("subagent session disappeared"))?;
        if !session.active {
            return Err(CompletionError::new(
                CompletionErrorCode::InactiveTurn,
                false,
                "submit_result is only available during an active subagent turn",
                "Start a new assigned turn before submitting.",
            )
            .into());
        }
        if session.steering {
            return Err(CompletionError::new(
                CompletionErrorCode::SteeringInProgress,
                true,
                "the subagent turn is being steered",
                "Wait for the steering message, incorporate it, and submit with its turn_token.",
            )
            .into());
        }
        if session.active_turn_token != Some(turn_token) {
            return Err(CompletionError::new(
                CompletionErrorCode::StaleTurnToken,
                true,
                "submit_result used a stale or unknown turn_token",
                "Read and incorporate the latest steering instructions before resubmitting; do not only replace the token.",
            ).with_token(session.active_turn_token).into());
        }
        if session.submitted_output.is_some() {
            return Err(CompletionError::new(
                CompletionErrorCode::AlreadyAccepted,
                false,
                "submit_result already accepted one result for this turn",
                "Finish this turn; do not submit again.",
            )
            .into());
        }
        let (output, decoded_json_text) =
            validate_submitted_output(&session.output_validator, output)
                .map_err(|error| error.with_token(session.active_turn_token))?;
        session.submitted_output = Some(output);
        Ok(decoded_json_text)
    }

    fn begin_turn_steer(&mut self, root_session_id: &str, id: AgentId) -> Option<TurnSteer> {
        let session = self
            .scopes
            .get_mut(root_session_id)?
            .sessions
            .get_mut(&id)?;
        if !session.active || session.steering || session.submitted_output.is_some() {
            return None;
        }
        let previous_token = session.active_turn_token?;
        let token = session.next_turn_token.checked_add(1)?;
        session.next_turn_token = token;
        session.active_turn_token = Some(token);
        session.steering = true;
        Some(TurnSteer {
            id,
            previous_token,
            token,
        })
    }

    fn finish_turn_steer(&mut self, root_session_id: &str, steer: TurnSteer, committed: bool) {
        let Some(session) = self
            .scopes
            .get_mut(root_session_id)
            .and_then(|scope| scope.sessions.get_mut(&steer.id))
        else {
            return;
        };
        if session.active_turn_token != Some(steer.token) {
            return;
        }
        if !committed {
            session.active_turn_token = Some(steer.previous_token);
        }
        session.steering = false;
    }

    fn reserve_for(&mut self, session_id: &str) -> std::io::Result<AgentReservation> {
        let root_session_id = self.root_session_id(session_id).to_owned();
        if self
            .scopes
            .get(&root_session_id)
            .is_some_and(|scope| scope.closing)
        {
            return Err(std::io::Error::other(
                "subagent scope is closing and cannot spawn children",
            ));
        }
        let parent = self
            .scopes
            .get(&root_session_id)
            .and_then(|scope| scope.topology.agent_for_session(session_id));
        if let Some(parent) = parent {
            let parent_session = self
                .scopes
                .get(&root_session_id)
                .and_then(|scope| scope.sessions.get(&parent))
                .ok_or_else(|| std::io::Error::other("subagent parent disappeared"))?;
            if matches!(
                parent_session.status,
                AgentStatus::Closing | AgentStatus::Closed
            ) {
                return Err(std::io::Error::other(format!(
                    "agent {parent} is closing and cannot spawn children"
                )));
            }
        }
        self.reserve(&root_session_id, parent)
    }

    fn reserve(
        &mut self,
        session_id: &str,
        parent: Option<AgentId>,
    ) -> std::io::Result<AgentReservation> {
        let root_session_id = self.root_session_id(session_id).to_owned();
        if self
            .scopes
            .get(&root_session_id)
            .is_some_and(|scope| scope.closing)
        {
            return Err(std::io::Error::other(
                "subagent scope is closing and cannot reserve children",
            ));
        }
        let id = self.scope_mut(&root_session_id).topology.reserve(parent)?;
        Ok(AgentReservation {
            root_session_id,
            id,
            parent,
        })
    }

    fn insert(
        &mut self,
        root_session_id: String,
        id: AgentId,
        session_id: String,
        session: ChildSession,
    ) -> std::io::Result<()> {
        if let Some(parent) = session.descriptor.parent {
            let parent_session = self
                .scopes
                .get(&root_session_id)
                .and_then(|scope| scope.sessions.get(&parent))
                .ok_or_else(|| std::io::Error::other(format!("unknown parent agent {parent}")))?;
            if matches!(
                parent_session.status,
                AgentStatus::Closing | AgentStatus::Closed
            ) {
                return Err(std::io::Error::other(format!(
                    "agent {parent} stopped while spawning child {id}"
                )));
            }
        }
        self.scope_mut(&root_session_id).topology.insert(
            id,
            session_id.clone(),
            session.descriptor.parent,
        )?;
        self.root_by_session
            .insert(session_id, root_session_id.clone());
        let last_used = self.next_access();
        let mut session = session;
        session.last_used = last_used;
        self.scope_mut(&root_session_id)
            .sessions
            .insert(id, session);
        Ok(())
    }

    fn restore(
        &mut self,
        root_session_id: &str,
        descriptors: Vec<AgentDescriptor>,
    ) -> std::io::Result<Vec<AgentDescriptor>> {
        if root_session_id.trim().is_empty() {
            return Err(std::io::Error::other(
                "restored subagent root session ID must not be empty",
            ));
        }
        if descriptors.is_empty() {
            return Ok(Vec::new());
        }
        if self.scopes.contains_key(root_session_id) {
            return Err(std::io::Error::other(format!(
                "subagent scope already exists for {root_session_id}"
            )));
        }

        let mut ids = std::collections::HashSet::new();
        let mut session_ids = std::collections::HashSet::new();
        for descriptor in &descriptors {
            if descriptor.session_id.trim().is_empty() {
                return Err(std::io::Error::other(
                    "restored subagent session ID must not be empty",
                ));
            }
            if descriptor.session_id == root_session_id {
                return Err(std::io::Error::other(
                    "restored subagent session ID must differ from its root session ID",
                ));
            }
            if !ids.insert(descriptor.id) {
                return Err(std::io::Error::other(format!(
                    "duplicate restored agent_id {}",
                    descriptor.id
                )));
            }
            if !session_ids.insert(descriptor.session_id.clone())
                || self.root_by_session.contains_key(&descriptor.session_id)
            {
                return Err(std::io::Error::other(format!(
                    "duplicate restored subagent session ID {}",
                    descriptor.session_id
                )));
            }
        }

        let mut restored = Self::default();
        let mut pending = descriptors;
        let mut ordered = Vec::new();
        while !pending.is_empty() {
            let before = pending.len();
            let mut deferred = Vec::new();
            for descriptor in pending {
                let parent_ready = descriptor.parent.is_none_or(|parent| {
                    restored
                        .scopes
                        .get(root_session_id)
                        .is_some_and(|scope| scope.sessions.contains_key(&parent))
                });
                if !parent_ready {
                    deferred.push(descriptor);
                    continue;
                }
                restored.insert(
                    root_session_id.to_owned(),
                    descriptor.id,
                    descriptor.session_id.clone(),
                    restored_tombstone(descriptor.clone(), None),
                )?;
                ordered.push(descriptor);
            }
            if deferred.len() == before {
                return Err(std::io::Error::other(
                    "restored subagent topology has an unknown parent or cycle",
                ));
            }
            pending = deferred;
        }

        let mut scope = restored
            .scopes
            .remove(root_session_id)
            .ok_or_else(|| std::io::Error::other("restored subagent scope disappeared"))?;
        for session in scope.sessions.values_mut() {
            session.last_used = self.next_access();
        }
        for (session_id, retained_root) in restored.root_by_session {
            self.root_by_session.insert(session_id, retained_root);
        }
        self.scopes.insert(root_session_id.to_owned(), scope);
        Ok(ordered)
    }

    fn validate_insert(
        &self,
        root_session_id: &str,
        descriptor: &AgentDescriptor,
    ) -> std::io::Result<()> {
        if self
            .scopes
            .get(root_session_id)
            .is_some_and(|scope| scope.closing)
        {
            return Err(std::io::Error::other(
                "subagent scope stopped while spawning children",
            ));
        }
        if let Some(parent) = descriptor.parent {
            let parent_session = self
                .scopes
                .get(root_session_id)
                .and_then(|scope| scope.sessions.get(&parent))
                .ok_or_else(|| std::io::Error::other(format!("unknown parent agent {parent}")))?;
            if matches!(
                parent_session.status,
                AgentStatus::Closing | AgentStatus::Closed
            ) {
                return Err(std::io::Error::other(format!(
                    "agent {parent} stopped while spawning child {}",
                    descriptor.id
                )));
            }
        }
        Ok(())
    }

    fn harness_in_scope(
        &self,
        root_session_id: &str,
        id: AgentId,
    ) -> std::io::Result<HarnessHandle> {
        self.scopes
            .get(root_session_id)
            .and_then(|scope| scope.sessions.get(&id))
            .and_then(|session| session.harness.clone())
            .ok_or_else(|| std::io::Error::other(format!("agent {id} is closed")))
    }

    fn directory(
        &self,
        session_id: &str,
        include_completed: bool,
        include_self: bool,
    ) -> Vec<AgentDirectoryEntry> {
        let root_session_id = self.root_session_id(session_id);
        let Some(scope) = self.scopes.get(root_session_id) else {
            return Vec::new();
        };
        let caller = scope.topology.agent_for_session(session_id);
        let mut ids = scope.topology.ids();
        ids.sort_unstable();
        ids.into_iter()
            .filter(|id| include_self || caller != Some(*id))
            .filter_map(|id| {
                let session = scope.sessions.get(&id)?;
                if !include_completed
                    && !matches!(session.status, AgentStatus::Pending | AgentStatus::Running)
                {
                    return None;
                }
                let can_message = caller != Some(id)
                    && (session.harness.is_some() || session.stored_runtime.is_some())
                    && !matches!(
                        session.status,
                        AgentStatus::Pending | AgentStatus::Closing | AgentStatus::Closed
                    );
                let can_manage = caller != Some(id)
                    && !matches!(session.status, AgentStatus::Closing | AgentStatus::Closed)
                    && scope.topology.authorize(session_id, id).is_ok();
                Some(AgentDirectoryEntry {
                    agent_id: id,
                    role: bounded_summary(&session.descriptor.role),
                    task: bounded_summary(&session.descriptor.task),
                    parent_agent_id: session.descriptor.parent,
                    status: session.status.clone(),
                    can_message,
                    can_manage,
                })
            })
            .collect()
    }

    fn prepare_message(
        &mut self,
        session_id: &str,
        to: AgentId,
        priority: MessagePriority,
        purpose: MessagePurpose,
        in_reply_to: Option<MessageId>,
        body: String,
    ) -> std::io::Result<PreparedMessage> {
        let root_session_id = self.root_session_id(session_id).to_owned();
        let scope = self
            .scopes
            .get_mut(&root_session_id)
            .ok_or_else(|| std::io::Error::other(format!("unknown agent_id {to}")))?;
        let from = scope
            .topology
            .agent_for_session(session_id)
            .map_or(MessageSender::Root, |agent_id| MessageSender::Agent {
                agent_id,
            });
        if from.agent_id() == Some(to) {
            return Err(std::io::Error::other("agents cannot message themselves"));
        }
        if purpose == MessagePurpose::Delegate {
            scope.topology.authorize(session_id, to)?;
        }
        let target = scope
            .sessions
            .get_mut(&to)
            .ok_or_else(|| std::io::Error::other(format!("unknown agent_id {to}")))?;
        if matches!(target.status, AgentStatus::Pending) {
            return Err(std::io::Error::other(format!(
                "agent {to} has not started and cannot receive messages yet"
            )));
        }
        if matches!(target.status, AgentStatus::Closing | AgentStatus::Closed) {
            return Err(std::io::Error::other(format!(
                "agent {to} is {:?} and cannot receive messages",
                target.status
            )));
        }
        let harness = target.harness.clone().ok_or_else(|| {
            std::io::Error::other(format!(
                "agent {to} has no saved runtime history and cannot resume from its retained descriptor. Call list_agents with include_completed=true and select a recipient with can_message=true, or spawn a replacement agent. Do not retry this recipient while can_message=false."
            ))
        })?;
        self.next_access = self.next_access.wrapping_add(1);
        target.last_used = self.next_access;
        let message = scope
            .messages
            .prepare(from, to, priority, purpose, in_reply_to, body)?;
        Ok(PreparedMessage {
            root_session_id,
            message,
            harness,
        })
    }

    fn commit_message(
        &mut self,
        root_session_id: &str,
        message: AgentMessage,
    ) -> std::io::Result<AgentThread> {
        let scope = self
            .scopes
            .get_mut(root_session_id)
            .ok_or_else(|| std::io::Error::other("subagent scope disappeared"))?;
        Ok(scope.messages.commit(message))
    }

    fn rollback_message(&mut self, root_session_id: &str, id: MessageId) {
        if let Some(scope) = self.scopes.get_mut(root_session_id) {
            scope.messages.rollback(id);
        }
    }

    fn begin_delegation(
        &mut self,
        root_session_id: &str,
        id: MessageId,
    ) -> Option<(DelegationChange, AgentDescriptor)> {
        let scope = self.scopes.get_mut(root_session_id)?;
        let message = scope.messages.message(id)?;
        if message.purpose != MessagePurpose::Delegate {
            return None;
        }
        let target = scope.sessions.get_mut(&message.to)?;
        let previous_task = std::mem::replace(&mut target.descriptor.task, message.body);
        Some((
            DelegationChange {
                target: message.to,
                previous_task,
            },
            target.descriptor.clone(),
        ))
    }

    fn rollback_delegation(
        &mut self,
        root_session_id: &str,
        change: DelegationChange,
    ) -> Option<AgentDescriptor> {
        let target = self
            .scopes
            .get_mut(root_session_id)?
            .sessions
            .get_mut(&change.target)?;
        target.descriptor.task = change.previous_task;
        Some(target.descriptor.clone())
    }

    fn thread_for_message(&self, root_session_id: &str, id: MessageId) -> Option<AgentThread> {
        self.scopes
            .get(root_session_id)
            .and_then(|scope| scope.messages.thread_for_message(id))
    }

    fn mark_message_admitted(
        &mut self,
        root_session_id: &str,
        id: MessageId,
        disposition: MessageDisposition,
    ) {
        if let Some(scope) = self.scopes.get_mut(root_session_id) {
            scope.messages.mark_admitted(id, disposition);
        }
    }

    fn mark_message_terminal(&mut self, root_session_id: &str, id: MessageId) {
        if let Some(scope) = self.scopes.get_mut(root_session_id) {
            scope.messages.mark_terminal(id);
        }
    }

    fn summaries(&self, session_id: &str, ids: &[AgentId]) -> std::io::Result<Vec<AgentSummary>> {
        let root_session_id = self.root_session_id(session_id);
        for &id in ids {
            self.authorize(session_id, id)?;
        }
        self.summaries_in_scope(root_session_id, ids)
    }

    fn summaries_in_scope(
        &self,
        root_session_id: &str,
        ids: &[AgentId],
    ) -> std::io::Result<Vec<AgentSummary>> {
        let scope = self
            .scopes
            .get(root_session_id)
            .ok_or_else(|| std::io::Error::other("subagent scope disappeared"))?;
        ids.iter()
            .map(|id| {
                scope
                    .sessions
                    .get(id)
                    .map(ChildSession::summary)
                    .ok_or_else(|| std::io::Error::other(format!("unknown agent_id {id}")))
            })
            .collect()
    }

    fn request_interrupt(
        &mut self,
        session_id: &str,
        id: AgentId,
    ) -> std::io::Result<(String, Vec<AgentId>, Vec<HarnessHandle>)> {
        let root_session_id = self.authorize(session_id, id)?;
        let ids = self.subtree_shutdown_order(&root_session_id, id)?;
        let harnesses = self.harnesses(&root_session_id, &ids, false)?;
        Ok((root_session_id, ids, harnesses))
    }

    fn request_close(&mut self, session_id: &str, id: AgentId) -> std::io::Result<CloseRequest> {
        let root_session_id = self.authorize(session_id, id)?;
        let ids = self.subtree_shutdown_order(&root_session_id, id)?;
        let harnesses = self.harnesses(&root_session_id, &ids, true)?;
        let status_updates = ids
            .iter()
            .copied()
            .map(|id| (id, AgentStatus::Closing))
            .collect();
        Ok(CloseRequest {
            root_session_id,
            ids,
            harnesses,
            status_updates,
        })
    }

    fn request_close_all(&mut self, session_id: &str) -> std::io::Result<CloseRequest> {
        let root_session_id = self.root_session_id(session_id).to_owned();
        let ids = {
            let scope = self.scope_mut(&root_session_id);
            scope.closing = true;
            scope.topology.all_postorder()
        };
        let harnesses = self.harnesses(&root_session_id, &ids, true)?;
        let status_updates = ids
            .iter()
            .copied()
            .map(|id| (id, AgentStatus::Closing))
            .collect();
        Ok(CloseRequest {
            root_session_id,
            ids,
            harnesses,
            status_updates,
        })
    }

    fn request_interrupt_all(
        &mut self,
        session_id: &str,
    ) -> (String, Vec<AgentId>, Vec<HarnessHandle>) {
        let root_session_id = self.root_session_id(session_id).to_owned();
        let ids = self
            .scopes
            .get(&root_session_id)
            .map(|scope| scope.topology.ids())
            .unwrap_or_default();
        let harnesses = self
            .harnesses(&root_session_id, &ids, false)
            .unwrap_or_default();
        (root_session_id, ids, harnesses)
    }

    fn harnesses(
        &mut self,
        root_session_id: &str,
        ids: &[AgentId],
        closing: bool,
    ) -> std::io::Result<Vec<HarnessHandle>> {
        let scope = self
            .scopes
            .get_mut(root_session_id)
            .ok_or_else(|| std::io::Error::other("subagent scope disappeared"))?;
        let mut harnesses = Vec::new();
        for id in ids {
            let session = scope
                .sessions
                .get_mut(id)
                .ok_or_else(|| std::io::Error::other(format!("unknown agent_id {id}")))?;
            if closing {
                session.status = AgentStatus::Closing;
            }
            harnesses.extend(session.harness.iter().cloned());
        }
        Ok(harnesses)
    }

    fn finish_close(
        &mut self,
        root_session_id: &str,
        ids: &[AgentId],
    ) -> std::io::Result<ClosedSessions> {
        let scope = self
            .scopes
            .get_mut(root_session_id)
            .ok_or_else(|| std::io::Error::other("subagent scope disappeared"))?;
        let mut harness_tasks = Vec::new();
        let mut event_tasks = Vec::new();
        for id in ids {
            let session = scope
                .sessions
                .get_mut(id)
                .ok_or_else(|| std::io::Error::other(format!("unknown agent_id {id}")))?;
            if session.active {
                return Err(std::io::Error::other(format!(
                    "agent {id} is still running"
                )));
            }
            session.harness = None;
            session.evicted = false;
            harness_tasks.extend(session.harness_task.take());
            event_tasks.extend(session.event_task.take());
            session.status = AgentStatus::Closed;
        }
        let summaries = ids
            .iter()
            .filter_map(|id| scope.sessions.get(id).map(ChildSession::summary))
            .collect();
        Ok(ClosedSessions {
            summaries,
            harness_tasks,
            event_tasks,
        })
    }

    fn all_inactive(&self, root_session_id: &str, ids: &[AgentId]) -> std::io::Result<bool> {
        let scope = self
            .scopes
            .get(root_session_id)
            .ok_or_else(|| std::io::Error::other("subagent scope disappeared"))?;
        Ok(ids.iter().all(|id| {
            scope
                .sessions
                .get(id)
                .is_some_and(|session| !session.active)
        }))
    }

    fn take_resident_eviction(
        &mut self,
        root_session_id: &str,
        limit: usize,
    ) -> Option<ResidentEviction> {
        let scope = self.scopes.get_mut(root_session_id)?;
        let resident = scope
            .sessions
            .values()
            .filter(|session| session.harness.is_some())
            .count();
        if resident <= limit {
            return None;
        }

        let candidate = scope
            .sessions
            .iter()
            .filter(|(_, session)| {
                !session.active && session.status.can_start_turn() && session.harness.is_some()
            })
            .filter(|(id, _)| !scope.messages.has_pending_for(**id))
            .filter(|(id, _)| {
                !scope.sessions.iter().any(|(other_id, other)| {
                    other.harness.is_some() && scope.topology.is_descendant(*other_id, **id)
                })
            })
            .min_by_key(|(id, session)| (session.last_used, **id))
            .map(|(id, _)| *id)?;
        let session = scope.sessions.get_mut(&candidate)?;
        let harness = session.harness.take()?;
        session.evicted = true;
        Some(ResidentEviction {
            id: candidate,
            harness,
        })
    }

    fn subtree_shutdown_order(
        &self,
        root_session_id: &str,
        id: AgentId,
    ) -> std::io::Result<Vec<AgentId>> {
        self.scopes
            .get(root_session_id)
            .ok_or_else(|| std::io::Error::other("subagent scope disappeared"))?
            .topology
            .subtree_postorder(id)
    }

    fn authorize(&self, session_id: &str, id: AgentId) -> std::io::Result<String> {
        let root_session_id = self.root_session_id(session_id);
        self.scopes
            .get(root_session_id)
            .ok_or_else(|| std::io::Error::other(format!("unknown agent_id {id}")))?
            .topology
            .authorize(session_id, id)?;
        Ok(root_session_id.to_owned())
    }

    fn root_session_id<'a>(&'a self, session_id: &'a str) -> &'a str {
        self.root_by_session
            .get(session_id)
            .map_or(session_id, String::as_str)
    }

    fn scope_mut(&mut self, root_session_id: &str) -> &mut AgentScope {
        self.scopes.entry(root_session_id.to_owned()).or_default()
    }
}

const AGENT_STOP_TIMEOUT: Duration = Duration::from_secs(30);

impl Registry {
    pub(super) fn new(
        updates: mpsc::UnboundedSender<ScopedAgentUpdate>,
        max_concurrency: usize,
    ) -> Self {
        let (revision, _) = watch::channel(0);
        Self {
            id: SubagentRuntimeId::next(),
            spawn_router: std::sync::RwLock::new(None),
            state: tokio::sync::Mutex::new(RegistryState::default()),
            updates,
            revision,
            capacity: Capacity::new(max_concurrency),
            session_handles: std::sync::RwLock::new(HashMap::new()),
            max_resident: AtomicUsize::new(crate::DEFAULT_MAX_RESIDENT_SUBAGENTS),
            residency_lock: tokio::sync::Mutex::new(()),
            message_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub(super) fn reserve_turn(&self) -> std::io::Result<TurnCapacity> {
        self.capacity.reserve()
    }

    pub(super) fn batch_startup(self: &Arc<Self>) -> BatchStartup {
        BatchStartup {
            registry: Arc::clone(self),
            cleanup: None,
        }
    }

    pub(super) fn reserve_turns(&self, count: usize) -> std::io::Result<Vec<TurnCapacity>> {
        self.capacity.reserve_many(count)
    }

    /// Installs a host routing policy before accepting child spawns.
    pub fn set_spawn_router(&self, router: Arc<dyn crate::SpawnRouter>) {
        *self
            .spawn_router
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(router);
    }

    pub(super) fn spawn_router(&self) -> Option<Arc<dyn crate::SpawnRouter>> {
        self.spawn_router
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set_max_concurrency(&self, limit: usize) {
        self.capacity.set_limit(limit);
    }

    pub fn set_max_resident(&self, limit: usize) {
        self.max_resident.store(limit.max(1), Ordering::Relaxed);
    }

    pub async fn is_root_session(&self, session_id: &str) -> bool {
        !self
            .state
            .lock()
            .await
            .root_by_session
            .contains_key(session_id)
    }

    /// Restores persisted logical children as interrupted, nonresident tombstones.
    ///
    /// This preserves handles returned by an earlier runtime without claiming
    /// that in-memory child execution survived reconstruction.
    pub async fn restore(
        &self,
        root_session_id: &str,
        descriptors: Vec<AgentDescriptor>,
    ) -> std::io::Result<()> {
        self.restore_with_host_contexts(root_session_id, descriptors, HashMap::new())
            .await
    }

    /// Restores logical children with embedding-private invocation context.
    #[doc(hidden)]
    pub async fn restore_with_host_contexts(
        &self,
        root_session_id: &str,
        descriptors: Vec<AgentDescriptor>,
        mut host_contexts: HashMap<String, Option<Arc<str>>>,
    ) -> std::io::Result<()> {
        if !host_contexts.is_empty()
            && (host_contexts.len() != descriptors.len()
                || descriptors
                    .iter()
                    .any(|descriptor| !host_contexts.contains_key(&descriptor.session_id)))
        {
            return Err(std::io::Error::other(
                "restored subagent host contexts must exactly match restored sessions",
            ));
        }
        let mut state = self.state.lock().await;
        let restored = state.restore(root_session_id, descriptors)?;
        if restored.is_empty() {
            return Ok(());
        }
        let scope = state
            .scopes
            .get_mut(root_session_id)
            .expect("a non-empty successful restore creates its scope");
        for descriptor in &restored {
            scope
                .sessions
                .get_mut(&descriptor.id)
                .expect("a restored descriptor has a retained session")
                .host_context = host_contexts.remove(&descriptor.session_id).flatten();
        }
        drop(state);
        for descriptor in restored {
            let id = descriptor.id;
            self.send(root_session_id, AgentUpdate::Added(descriptor));
            self.send(
                root_session_id,
                AgentUpdate::Status {
                    id,
                    status: AgentStatus::Interrupted,
                },
            );
        }
        self.changed();
        Ok(())
    }

    /// Captures the latest safe driver boundaries. Active turns restore as interrupted.
    pub async fn checkpoint(&self, root_session_id: &str) -> std::io::Result<SubagentCheckpoint> {
        self.capture_checkpoint(root_session_id, true).await
    }

    /// Captures committed driver history without cancelling ongoing child work.
    pub async fn live_checkpoint(
        &self,
        root_session_id: &str,
    ) -> std::io::Result<SubagentCheckpoint> {
        self.capture_checkpoint(root_session_id, false).await
    }

    async fn capture_checkpoint(
        &self,
        root_session_id: &str,
        interrupt: bool,
    ) -> std::io::Result<SubagentCheckpoint> {
        let _residency_guard = self.residency_lock.lock().await;
        let _message_guard = self.message_lock.lock().await;
        if self.state.lock().await.root_session_id(root_session_id) != root_session_id {
            return Err(std::io::Error::other(
                "only a root can checkpoint its subagent scope",
            ));
        }
        // Joining cancellation commits the safe history boundary before we read
        // status/results. This also rejects mailbox work that cannot survive unload.
        while interrupt {
            let (root, ids, harnesses) = self
                .state
                .lock()
                .await
                .request_interrupt_all(root_session_id);
            if ids.is_empty() {
                break;
            }
            self.interrupt_harnesses(&root, &ids, harnesses, Instant::now() + AGENT_STOP_TIMEOUT)
                .await?;
            let state = self.state.lock().await;
            if state
                .scopes
                .get(root_session_id)
                .is_none_or(|scope| scope.sessions.values().all(|session| !session.active))
            {
                break;
            }
        }
        let runtimes = {
            let state = self.state.lock().await;
            if state.root_session_id(root_session_id) != root_session_id {
                return Err(std::io::Error::other(
                    "only a root can checkpoint its subagent scope",
                ));
            }
            state
                .scopes
                .get(root_session_id)
                .map(|scope| {
                    scope
                        .sessions
                        .iter()
                        .filter(|(_, session)| {
                            !matches!(session.status, AgentStatus::Closed | AgentStatus::Closing)
                        })
                        .map(|(id, session)| {
                            (*id, session.harness.clone(), session.stored_runtime.clone())
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let mut children = Vec::with_capacity(runtimes.len());
        for (id, harness, stored) in runtimes {
            let (runtime, state) = loop {
                let before = {
                    let state = self.state.lock().await;
                    let session = &state.scopes[root_session_id].sessions[&id];
                    (session.status.clone(), session.next_turn_token)
                };
                let runtime = match &harness {
                    Some(harness) => Some(harness.snapshot().await?),
                    None => stored.clone(),
                };
                let state = self.state.lock().await;
                let session = &state.scopes[root_session_id].sessions[&id];
                if before == (session.status.clone(), session.next_turn_token) {
                    break (runtime, state);
                }
            };
            let session = &state.scopes[root_session_id].sessions[&id];
            if matches!(session.status, AgentStatus::Closed | AgentStatus::Closing) {
                continue;
            }
            children.push(ChildCheckpoint {
                descriptor: session.descriptor.clone(),
                runtime,
                output_schema: session.output_schema.clone(),
                next_turn_token: session.next_turn_token,
                status: match &session.status {
                    AgentStatus::Pending | AgentStatus::Running => AgentStatus::Interrupted,
                    AgentStatus::Closing => AgentStatus::Closed,
                    status => status.clone(),
                },
                last_output: session.last_output.clone(),
                host_context: session.host_context.as_deref().map(str::to_owned),
            });
        }
        children.sort_by_key(|child| child.descriptor.id);
        let next_agent_id = self
            .state
            .lock()
            .await
            .scopes
            .get(root_session_id)
            .map_or(1, |scope| scope.topology.next_agent_id());
        let checkpoint = SubagentCheckpoint {
            version: 1,
            root_session_id: root_session_id.to_owned(),
            next_agent_id,
            children,
        };
        checkpoint.validate(root_session_id)?;
        Ok(checkpoint)
    }

    /// Rebuilds real drivers in parent-first order using this host's parent capabilities.
    pub async fn restore_checkpoint(
        self: &Arc<Self>,
        root: &Nanocodex,
        checkpoint: SubagentCheckpoint,
    ) -> std::io::Result<()> {
        let root_session_id = root.session_id();
        checkpoint.validate(root_session_id)?;
        let _residency_guard = self.residency_lock.lock().await;
        let _message_guard = self.message_lock.lock().await;
        if self.state.lock().await.scopes.contains_key(root_session_id) {
            return Err(std::io::Error::other("subagent scope already exists"));
        }
        let ordered = self.state.lock().await.restore(
            root_session_id,
            checkpoint
                .children
                .iter()
                .map(|child| child.descriptor.clone())
                .collect(),
        )?;
        self.state
            .lock()
            .await
            .scope_mut(root_session_id)
            .topology
            .restore_next_agent_id(checkpoint.next_agent_id)?;
        let mut children = checkpoint
            .children
            .into_iter()
            .map(|child| (child.descriptor.id, child))
            .collect::<HashMap<_, _>>();
        let mut parents: HashMap<AgentId, Nanocodex> = HashMap::new();
        let mut gates = Vec::new();
        let result: std::io::Result<()> = async {
            for descriptor in &ordered {
                let child = children
                    .remove(&descriptor.id)
                    .expect("validated child checkpoint");
                let contract = OutputContract::compile(&child.output_schema)?;
                let closed = matches!(child.status, AgentStatus::Closed | AgentStatus::Closing);
                let host_context = child.host_context.map(Arc::<str>::from);
                let resources = if let Some(runtime) = child.runtime.clone().filter(|_| !closed) {
                    let parent = match descriptor.parent {
                        Some(id) => parents.get(&id).ok_or_else(|| {
                            std::io::Error::other("restored parent runtime is unavailable")
                        })?,
                        None => root,
                    };
                    let needs_assignment = runtime.conversation.is_none();
                    let (agent, events) = parent
                        .restore_child(runtime, host_context.clone())
                        .await
                        .map_err(std::io::Error::other)?;
                    parents.insert(descriptor.id, agent.clone());
                    let (start, ready) = oneshot::channel();
                    let event_task = forward_events(
                        root_session_id.to_owned(),
                        descriptor.id,
                        events,
                        ready,
                        Arc::downgrade(self),
                        self.updates.clone(),
                    );
                    let (harness, task) = harness::spawn(
                        root_session_id.to_owned(),
                        descriptor.id,
                        agent,
                        self.capacity.clone(),
                        Arc::downgrade(self),
                        contract.schema,
                        needs_assignment
                            .then(|| super::model::agent_prompt(descriptor.id, &descriptor.task)),
                    );
                    gates.push(start);
                    Some((harness, task, event_task))
                } else {
                    None
                };
                let mut state = self.state.lock().await;
                let session = state
                    .scopes
                    .get_mut(root_session_id)
                    .expect("restored scope")
                    .sessions
                    .get_mut(&descriptor.id)
                    .expect("restored child");
                session.host_context = host_context;
                session.output_validator = contract.validator;
                session.output_schema = child.output_schema;
                session.stored_runtime = child.runtime;
                session.next_turn_token = child.next_turn_token;
                session.last_output = child.last_output;
                session.status = match child.status {
                    AgentStatus::Pending | AgentStatus::Running => AgentStatus::Interrupted,
                    AgentStatus::Closing => AgentStatus::Closed,
                    status => status,
                };
                session.evicted = !closed && resources.is_none();
                if let Some((harness, task, event_task)) = resources {
                    session.harness = Some(harness);
                    session.harness_task = Some(task);
                    session.event_task = Some(event_task);
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            // No Added notifications or event streams escape a partial reconstruction.
            drop(gates);
            for (_, agent) in parents {
                drop(agent.shutdown().await);
            }
            let mut state = self.state.lock().await;
            state.scopes.remove(root_session_id);
            state
                .root_by_session
                .retain(|_, root_id| root_id != root_session_id);
            return Err(error);
        }
        for descriptor in ordered {
            let id = descriptor.id;
            let status = self.state.lock().await.scopes[root_session_id].sessions[&id]
                .status
                .clone();
            if !matches!(status, AgentStatus::Closed) {
                self.send(root_session_id, AgentUpdate::Added(descriptor));
            }
            self.send(root_session_id, AgentUpdate::Status { id, status });
        }
        for gate in gates {
            let _ = gate.send(());
        }
        self.changed();
        Ok(())
    }

    /// Returns embedding-private context for one retained child session.
    #[doc(hidden)]
    pub async fn host_context(&self, root_session_id: &str, id: AgentId) -> Option<Arc<str>> {
        self.state
            .lock()
            .await
            .scopes
            .get(root_session_id)
            .and_then(|scope| scope.sessions.get(&id))
            .and_then(|session| session.host_context.as_ref().map(Arc::clone))
    }

    /// Returns embedding-private context inherited by a descendant session.
    #[doc(hidden)]
    pub async fn host_context_for_session(&self, session_id: &str) -> Option<Arc<str>> {
        let state = self.state.lock().await;
        let root_session_id = state.root_by_session.get(session_id)?;
        state
            .scopes
            .get(root_session_id)?
            .sessions
            .values()
            .find(|session| session.descriptor.session_id == session_id)
            .and_then(|session| session.host_context.as_ref().map(Arc::clone))
    }

    pub(super) async fn reserve(&self, session_id: &str) -> std::io::Result<AgentReservation> {
        self.state.lock().await.reserve_for(session_id)
    }

    pub(super) async fn reserve_many(
        &self,
        session_id: &str,
        count: usize,
    ) -> std::io::Result<Vec<AgentReservation>> {
        let mut state = self.state.lock().await;
        (0..count).map(|_| state.reserve_for(session_id)).collect()
    }

    pub(super) async fn submit_result(
        &self,
        session_id: &str,
        turn_token: u64,
        output: Value,
    ) -> std::io::Result<bool> {
        self.state
            .lock()
            .await
            .submit_result(session_id, turn_token, output)
    }

    pub(super) async fn begin_turn_steer(
        &self,
        root_session_id: &str,
        id: AgentId,
    ) -> Option<TurnSteer> {
        self.state
            .lock()
            .await
            .begin_turn_steer(root_session_id, id)
    }

    pub(super) async fn finish_turn_steer(
        &self,
        root_session_id: &str,
        steer: TurnSteer,
        committed: bool,
    ) {
        self.state
            .lock()
            .await
            .finish_turn_steer(root_session_id, steer, committed);
    }

    pub(super) async fn insert(
        self: &Arc<Self>,
        root_session_id: String,
        descriptor: AgentDescriptor,
        host_context: Option<Arc<str>>,
        agent: Nanocodex,
        event_task: Task<()>,
        contract: OutputContract,
    ) -> std::io::Result<()> {
        let OutputContract { validator, schema } = contract;
        let mut state = self.state.lock().await;
        state.validate_insert(&root_session_id, &descriptor)?;
        let (harness, harness_task) = harness::spawn(
            root_session_id.clone(),
            descriptor.id,
            agent,
            self.capacity.clone(),
            Arc::downgrade(self),
            schema.clone(),
            None,
        );
        state.insert(
            root_session_id,
            descriptor.id,
            descriptor.session_id.clone(),
            ChildSession {
                descriptor,
                host_context,
                event_task: Some(event_task),
                harness: Some(harness),
                harness_task: Some(harness_task),
                status: AgentStatus::Pending,
                active: false,
                output_validator: validator,
                output_schema: serde_json::from_str(&schema).expect("compiled schema is JSON"),
                stored_runtime: None,
                next_turn_token: 0,
                active_turn_token: None,
                steering: false,
                submitted_output: None,
                last_output: None,
                last_used: 0,
                evicted: false,
            },
        )?;
        drop(state);
        self.changed();
        Ok(())
    }

    pub(super) async fn launch_initial_turn(
        self: &Arc<Self>,
        root_session_id: &str,
        id: AgentId,
        prompt: String,
        capacity: TurnCapacity,
    ) -> std::io::Result<()> {
        let harness = self
            .state
            .lock()
            .await
            .harness_in_scope(root_session_id, id)?;
        harness.start(prompt, capacity).await
    }

    pub(super) async fn harness_turn_started(
        &self,
        root_session_id: &str,
        id: AgentId,
    ) -> Option<u64> {
        let token = {
            let mut state = self.state.lock().await;
            let last_used = state.next_access();
            let session = state
                .scopes
                .get_mut(root_session_id)
                .and_then(|scope| scope.sessions.get_mut(&id))?;
            if !session.status.can_start_turn() || session.active {
                None
            } else {
                let token = session.next_turn_token.checked_add(1)?;
                session.next_turn_token = token;
                session.active_turn_token = Some(token);
                session.active = true;
                session.steering = false;
                session.submitted_output = None;
                session.last_used = last_used;
                session.status = AgentStatus::Running;
                Some(token)
            }
        };
        if token.is_some() {
            self.send(
                root_session_id,
                AgentUpdate::Status {
                    id,
                    status: AgentStatus::Running,
                },
            );
            self.changed();
        }
        token
    }

    pub(super) async fn harness_turn_start_failed(
        &self,
        root_session_id: &str,
        id: AgentId,
        error: String,
    ) {
        let status = {
            let mut state = self.state.lock().await;
            let Some(session) = state
                .scopes
                .get_mut(root_session_id)
                .and_then(|scope| scope.sessions.get_mut(&id))
            else {
                return;
            };
            session.active = false;
            session.active_turn_token = None;
            session.steering = false;
            session.submitted_output = None;
            if !matches!(session.status, AgentStatus::Closing | AgentStatus::Closed) {
                session.status = AgentStatus::Failed { error };
            }
            session.status.clone()
        };
        self.send(root_session_id, AgentUpdate::Status { id, status });
        self.changed();
    }

    pub(super) async fn harness_turn_finished(
        self: &Arc<Self>,
        root_session_id: &str,
        id: AgentId,
        result: AgentResult<TurnResult>,
    ) {
        let status = {
            let mut state = self.state.lock().await;
            let Some(session) = state
                .scopes
                .get_mut(root_session_id)
                .and_then(|scope| scope.sessions.get_mut(&id))
            else {
                return;
            };
            if !session.active {
                return;
            }
            session.active = false;
            session.active_turn_token = None;
            session.steering = false;
            let submitted_output = session.submitted_output.take();
            // Acceptance belongs to this turn even if cancellation/close wins settlement.
            // Keep its evidence, without claiming the interrupted execution completed.
            if let Some(output) = &submitted_output {
                session.last_output = Some(output.clone());
            }
            if matches!(session.status, AgentStatus::Closing | AgentStatus::Closed) {
                session.status.clone()
            } else {
                match result {
                    Ok(_) => complete_session(session, submitted_output),
                    Err(NanocodexError::TurnCancelled) => AgentStatus::Interrupted,
                    Err(error) => AgentStatus::Failed {
                        error: error.to_string(),
                    },
                }
            }
            .clone_into(&mut session.status);
            session.status.clone()
        };
        self.send(root_session_id, AgentUpdate::Status { id, status });
        self.changed();
        let registry = Arc::clone(self);
        let root_session_id = root_session_id.to_owned();
        drop(platform::spawn(async move {
            registry.enforce_resident_limit(&root_session_id).await;
        }));
    }

    async fn enforce_resident_limit(&self, root_session_id: &str) {
        let _residency_guard = self.residency_lock.lock().await;
        loop {
            // Serialize candidate selection with delivery admission. A message
            // is committed before this guard becomes available, so pending
            // mailbox work makes its target ineligible for eviction.
            let _message_guard = self.message_lock.lock().await;
            let eviction = self
                .state
                .lock()
                .await
                .take_resident_eviction(root_session_id, self.max_resident.load(Ordering::Relaxed));
            let Some(ResidentEviction { id, harness }) = eviction else {
                return;
            };
            match harness.snapshot().await {
                Ok(snapshot) => {
                    if let Some(session) = self
                        .state
                        .lock()
                        .await
                        .scopes
                        .get_mut(root_session_id)
                        .and_then(|scope| scope.sessions.get_mut(&id))
                    {
                        session.stored_runtime = Some(snapshot);
                    }
                }
                Err(_) => {
                    if let Some(session) = self
                        .state
                        .lock()
                        .await
                        .scopes
                        .get_mut(root_session_id)
                        .and_then(|scope| scope.sessions.get_mut(&id))
                    {
                        session.harness = Some(harness);
                        session.evicted = false;
                    }
                    return;
                }
            }
            self.changed();
            if harness.close().await.is_err() {
                self.harness_closed(root_session_id, id).await;
            }
            // Drain the old generation before another delivery may restore this
            // ID. Its late runtime_closed callback must never close the new driver.
            let tasks = {
                let mut state = self.state.lock().await;
                let session = state
                    .scopes
                    .get_mut(root_session_id)
                    .and_then(|scope| scope.sessions.get_mut(&id));
                session.map(|session| (session.harness_task.take(), session.event_task.take()))
            };
            if let Some((harness_task, event_task)) = tasks {
                if let Some(task) = harness_task {
                    let _ = task.await;
                }
                if let Some(task) = event_task {
                    let _ = task.await;
                }
            }
        }
    }

    pub(super) async fn harness_closed(&self, root_session_id: &str, id: AgentId) {
        let status_update = {
            let mut state = self.state.lock().await;
            let Some(session) = state
                .scopes
                .get_mut(root_session_id)
                .and_then(|scope| scope.sessions.get_mut(&id))
            else {
                return;
            };
            if matches!(session.status, AgentStatus::Closed) {
                None
            } else {
                session.harness = None;
                session.active = false;
                session.active_turn_token = None;
                session.steering = false;
                session.submitted_output = None;
                if session.evicted && !matches!(session.status, AgentStatus::Closing) {
                    None
                } else {
                    session.evicted = false;
                    session.status = AgentStatus::Closed;
                    Some(AgentStatus::Closed)
                }
            }
        };
        if let Some(status) = status_update {
            self.send(root_session_id, AgentUpdate::Status { id, status });
        }
        self.changed();
    }

    async fn runtime_closed(&self, root_session_id: &str, id: AgentId) {
        let harness = {
            let state = self.state.lock().await;
            state
                .scopes
                .get(root_session_id)
                .and_then(|scope| scope.sessions.get(&id))
                .filter(|session| {
                    !matches!(session.status, AgentStatus::Closing | AgentStatus::Closed)
                })
                .and_then(|session| session.harness.clone())
        };
        let Some(harness) = harness else {
            self.harness_closed(root_session_id, id).await;
            return;
        };
        drop(harness.close().await);
    }

    pub(super) fn send(&self, root_session_id: &str, update: AgentUpdate) {
        let _ = send_update(&self.updates, root_session_id, update);
    }

    pub async fn directory(
        &self,
        session_id: &str,
        include_completed: bool,
        include_self: bool,
    ) -> Vec<AgentDirectoryEntry> {
        self.state
            .lock()
            .await
            .directory(session_id, include_completed, include_self)
    }

    /// Keep weak factory capabilities, never a second owner of a child driver.
    pub(crate) fn register_handle(&self, handle: AgentHandle) {
        self.session_handles
            .write()
            .expect("session handles poisoned")
            .insert(handle.session_id().to_owned(), handle);
    }

    // Caller holds residency_lock and message_lock, fencing eviction, close and
    // competing deliveries until the exact child is rehydrated and admitted.
    async fn rehydrate(
        self: &Arc<Self>,
        session_id: &str,
        to: AgentId,
        purpose: MessagePurpose,
    ) -> std::io::Result<()> {
        let (root, mut missing) = {
            let state = self.state.lock().await;
            let root = state.root_session_id(session_id).to_owned();
            let scope = state
                .scopes
                .get(&root)
                .ok_or_else(|| std::io::Error::other(format!("unknown agent_id {to}")))?;
            if scope.topology.agent_for_session(session_id) == Some(to) {
                return Err(std::io::Error::other("agents cannot message themselves"));
            }
            if purpose == MessagePurpose::Delegate {
                scope.topology.authorize(session_id, to)?;
            }
            let mut id = to;
            let mut missing = Vec::new();
            loop {
                let session = scope
                    .sessions
                    .get(&id)
                    .ok_or_else(|| std::io::Error::other(format!("unknown agent_id {id}")))?;
                if matches!(
                    session.status,
                    AgentStatus::Pending | AgentStatus::Closing | AgentStatus::Closed
                ) || session.harness.is_some()
                {
                    break;
                }
                let Some(snapshot) = session.stored_runtime.clone() else {
                    break;
                };
                let parent_session = session
                    .descriptor
                    .parent
                    .and_then(|parent| scope.sessions.get(&parent))
                    .map_or_else(
                        || root.clone(),
                        |parent| parent.descriptor.session_id.clone(),
                    );
                missing.push((
                    id,
                    parent_session,
                    snapshot,
                    session.host_context.clone(),
                    session.output_schema.clone(),
                    session.descriptor.task.clone(),
                ));
                match session.descriptor.parent {
                    Some(parent) => id = parent,
                    None => break,
                }
            }
            (root, missing)
        };
        while let Some((id, parent_session, snapshot, host_context, schema, task)) = missing.pop() {
            let parent = self
                .session_handles
                .read()
                .expect("session handles poisoned")
                .get(&parent_session)
                .cloned()
                .ok_or_else(|| {
                    std::io::Error::other("subagent parent runtime is unavailable for restoration")
                })?;
            let contract = OutputContract::compile(&schema)?;
            let needs_assignment = snapshot.conversation.is_none();
            let (agent, events) = parent
                .restore_child(snapshot, host_context)
                .await
                .map_err(std::io::Error::other)?;
            let (start, ready) = oneshot::channel();
            let event_task = forward_events(
                root.clone(),
                id,
                events,
                ready,
                Arc::downgrade(self),
                self.updates.clone(),
            );
            let (harness, task) = harness::spawn(
                root.clone(),
                id,
                agent,
                self.capacity.clone(),
                Arc::downgrade(self),
                contract.schema,
                needs_assignment.then(|| super::model::agent_prompt(id, &task)),
            );
            let mut state = self.state.lock().await;
            let session = state
                .scopes
                .get_mut(&root)
                .expect("locked scope")
                .sessions
                .get_mut(&id)
                .expect("retained child");
            session.harness = Some(harness);
            session.harness_task = Some(task);
            session.event_task = Some(event_task);
            session.evicted = false;
            drop(state);
            let _ = start.send(());
        }
        Ok(())
    }

    pub async fn send_message(
        self: &Arc<Self>,
        session_id: &str,
        to: AgentId,
        priority: MessagePriority,
        purpose: MessagePurpose,
        in_reply_to: Option<MessageId>,
        body: String,
    ) -> std::io::Result<MessageReceipt> {
        let _residency_guard = self.residency_lock.lock().await;
        let _message_guard = self.message_lock.lock().await;
        self.rehydrate(session_id, to, purpose).await?;
        let prepared = self.state.lock().await.prepare_message(
            session_id,
            to,
            priority,
            purpose,
            in_reply_to,
            body,
        )?;
        let delivery = prepared
            .harness
            .enqueue_delivery(prepared.message.clone())?;
        self.state
            .lock()
            .await
            .commit_message(&prepared.root_session_id, prepared.message.clone())?;
        let disposition = match delivery.release().await {
            Ok(disposition) => disposition,
            Err(error) => {
                self.state
                    .lock()
                    .await
                    .rollback_message(&prepared.root_session_id, prepared.message.id);
                return Err(error);
            }
        };
        Ok(MessageReceipt {
            message_id: prepared.message.id,
            thread_id: prepared.message.thread_id,
            from: prepared.message.from,
            to_agent_id: prepared.message.to,
            disposition,
        })
    }

    pub(super) async fn message_admitted(
        &self,
        root_session_id: &str,
        id: MessageId,
        disposition: MessageDisposition,
    ) {
        let thread = {
            let mut state = self.state.lock().await;
            let thread = state.thread_for_message(root_session_id, id);
            state.mark_message_admitted(root_session_id, id, disposition);
            thread
        };
        let Some(thread) = thread else {
            return;
        };
        self.send(
            root_session_id,
            AgentUpdate::Message(AgentMessageUpdate {
                message_id: id,
                thread,
                delivery: MessageDeliveryState::Admitted { disposition },
            }),
        );
        self.changed();
    }

    pub(super) async fn message_rejected(&self, root_session_id: &str, id: MessageId) {
        self.state
            .lock()
            .await
            .rollback_message(root_session_id, id);
    }

    pub(super) async fn message_delivered(
        &self,
        root_session_id: &str,
        id: MessageId,
        disposition: MessageDisposition,
    ) {
        self.publish_message_state(
            root_session_id,
            id,
            MessageDeliveryState::Delivered { disposition },
        )
        .await;
    }

    pub(super) async fn begin_message_delivery(
        &self,
        root_session_id: &str,
        id: MessageId,
    ) -> Option<DelegationChange> {
        let (change, descriptor) = self
            .state
            .lock()
            .await
            .begin_delegation(root_session_id, id)?;
        self.send(root_session_id, AgentUpdate::Added(descriptor));
        self.changed();
        Some(change)
    }

    pub(super) async fn rollback_message_delivery(
        &self,
        root_session_id: &str,
        change: DelegationChange,
    ) {
        let descriptor = self
            .state
            .lock()
            .await
            .rollback_delegation(root_session_id, change);
        if let Some(descriptor) = descriptor {
            self.send(root_session_id, AgentUpdate::Added(descriptor));
            self.changed();
        }
    }

    pub(super) async fn message_failed(&self, root_session_id: &str, id: MessageId, error: String) {
        self.publish_message_state(root_session_id, id, MessageDeliveryState::Failed { error })
            .await;
    }

    async fn publish_message_state(
        &self,
        root_session_id: &str,
        message_id: MessageId,
        delivery: MessageDeliveryState,
    ) {
        let thread = self
            .state
            .lock()
            .await
            .thread_for_message(root_session_id, message_id);
        let Some(thread) = thread else {
            return;
        };
        self.send(
            root_session_id,
            AgentUpdate::Message(AgentMessageUpdate {
                message_id,
                thread,
                delivery,
            }),
        );
        self.changed();
        self.state
            .lock()
            .await
            .mark_message_terminal(root_session_id, message_id);
    }

    pub async fn wait(
        &self,
        session_id: &str,
        ids: &[AgentId],
        duration: Duration,
    ) -> std::io::Result<(Vec<AgentSummary>, bool)> {
        if ids.is_empty() {
            return Err(std::io::Error::other("agent_ids must not be empty"));
        }
        let mut revision = self.revision.subscribe();
        let deadline = Instant::now() + duration;
        loop {
            let summaries = self.state.lock().await.summaries(session_id, ids)?;
            if summaries
                .iter()
                .any(|summary| summary.status.is_wait_terminal())
            {
                return Ok((summaries, false));
            }
            if timeout_at(deadline, revision.changed()).await.is_err() {
                let summaries = self.state.lock().await.summaries(session_id, ids)?;
                return Ok((summaries, true));
            }
        }
    }

    pub async fn interrupt(
        &self,
        session_id: &str,
        id: AgentId,
    ) -> std::io::Result<Vec<AgentSummary>> {
        let _message_guard = self.message_lock.lock().await;
        let (root_session_id, ids, harnesses) = {
            let mut state = self.state.lock().await;
            state.request_interrupt(session_id, id)?
        };
        self.changed();
        let deadline = Instant::now() + AGENT_STOP_TIMEOUT;
        self.interrupt_harnesses(&root_session_id, &ids, harnesses, deadline)
            .await?;
        self.state
            .lock()
            .await
            .summaries_in_scope(&root_session_id, &ids)
    }

    pub async fn close(&self, session_id: &str, id: AgentId) -> std::io::Result<Vec<AgentSummary>> {
        let _message_guard = self.message_lock.lock().await;
        let CloseRequest {
            root_session_id,
            ids,
            harnesses,
            status_updates,
        } = {
            let mut state = self.state.lock().await;
            state.request_close(session_id, id)?
        };
        for (id, status) in status_updates {
            self.send(&root_session_id, AgentUpdate::Status { id, status });
        }
        self.changed();
        self.stop_and_close(root_session_id, ids, harnesses).await
    }

    async fn close_batch(&self, root_session_id: &str, ids: Vec<AgentId>) {
        for id in ids.into_iter().rev() {
            drop(self.close(root_session_id, id).await);
        }
    }

    async fn close_all(&self, session_id: &str) -> std::io::Result<Vec<AgentSummary>> {
        let _message_guard = self.message_lock.lock().await;
        let CloseRequest {
            root_session_id,
            ids,
            harnesses,
            status_updates,
        } = {
            let mut state = self.state.lock().await;
            state.request_close_all(session_id)?
        };
        for (id, status) in status_updates {
            self.send(&root_session_id, AgentUpdate::Status { id, status });
        }
        self.changed();
        self.stop_and_close(root_session_id, ids, harnesses).await
    }

    async fn stop_and_close(
        &self,
        root_session_id: String,
        ids: Vec<AgentId>,
        harnesses: Vec<HarnessHandle>,
    ) -> std::io::Result<Vec<AgentSummary>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let deadline = Instant::now() + AGENT_STOP_TIMEOUT;
        let closing_result = self.close_harnesses(harnesses, deadline).await;
        self.wait_until_inactive(&root_session_id, &ids, deadline)
            .await?;
        // Closing an already-finished driver can race with its natural shutdown.
        // Once every harness reports inactive, command errors no longer identify
        // a live resource and must not prevent task handles from being joined.
        drop(closing_result);
        let ClosedSessions {
            summaries,
            harness_tasks,
            event_tasks,
        } = self
            .state
            .lock()
            .await
            .finish_close(&root_session_id, &ids)?;
        for summary in &summaries {
            self.send(
                &root_session_id,
                AgentUpdate::Status {
                    id: summary.agent_id,
                    status: AgentStatus::Closed,
                },
            );
        }
        self.changed();
        self.wait_for_tasks(harness_tasks, deadline, "subagent harnesses")
            .await?;
        self.wait_for_tasks(event_tasks, deadline, "subagent event streams")
            .await?;
        Ok(summaries)
    }

    async fn cancel_all(&self, session_id: &str) {
        let _message_guard = self.message_lock.lock().await;
        let (root_session_id, ids, harnesses) = {
            let mut state = self.state.lock().await;
            state.request_interrupt_all(session_id)
        };
        self.changed();
        let deadline = Instant::now() + AGENT_STOP_TIMEOUT;
        drop(
            self.interrupt_harnesses(&root_session_id, &ids, harnesses, deadline)
                .await,
        );
    }

    async fn interrupt_harnesses(
        &self,
        root_session_id: &str,
        ids: &[AgentId],
        harnesses: Vec<HarnessHandle>,
        deadline: Instant,
    ) -> std::io::Result<()> {
        let interruption = async move {
            let results = join_all(
                harnesses
                    .into_iter()
                    .map(|harness| async move { harness.interrupt().await }),
            )
            .await;
            first_error(results)
        };
        let interruption_result = timeout_at(deadline, interruption).await.map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out interrupting subagent harnesses",
            )
        })?;
        self.wait_until_inactive(root_session_id, ids, deadline)
            .await?;
        drop(interruption_result);
        Ok(())
    }

    async fn close_harnesses(
        &self,
        harnesses: Vec<HarnessHandle>,
        deadline: Instant,
    ) -> std::io::Result<()> {
        let closing = async move {
            let results = join_all(
                harnesses
                    .into_iter()
                    .map(|harness| async move { harness.close().await }),
            )
            .await;
            first_error(results)
        };
        timeout_at(deadline, closing).await.map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out closing subagent harnesses",
            )
        })?
    }

    async fn wait_for_tasks(
        &self,
        mut tasks: Vec<Task<()>>,
        deadline: Instant,
        description: &str,
    ) -> std::io::Result<()> {
        if tasks.is_empty() {
            return Ok(());
        }
        let completion = join_all(tasks.iter_mut());
        match timeout_at(deadline, completion).await {
            Ok(results) => results
                .into_iter()
                .find_map(Result::err)
                .map_or(Ok(()), |error| {
                    Err(std::io::Error::other(format!(
                        "{description} failed during shutdown: {error}"
                    )))
                }),
            Err(_) => {
                for task in tasks {
                    task.abort();
                }
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("timed out waiting for {description} to close"),
                ))
            }
        }
    }

    async fn wait_until_inactive(
        &self,
        root_session_id: &str,
        ids: &[AgentId],
        deadline: Instant,
    ) -> std::io::Result<()> {
        let mut revision = self.revision.subscribe();
        loop {
            if self.state.lock().await.all_inactive(root_session_id, ids)? {
                return Ok(());
            }
            timeout_at(deadline, revision.changed())
                .await
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "timed out waiting for subagent turns to stop",
                    )
                })?
                .map_err(|_| std::io::Error::other("subagent runtime is closed"))?;
        }
    }

    fn changed(&self) {
        self.revision.send_modify(|revision| {
            *revision = revision.wrapping_add(1);
        });
    }
}

fn complete_session(session: &mut ChildSession, output: Option<Value>) -> AgentStatus {
    let Some(output) = output else {
        return AgentStatus::Failed {
            error: CompletionError::new(
                CompletionErrorCode::MissingResult,
                false,
                "subagent turn ended without a valid submit_result call",
                "Inspect child evidence before assigning recovery. A plain final answer is not an accepted result; do not replay task side effects.",
            ).to_string(),
        };
    };
    session.last_output = Some(output.clone());
    AgentStatus::Completed { output }
}

/// Preserve valid JSON values; tolerate one encoded container only when the
/// decoded value independently satisfies the exact child contract.
fn validate_submitted_output(
    validator: &Validator,
    output: Value,
) -> Result<(Value, bool), CompletionError> {
    if validator.is_valid(&output) {
        return Ok((output, false));
    }
    const MAX_ENCODED_OUTPUT_BYTES: usize = 1_048_576;
    if let Value::String(text) = &output
        && text.len() <= MAX_ENCODED_OUTPUT_BYTES
        && let Ok(decoded) = serde_json::from_str::<Value>(text)
        && matches!(decoded, Value::Object(_) | Value::Array(_))
        && validator.is_valid(&decoded)
    {
        return Ok((decoded, true));
    }
    let errors = validator
        .iter_errors(&output)
        .take(4)
        .map(|error| {
            format!(
                "instance {}: schema {}",
                error.instance_path(),
                error.schema_path()
            )
        })
        .collect::<Vec<_>>();
    Err(CompletionError::new(
        CompletionErrorCode::SchemaValidation,
        true,
        "submitted output does not match the required schema",
        "Correct output using the required schema and retry submit_result within this turn; do not repeat task side effects.",
    ).with_details(errors))
}

fn restored_tombstone(descriptor: AgentDescriptor, host_context: Option<Arc<str>>) -> ChildSession {
    ChildSession {
        descriptor,
        host_context,
        event_task: None,
        harness: None,
        harness_task: None,
        status: AgentStatus::Interrupted,
        active: false,
        output_validator: jsonschema::validator_for(&Value::Bool(false))
            .expect("the false JSON Schema is valid"),
        output_schema: Value::Bool(false),
        stored_runtime: None,
        next_turn_token: 0,
        active_turn_token: None,
        steering: false,
        submitted_output: None,
        last_output: None,
        last_used: 0,
        evicted: true,
    }
}

fn first_error(results: Vec<std::io::Result<()>>) -> std::io::Result<()> {
    results.into_iter().find(Result::is_err).unwrap_or(Ok(()))
}

fn bounded_summary(value: &str) -> String {
    const MAX_BYTES: usize = 160;
    if value.len() <= MAX_BYTES {
        return value.to_owned();
    }
    let end = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= MAX_BYTES)
        .last()
        .unwrap_or_default();
    value[..end].to_owned()
}

impl ChildSession {
    pub(super) fn summary(&self) -> AgentSummary {
        let last_output = if matches!(self.status, AgentStatus::Completed { .. }) {
            None
        } else {
            self.last_output.clone()
        };
        AgentSummary {
            agent_id: self.descriptor.id,
            role: self.descriptor.role.clone(),
            task: self.descriptor.task.clone(),
            parent_agent_id: self.descriptor.parent,
            status: self.status.clone(),
            last_output,
        }
    }
}

#[derive(Clone)]
pub struct SubagentControl {
    registry: Arc<Registry>,
}

impl SubagentControl {
    pub fn set_max_concurrency(&self, limit: usize) {
        self.registry.set_max_concurrency(limit);
    }

    /// Changes the maximum number of reusable subagent runtimes retained after
    /// their turns become inactive. Values below one are clamped to one.
    pub fn set_max_resident(&self, limit: usize) {
        self.registry.set_max_resident(limit);
    }

    pub async fn cancel_all(&self, root_session_id: &str) {
        self.registry.cancel_all(root_session_id).await;
    }

    pub async fn close_all(&self, root_session_id: &str) -> std::io::Result<()> {
        self.registry.close_all(root_session_id).await.map(drop)
    }

    pub fn runtime_id(&self) -> SubagentRuntimeId {
        self.registry.id
    }
}

pub(super) fn forward_events(
    root_session_id: String,
    id: AgentId,
    mut events: AgentEvents,
    start: oneshot::Receiver<()>,
    registry: Weak<Registry>,
    updates: mpsc::UnboundedSender<ScopedAgentUpdate>,
) -> Task<()> {
    platform::spawn(async move {
        if start.await.is_err() {
            return;
        }
        while let Some(event) = events.recv().await {
            if !send_update(&updates, &root_session_id, AgentUpdate::Event { id, event }) {
                return;
            }
        }
        if let Some(registry) = registry.upgrade() {
            registry.runtime_closed(&root_session_id, id).await;
        }
    })
}

fn send_update(
    updates: &mpsc::UnboundedSender<ScopedAgentUpdate>,
    root_session_id: &str,
    update: AgentUpdate,
) -> bool {
    updates
        .send(ScopedAgentUpdate {
            root_session_id: root_session_id.to_owned(),
            update,
        })
        .is_ok()
}

pub fn channel(
    max_concurrency: usize,
) -> (
    Arc<Registry>,
    SubagentControl,
    mpsc::UnboundedReceiver<ScopedAgentUpdate>,
) {
    let (updates, receiver) = mpsc::unbounded_channel();
    let registry = Arc::new(Registry::new(updates, max_concurrency));
    let control = SubagentControl {
        registry: Arc::clone(&registry),
    };
    (registry, control, receiver)
}

#[cfg(test)]
mod tests {
    use super::{
        AgentDescriptor, AgentId, AgentStatus, ChildSession, OutputContract, Registry,
        RegistryState, complete_session, completion_instructions, forward_events,
    };
    use crate::platform;
    use crate::{
        AgentUpdate, MessageDeliveryState, MessageDisposition, MessagePriority, MessagePurpose,
    };
    use nanocodex_agent::{
        AgentEvents, Nanocodex, OpenAi, ResponseError,
        transport::{ResponsesAttempt, ResponsesServiceResponse},
    };
    use serde_json::json;
    use std::{
        collections::HashMap,
        future::{Pending, pending},
        result::Result as StdResult,
        sync::Arc,
        task::{Context, Poll},
        time::Duration,
    };
    use tokio::{
        sync::{Notify, mpsc, oneshot},
        time::timeout,
    };
    use tower::Service;

    #[derive(Clone)]
    struct PendingService {
        called: Arc<Notify>,
    }

    impl Service<ResponsesAttempt> for PendingService {
        type Response = ResponsesServiceResponse;
        type Error = ResponseError;
        type Future = Pending<StdResult<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<StdResult<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _request: ResponsesAttempt) -> Self::Future {
            self.called.notify_one();
            pending()
        }
    }

    fn pending_agent(called: Arc<Notify>) -> (Nanocodex, AgentEvents) {
        let openai = OpenAi::builder("test-key")
            .service(move || PendingService {
                called: Arc::clone(&called),
            })
            .build()
            .unwrap();
        Nanocodex::builder(openai).build().unwrap()
    }

    struct TestSpawnRouter {
        resolutions: std::sync::atomic::AtomicUsize,
        bindings: std::sync::atomic::AtomicUsize,
        reject_resolution: bool,
        reject_binding: usize,
    }

    #[async_trait::async_trait]
    impl crate::SpawnRouter for TestSpawnRouter {
        async fn resolve(
            &self,
            _parent: &str,
            _role: &str,
            _task: &str,
            _options: nanocodex_agent::SpawnOptions,
            _context: Option<&str>,
        ) -> std::io::Result<crate::SpawnRoute> {
            self.resolutions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.reject_resolution {
                return Err(std::io::Error::other("routing authorization denied"));
            }
            Ok(crate::SpawnRoute {
                options: nanocodex_agent::SpawnOptions::new()
                    .model(nanocodex_agent::Model::Sol)
                    .thinking(nanocodex_agent::Thinking::High),
                reference: "prepared-route".to_owned(),
            })
        }

        fn bind(
            &self,
            _parent: &str,
            _child: &str,
            _reference: &str,
            _context: Option<&str>,
        ) -> std::io::Result<()> {
            let count = self
                .bindings
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            if count == self.reject_binding {
                return Err(std::io::Error::other("durable pin failed"));
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn routed_spawn_failures_never_start_a_child_or_publish_it() {
        use crate::tools::{AgentTask, start_agent_with, start_agents};
        use nanocodex_agent::{Model, SpawnOptions};
        use std::sync::atomic::Ordering;

        // Authorization, explicit override conflicts, single pin failures, and
        // a late batch pin failure must all happen before any model work starts.
        for (batch, reject_resolution, reject_binding, explicit_override, expected_children) in [
            (false, true, 0, false, 0),
            (false, false, 0, true, 0),
            (false, false, 1, false, 1),
            (true, true, 0, false, 0),
            (true, false, 2, false, 2),
        ] {
            let called = Arc::new(Notify::new());
            let service_called = Arc::clone(&called);
            let openai = OpenAi::builder("test-key")
                .service(move || PendingService {
                    called: Arc::clone(&service_called),
                })
                .build()
                .unwrap();
            let (handles, mut received_handles) = mpsc::unbounded_channel();
            let (parent, _events) = Nanocodex::builder(openai)
                .tools_factory(move |handle| {
                    handles.send(handle).unwrap();
                    nanocodex_tools::Tools::builder().without_defaults().build()
                })
                .build()
                .unwrap();
            let parent_handle = received_handles.recv().await.unwrap();
            let (updates, mut receiver) = mpsc::unbounded_channel();
            let registry = Arc::new(Registry::new(updates, 2));
            let router = Arc::new(TestSpawnRouter {
                resolutions: 0.into(),
                bindings: 0.into(),
                reject_resolution,
                reject_binding,
            });
            registry.set_spawn_router(router.clone());
            let task = || AgentTask {
                role: "worker".to_owned(),
                task: "work".to_owned(),
                output_schema: json!({ "type": "object" }),
            };
            let error = if batch {
                start_agents(
                    &parent_handle,
                    &registry,
                    parent.session_id(),
                    vec![task(), task()],
                )
                .await
                .err()
                .unwrap()
            } else {
                let options = if explicit_override {
                    SpawnOptions::new().model(Model::Astra)
                } else {
                    SpawnOptions::new()
                };
                start_agent_with(
                    &parent_handle,
                    &registry,
                    parent.session_id(),
                    task(),
                    options,
                )
                .await
                .err()
                .unwrap()
            };
            let expected_error = if reject_resolution {
                "authorization denied"
            } else if explicit_override {
                "explicit override"
            } else {
                "durable pin failed"
            };
            assert!(error.to_string().contains(expected_error), "{error}");
            assert!(
                registry
                    .directory(parent.session_id(), true, false)
                    .await
                    .is_empty()
            );
            assert!(receiver.try_recv().is_err());
            assert!(
                timeout(Duration::from_millis(20), called.notified())
                    .await
                    .is_err()
            );
            for _ in 0..expected_children {
                let child = received_handles.try_recv().unwrap();
                assert!(
                    child.spawn().await.is_err(),
                    "failed child must be shut down"
                );
            }
            assert!(received_handles.try_recv().is_err());
            assert_eq!(router.bindings.load(Ordering::SeqCst), expected_children);
            assert!(
                registry.reserve_turns(2).is_ok(),
                "failed spawn must release capacity"
            );
            parent.shutdown().await.unwrap();
        }
    }

    fn test_contract() -> OutputContract {
        OutputContract {
            validator: jsonschema::validator_for(&json!({})).unwrap(),
            schema: "{}".to_owned(),
        }
    }

    #[test]
    fn output_contract_renders_the_schema_for_every_turn() {
        let schema = json!({
            "type": "object",
            "properties": { "report": { "type": "string" } },
            "required": ["report"]
        });

        let contract = OutputContract::compile(&schema).unwrap();
        let instructions = completion_instructions(&contract.schema, 7);

        assert!(instructions.contains("actual tool catalog"));
        assert!(instructions.contains("unless that catalog exposes it"));
        assert!(instructions.contains("Pass objects and arrays directly as JSON values"));
        assert!(instructions.contains("turn_token: 7"));
        assert!(instructions.contains("exactly once"));
        assert!(instructions.contains("\"report\""));
        assert!(contract.validator.is_valid(&json!({ "report": "done" })));
    }

    #[tokio::test]
    async fn batch_reservations_are_stable_and_contiguous() {
        let (updates, _receiver) = mpsc::unbounded_channel();
        let registry = Registry::new(updates, 3);

        let batch = registry.reserve_many("root", 3).await.unwrap();
        assert_eq!(
            batch
                .into_iter()
                .map(|reservation| reservation.id)
                .collect::<Vec<_>>(),
            [AgentId::new(1), AgentId::new(2), AgentId::new(3)]
        );
        assert_eq!(registry.reserve("root").await.unwrap().id, AgentId::new(4));
    }

    #[tokio::test]
    async fn restored_tombstones_preserve_topology_wait_updates_and_allocation() {
        let (updates, mut receiver) = mpsc::unbounded_channel();
        let registry = Registry::new(updates, 3);
        let parent = AgentDescriptor {
            id: AgentId::new(2),
            session_id: "parent-session".to_owned(),
            role: "parent".to_owned(),
            task: "coordinate".to_owned(),
            parent: None,
        };
        let child = AgentDescriptor {
            id: AgentId::new(7),
            session_id: "child-session".to_owned(),
            role: "child".to_owned(),
            task: "investigate".to_owned(),
            parent: Some(parent.id),
        };

        registry
            .restore("root", vec![child.clone(), parent.clone()])
            .await
            .unwrap();

        let directory = registry.directory("root", true, false).await;
        assert_eq!(directory.len(), 2);
        let restored_parent = directory
            .iter()
            .find(|entry| entry.agent_id == parent.id)
            .unwrap();
        assert_eq!(restored_parent.parent_agent_id, None);
        assert_eq!(restored_parent.status, AgentStatus::Interrupted);
        assert!(!restored_parent.can_message);
        assert!(restored_parent.can_manage);
        let restored_child = directory
            .iter()
            .find(|entry| entry.agent_id == child.id)
            .unwrap();
        assert_eq!(restored_child.parent_agent_id, Some(parent.id));
        assert_eq!(restored_child.status, AgentStatus::Interrupted);
        assert!(!restored_child.can_message);
        assert!(restored_child.can_manage);

        let (summaries, timed_out) = registry
            .wait("root", &[parent.id, child.id], Duration::from_millis(1))
            .await
            .unwrap();
        assert!(!timed_out);
        assert!(
            summaries
                .iter()
                .all(|summary| summary.status == AgentStatus::Interrupted)
        );
        assert_eq!(registry.reserve("root").await.unwrap().id, AgentId::new(8));

        for expected in [&parent, &child] {
            let added = receiver.try_recv().unwrap();
            assert_eq!(added.root_session_id, "root");
            let AgentUpdate::Added(descriptor) = added.update else {
                panic!("expected an Added update");
            };
            assert_eq!(descriptor, *expected);
            let interrupted = receiver.try_recv().unwrap();
            assert_eq!(interrupted.root_session_id, "root");
            let AgentUpdate::Status { id, status } = interrupted.update else {
                panic!("expected a Status update");
            };
            assert_eq!(id, expected.id);
            assert_eq!(status, AgentStatus::Interrupted);
        }
        assert!(receiver.try_recv().is_err());

        let closed = registry.close_all("root").await.unwrap();
        assert_eq!(
            closed
                .iter()
                .map(|summary| (summary.agent_id, &summary.status))
                .collect::<Vec<_>>(),
            [
                (child.id, &AgentStatus::Closed),
                (parent.id, &AgentStatus::Closed),
            ]
        );
        for expected_status in [AgentStatus::Closing, AgentStatus::Closed] {
            for expected_id in [child.id, parent.id] {
                let update = receiver.try_recv().unwrap();
                let AgentUpdate::Status { id, status } = update.update else {
                    panic!("expected a cleanup Status update");
                };
                assert_eq!(update.root_session_id, "root");
                assert_eq!(id, expected_id);
                assert_eq!(status, expected_status);
            }
        }
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn restored_host_contexts_remain_private_and_session_scoped() {
        let (updates, _receiver) = mpsc::unbounded_channel();
        let registry = Registry::new(updates, 3);
        let first = AgentDescriptor {
            id: AgentId::new(1),
            session_id: "first-session".to_owned(),
            role: "first".to_owned(),
            task: "inspect".to_owned(),
            parent: None,
        };
        let second = AgentDescriptor {
            id: AgentId::new(2),
            session_id: "second-session".to_owned(),
            role: "second".to_owned(),
            task: "review".to_owned(),
            parent: None,
        };

        registry
            .restore_with_host_contexts(
                "root",
                vec![first.clone(), second.clone()],
                HashMap::from([
                    (
                        first.session_id.clone(),
                        Some(Arc::<str>::from("opaque-first-turn")),
                    ),
                    (
                        second.session_id.clone(),
                        Some(Arc::<str>::from("opaque-second-turn")),
                    ),
                ]),
            )
            .await
            .unwrap();

        assert_eq!(
            registry.host_context("root", first.id).await.as_deref(),
            Some("opaque-first-turn")
        );
        assert_eq!(
            registry.host_context("root", second.id).await.as_deref(),
            Some("opaque-second-turn")
        );
        assert_eq!(
            registry
                .host_context_for_session("first-session")
                .await
                .as_deref(),
            Some("opaque-first-turn")
        );
        assert_eq!(registry.host_context("other-root", first.id).await, None);
        assert_eq!(
            registry.directory("root", true, false).await.len(),
            2,
            "private context must not change the public directory projection"
        );

        registry.restore("root", Vec::new()).await.unwrap();
        assert_eq!(
            registry.host_context("root", first.id).await.as_deref(),
            Some("opaque-first-turn"),
            "an empty compatibility restore must not clear a live context"
        );
        assert_eq!(
            registry.host_context("root", second.id).await.as_deref(),
            Some("opaque-second-turn")
        );
    }

    #[tokio::test]
    async fn restored_host_context_keys_are_validated_before_registry_mutation() {
        let first = AgentDescriptor {
            id: AgentId::new(1),
            session_id: "first-session".to_owned(),
            role: "worker".to_owned(),
            task: "inspect".to_owned(),
            parent: None,
        };
        let second = AgentDescriptor {
            id: AgentId::new(2),
            session_id: "second-session".to_owned(),
            role: "reviewer".to_owned(),
            task: "review".to_owned(),
            parent: None,
        };

        for host_contexts in [
            HashMap::from([(
                first.session_id.clone(),
                Some(Arc::<str>::from("opaque-turn")),
            )]),
            HashMap::from([
                (
                    first.session_id.clone(),
                    Some(Arc::<str>::from("opaque-turn")),
                ),
                (
                    second.session_id.clone(),
                    Some(Arc::<str>::from("second-turn")),
                ),
                (
                    "unknown-session".to_owned(),
                    Some(Arc::<str>::from("other-turn")),
                ),
            ]),
        ] {
            let (updates, _receiver) = mpsc::unbounded_channel();
            let registry = Registry::new(updates, 3);
            let error = registry
                .restore_with_host_contexts(
                    "root",
                    vec![first.clone(), second.clone()],
                    host_contexts,
                )
                .await
                .unwrap_err();

            assert!(error.to_string().contains("must exactly match"));
            assert!(registry.directory("root", true, false).await.is_empty());
            for descriptor in [&first, &second] {
                assert!(registry.is_root_session(&descriptor.session_id).await);
            }
        }
    }

    #[tokio::test]
    async fn invalid_restored_topologies_are_rejected_atomically() {
        let descriptor = |id: u64, session_id: &str, parent: Option<u64>| AgentDescriptor {
            id: AgentId::new(id),
            session_id: session_id.to_owned(),
            role: "worker".to_owned(),
            task: "work".to_owned(),
            parent: parent.map(AgentId::new),
        };
        let cases = [
            (
                vec![descriptor(0, "zero", None)],
                "agent ID must be greater than zero",
            ),
            (
                vec![descriptor(u64::MAX, "maximum", None)],
                "agent ID must be less than the maximum u64 value",
            ),
            (
                vec![descriptor(1, "first", None), descriptor(1, "second", None)],
                "duplicate restored agent_id 1",
            ),
            (
                vec![
                    descriptor(1, "duplicate-session", None),
                    descriptor(2, "duplicate-session", None),
                ],
                "duplicate restored subagent session ID duplicate-session",
            ),
            (
                vec![descriptor(1, "orphan", Some(9))],
                "unknown parent or cycle",
            ),
            (
                vec![
                    descriptor(1, "cycle-a", Some(2)),
                    descriptor(2, "cycle-b", Some(1)),
                ],
                "unknown parent or cycle",
            ),
        ];

        for (descriptors, expected_error) in cases {
            let (updates, mut receiver) = mpsc::unbounded_channel();
            let registry = Registry::new(updates, 3);
            let error = registry.restore("root", descriptors).await.unwrap_err();
            assert!(
                error.to_string().contains(expected_error),
                "unexpected error: {error}"
            );
            assert!(registry.directory("root", true, false).await.is_empty());
            assert_eq!(registry.reserve("root").await.unwrap().id, AgentId::new(1));
            assert!(receiver.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn close_all_fences_a_batch_before_its_first_insert() {
        let (updates, _receiver) = mpsc::unbounded_channel();
        let registry = Arc::new(Registry::new(updates, 1));
        let reservation = registry.reserve("root").await.unwrap();
        let closed = registry.close_all("root").await.unwrap();
        assert!(closed.is_empty());

        let (agent, events) = pending_agent(Arc::new(Notify::new()));
        let contract = OutputContract::compile(&json!({ "type": "object" })).unwrap();
        let error = registry
            .insert(
                reservation.root_session_id,
                AgentDescriptor {
                    id: reservation.id,
                    session_id: agent.session_id().to_string(),
                    role: "child".to_owned(),
                    task: "work".to_owned(),
                    parent: None,
                },
                None,
                agent,
                forward_events(
                    "root".to_owned(),
                    reservation.id,
                    events,
                    oneshot::channel().1,
                    Arc::downgrade(&registry),
                    registry.updates.clone(),
                ),
                contract,
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("scope stopped"));
    }

    #[tokio::test]
    async fn cancelled_batch_startup_closes_every_tracked_child() {
        let (updates, _receiver) = mpsc::unbounded_channel();
        let registry = Arc::new(Registry::new(updates, 2));
        let mut startup = registry.batch_startup();
        let mut ids = Vec::new();
        for _ in 0..2 {
            let reservation = registry.reserve("root").await.unwrap();
            let (agent, events) = pending_agent(Arc::new(Notify::new()));
            insert_runtime_session(&registry, &reservation, None, agent, events).await;
            startup.track(&reservation.root_session_id, reservation.id);
            ids.push(reservation.id);
        }

        drop(startup);
        let mut revision = registry.revision.subscribe();
        timeout(Duration::from_secs(5), async {
            loop {
                let all_closed = {
                    let state = registry.state.lock().await;
                    ids.iter()
                        .all(|id| state.scopes["root"].sessions[id].status == AgentStatus::Closed)
                };
                if all_closed {
                    break;
                }
                revision.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    async fn insert_runtime_session(
        registry: &Arc<Registry>,
        reservation: &super::AgentReservation,
        parent: Option<AgentId>,
        agent: Nanocodex,
        events: AgentEvents,
    ) -> String {
        let session_id = events.request_id().to_owned();
        let descriptor = AgentDescriptor {
            id: reservation.id,
            session_id: session_id.clone(),
            role: format!("agent-{}", reservation.id),
            task: "wait forever".to_owned(),
            parent,
        };
        let (start_events, events_ready) = oneshot::channel();
        let event_task = forward_events(
            reservation.root_session_id.clone(),
            reservation.id,
            events,
            events_ready,
            Arc::downgrade(registry),
            registry.updates.clone(),
        );
        registry
            .insert(
                reservation.root_session_id.clone(),
                descriptor,
                None,
                agent,
                event_task,
                test_contract(),
            )
            .await
            .unwrap();
        start_events.send(()).unwrap();
        session_id
    }

    async fn insert_pending_runtime_session(
        registry: &Arc<Registry>,
        root_session_id: &str,
        parent: Option<AgentId>,
        called: Arc<Notify>,
    ) -> (AgentId, String) {
        let reservation = registry.reserve(root_session_id).await.unwrap();
        let id = reservation.id;
        let (agent, events) = pending_agent(called);
        let session_id =
            insert_runtime_session(registry, &reservation, parent, agent, events).await;
        (id, session_id)
    }

    async fn next_message_update(
        updates: &mut tokio::sync::mpsc::UnboundedReceiver<super::ScopedAgentUpdate>,
    ) -> crate::AgentMessageUpdate {
        timeout(Duration::from_secs(5), async {
            loop {
                let update = updates
                    .recv()
                    .await
                    .expect("the update channel should remain open");
                if let AgentUpdate::Message(message) = update.update {
                    return message;
                }
            }
        })
        .await
        .expect("a message update should arrive")
    }

    async fn mark_reusable(registry: &Arc<Registry>, root_session_id: &str, id: AgentId) {
        registry
            .state
            .lock()
            .await
            .scopes
            .get_mut(root_session_id)
            .unwrap()
            .sessions
            .get_mut(&id)
            .unwrap()
            .status = AgentStatus::Completed {
            output: json!({ "report": "ready for another turn" }),
        };
    }

    fn test_session(id: AgentId, session_id: &str, parent: Option<AgentId>) -> ChildSession {
        let descriptor = AgentDescriptor {
            id,
            session_id: session_id.to_owned(),
            role: format!("agent-{id}"),
            task: "test lifecycle".to_owned(),
            parent,
        };
        ChildSession {
            descriptor,
            host_context: None,
            event_task: Some(platform::spawn(async {})),
            harness: None,
            harness_task: None,
            status: AgentStatus::Pending,
            active: false,
            output_validator: test_contract().validator,
            output_schema: serde_json::from_str(&test_contract().schema).unwrap(),
            stored_runtime: None,
            next_turn_token: 0,
            active_turn_token: None,
            steering: false,
            submitted_output: None,
            last_output: None,
            last_used: 0,
            evicted: false,
        }
    }

    #[tokio::test]
    async fn submitted_outputs_are_validated_and_completed_as_json() {
        let mut registry = RegistryState::default();
        let reservation = registry.reserve("main", None).unwrap();
        let mut session = test_session(reservation.id, "child-session", None);
        session.active = true;
        session.next_turn_token = 1;
        session.active_turn_token = Some(1);
        session.status = AgentStatus::Running;
        session.output_validator = jsonschema::validator_for(&json!({
            "type": "object",
            "properties": { "answer": { "type": "integer" } },
            "required": ["answer"],
            "additionalProperties": false
        }))
        .unwrap();
        registry
            .insert(
                reservation.root_session_id,
                reservation.id,
                session.descriptor.session_id.clone(),
                session,
            )
            .unwrap();

        let invalid = registry.submit_result("child-session", 1, json!({ "answer": "42" }));
        let invalid = invalid.unwrap_err();
        let diagnostic = invalid
            .get_ref()
            .unwrap()
            .downcast_ref::<super::CompletionError>()
            .unwrap();
        assert_eq!(
            diagnostic.code,
            super::CompletionErrorCode::SchemaValidation
        );
        assert!(diagnostic.recoverable);
        assert_eq!(diagnostic.current_turn_token, Some(1));
        assert!(diagnostic.details[0].contains("/answer"));
        assert!(!diagnostic.details[0].contains("42"));
        assert!(
            diagnostic
                .recovery
                .contains("do not repeat task side effects")
        );
        assert!(
            registry
                .submit_result("child-session", 1, json!("{\"answer\":42}"))
                .unwrap()
        );
        assert!(
            registry
                .submit_result("child-session", 1, json!({ "answer": 43 }))
                .unwrap_err()
                .to_string()
                .contains("already accepted")
        );

        let session = registry
            .scopes
            .get_mut("main")
            .unwrap()
            .sessions
            .get_mut(&reservation.id)
            .unwrap();
        let output = session.submitted_output.take();
        let status = complete_session(session, output);

        assert_eq!(
            status,
            AgentStatus::Completed {
                output: json!({ "answer": 42 })
            }
        );
        assert_eq!(session.last_output, Some(json!({ "answer": 42 })));
    }

    #[test]
    fn encoded_container_results_obey_the_exact_contract_without_changing_valid_strings() {
        let object = jsonschema::validator_for(&json!({
            "type": "object", "properties": { "answer": { "type": "integer" } },
            "required": ["answer"], "additionalProperties": false
        }))
        .unwrap();
        assert_eq!(
            super::validate_submitted_output(&object, json!("{\"answer\":42}")).unwrap(),
            (json!({ "answer": 42 }), true)
        );
        assert_eq!(
            super::validate_submitted_output(&object, json!({ "answer": 42 })).unwrap(),
            (json!({ "answer": 42 }), false)
        );
        for invalid in [
            json!("{\"answer\":\"42\"}"),
            json!("{\"answer\":42,\"extra\":1}"),
            json!("not JSON"),
            json!("42"),
            json!("\"{\\\"answer\\\":42}\""),
            json!(format!("{}{{\"answer\":42}}", " ".repeat(1_048_576))),
        ] {
            let error = super::validate_submitted_output(&object, invalid).unwrap_err();
            assert_eq!(error.code, super::CompletionErrorCode::SchemaValidation);
            assert!(error.recoverable);
            assert!(!error.details.is_empty());
            assert!(!error.to_string().contains("42"));
            assert!(!error.to_string().contains("not JSON"));
        }
        let array = jsonschema::validator_for(&json!({"type":"array", "items":{"type":"integer"}}))
            .unwrap();
        assert_eq!(
            super::validate_submitted_output(&array, json!("[1,2]")).unwrap(),
            (json!([1, 2]), true)
        );
        let string =
            jsonschema::validator_for(&json!({"anyOf":[{"type":"string"},{"type":"object"}]}))
                .unwrap();
        let encoded = json!("{\"answer\":42}");
        assert_eq!(
            super::validate_submitted_output(&string, encoded.clone()).unwrap(),
            (encoded, false)
        );
        let number = jsonschema::validator_for(&json!({"type":"number"})).unwrap();
        assert!(super::validate_submitted_output(&number, json!("42")).is_err());
    }

    #[test]
    fn root_cannot_submit_a_subagent_result() {
        let mut registry = RegistryState::default();

        let error = registry.submit_result("main", 1, json!({ "report": "no" }));

        assert!(
            error
                .unwrap_err()
                .to_string()
                .contains("only available to subagents")
        );
    }

    #[tokio::test]
    async fn successful_turn_without_submission_fails_completion() {
        let mut session = test_session(AgentId::new(1), "child-session", None);

        let status = complete_session(&mut session, None);

        let AgentStatus::Failed { error } = status else {
            panic!("missing result must fail")
        };
        assert!(error.contains("without a valid submit_result call"));
        assert!(error.contains("do not replay task side effects"));
        assert!(
            !error.starts_with('{'),
            "failure cards must not display JSON envelopes"
        );
        assert_eq!(session.last_output, None);
    }

    #[tokio::test]
    async fn submission_from_completed_turn_cannot_satisfy_next_turn() {
        let mut registry = RegistryState::default();
        let reservation = registry.reserve("main", None).unwrap();
        let mut session = test_session(reservation.id, "child-session", None);
        session.active = true;
        session.next_turn_token = 1;
        session.active_turn_token = Some(1);
        session.status = AgentStatus::Running;
        registry
            .insert(
                reservation.root_session_id,
                reservation.id,
                session.descriptor.session_id.clone(),
                session,
            )
            .unwrap();
        let stale_output = json!({ "report": "result from the completed turn" });

        let session = registry
            .scopes
            .get_mut("main")
            .unwrap()
            .sessions
            .get_mut(&reservation.id)
            .unwrap();
        session.active = false;
        session.active = true;
        session.next_turn_token = 2;
        session.active_turn_token = Some(2);
        session.status = AgentStatus::Running;

        assert!(
            registry
                .submit_result("child-session", 1, stale_output)
                .is_err()
        );
    }

    #[tokio::test]
    async fn steering_rotates_the_token_and_stops_after_submission() {
        let mut registry = RegistryState::default();
        let reservation = registry.reserve("main", None).unwrap();
        let mut session = test_session(reservation.id, "child-session", None);
        session.active = true;
        session.next_turn_token = 1;
        session.active_turn_token = Some(1);
        session.status = AgentStatus::Running;
        registry
            .insert(
                reservation.root_session_id,
                reservation.id,
                session.descriptor.session_id.clone(),
                session,
            )
            .unwrap();

        let steer = registry.begin_turn_steer("main", reservation.id).unwrap();
        assert_eq!(steer.token(), 2);
        let error = registry
            .submit_result("child-session", 1, json!({"report": "before steering"}))
            .unwrap_err();
        let diagnostic = error
            .get_ref()
            .unwrap()
            .downcast_ref::<super::CompletionError>()
            .unwrap();
        assert_eq!(
            diagnostic.code,
            super::CompletionErrorCode::SteeringInProgress
        );
        assert_eq!(diagnostic.current_turn_token, None);
        registry.finish_turn_steer("main", steer, true);
        let error = registry
            .submit_result("child-session", 1, json!({ "report": "stale" }))
            .unwrap_err();
        let diagnostic = error
            .get_ref()
            .unwrap()
            .downcast_ref::<super::CompletionError>()
            .unwrap();
        assert_eq!(diagnostic.code, super::CompletionErrorCode::StaleTurnToken);
        assert_eq!(diagnostic.current_turn_token, Some(2));
        assert!(diagnostic.recoverable);
        registry
            .submit_result("child-session", 2, json!({ "report": "current" }))
            .unwrap();

        assert!(registry.begin_turn_steer("main", reservation.id).is_none());
    }

    #[tokio::test]
    async fn accepted_result_survives_cancelled_settlement_without_completing_execution() {
        let (registry, _control, _updates) = super::channel(32);
        let reservation = registry.reserve("main").await.unwrap();
        let id = reservation.id;
        let mut session = test_session(id, "child-session", None);
        session.active = true;
        session.active_turn_token = Some(1);
        session.next_turn_token = 1;
        session.status = AgentStatus::Running;
        registry
            .state
            .lock()
            .await
            .insert("main".into(), id, "child-session".into(), session)
            .unwrap();
        registry
            .submit_result("child-session", 1, json!({"report": "accepted"}))
            .await
            .unwrap();
        registry
            .harness_turn_finished(
                "main",
                id,
                Err(nanocodex_agent::NanocodexError::TurnCancelled),
            )
            .await;
        {
            let state = registry.state.lock().await;
            let session = &state.scopes["main"].sessions[&id];
            assert_eq!(session.status, AgentStatus::Interrupted);
            assert_eq!(
                session.summary().last_output,
                Some(json!({"report": "accepted"}))
            );
            assert_eq!(session.active_turn_token, None);
            assert_eq!(session.submitted_output, None);
        }
        assert_eq!(registry.harness_turn_started("main", id).await, Some(2));
        assert!(
            registry
                .submit_result("child-session", 1, json!({"report": "old"}))
                .await
                .is_err()
        );
        assert!(
            registry.state.lock().await.scopes["main"].sessions[&id]
                .submitted_output
                .is_none()
        );
    }

    #[tokio::test]
    async fn closed_agent_summaries_keep_the_last_completed_output() {
        let (registry, _control, _updates) = super::channel(32);
        let reservation = registry.reserve("main").await.unwrap();
        let mut session = test_session(reservation.id, "child-session", None);
        session.status = AgentStatus::Completed {
            output: json!({ "report": "completed work" }),
        };
        session.last_output = Some(json!({ "report": "completed work" }));
        registry
            .state
            .lock()
            .await
            .insert(
                reservation.root_session_id.clone(),
                reservation.id,
                session.descriptor.session_id.clone(),
                session,
            )
            .unwrap();

        let summaries = registry.close("main", reservation.id).await.unwrap();

        assert_eq!(summaries[0].status, AgentStatus::Closed);
        assert_eq!(
            summaries[0].last_output,
            Some(json!({ "report": "completed work" }))
        );
    }

    #[tokio::test]
    async fn pending_mailbox_work_protects_an_inactive_resident_from_eviction() {
        let (registry, _control, mut updates) = super::channel(0);
        registry.set_max_resident(1);
        let protected_called = Arc::new(Notify::new());
        let (protected, _protected_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::clone(&protected_called))
                .await;
        let (idle, _idle_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::new(Notify::new())).await;
        mark_reusable(&registry, "main", protected).await;
        mark_reusable(&registry, "main", idle).await;

        let receipt = registry
            .send_message(
                "main",
                protected,
                MessagePriority::Deferred,
                MessagePurpose::Coordinate,
                None,
                "Preserve this queued follow-up across residency enforcement.".to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.disposition, MessageDisposition::Queued);
        assert_eq!(
            next_message_update(&mut updates).await.delivery,
            MessageDeliveryState::Admitted {
                disposition: MessageDisposition::Queued,
            }
        );

        registry.enforce_resident_limit("main").await;
        {
            let state = registry.state.lock().await;
            let sessions = &state.scopes["main"].sessions;
            assert!(sessions[&protected].harness.is_some());
            assert_eq!(
                sessions[&protected].status,
                AgentStatus::Completed {
                    output: json!({ "report": "ready for another turn" }),
                }
            );
            assert!(sessions[&idle].harness.is_none());
            assert!(sessions[&idle].evicted);
        }

        registry.set_max_concurrency(1);
        timeout(Duration::from_secs(5), protected_called.notified())
            .await
            .expect("the protected queued message should start");
        registry.close_all("main").await.unwrap();
    }

    #[tokio::test]
    async fn eviction_preserves_interrupted_status_and_lifecycle_addressability() {
        let (registry, _control, _updates) = super::channel(1);
        registry.set_max_resident(1);
        let (interrupted, _interrupted_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::new(Notify::new())).await;
        let (newer, _newer_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::new(Notify::new())).await;
        {
            let mut state = registry.state.lock().await;
            let sessions = &mut state
                .scopes
                .get_mut("main")
                .expect("main scope should exist")
                .sessions;
            sessions
                .get_mut(&interrupted)
                .expect("interrupted session should exist")
                .status = AgentStatus::Interrupted;
            sessions
                .get_mut(&newer)
                .expect("newer session should exist")
                .status = AgentStatus::Completed {
                output: json!({ "report": "newer" }),
            };
        }

        registry.enforce_resident_limit("main").await;

        let entry = registry
            .directory("main", true, false)
            .await
            .into_iter()
            .find(|entry| entry.agent_id == interrupted)
            .expect("evicted agent should remain in the directory");
        assert_eq!(entry.status, AgentStatus::Interrupted);
        assert!(entry.can_message);
        assert!(entry.can_manage);
        let (summaries, timed_out) = registry
            .wait("main", &[interrupted], Duration::from_millis(1))
            .await
            .unwrap();
        assert!(!timed_out);
        assert_eq!(summaries[0].status, AgentStatus::Interrupted);
        assert_eq!(
            registry.close("main", interrupted).await.unwrap()[0].status,
            AgentStatus::Closed
        );
        registry.close_all("main").await.unwrap();
    }

    #[tokio::test]
    async fn active_turns_do_not_consume_the_inactive_residency_budget() {
        let (registry, _control, _updates) = super::channel(2);
        registry.set_max_resident(1);
        let first_called = Arc::new(Notify::new());
        let second_called = Arc::new(Notify::new());
        let (first, _) =
            insert_pending_runtime_session(&registry, "main", None, Arc::clone(&first_called))
                .await;
        let (second, _) =
            insert_pending_runtime_session(&registry, "main", None, Arc::clone(&second_called))
                .await;
        for id in [first, second] {
            registry
                .launch_initial_turn(
                    "main",
                    id,
                    format!("active turn for {id}"),
                    registry.reserve_turn().unwrap(),
                )
                .await
                .unwrap();
        }
        timeout(Duration::from_secs(5), first_called.notified())
            .await
            .unwrap();
        timeout(Duration::from_secs(5), second_called.notified())
            .await
            .unwrap();

        registry.enforce_resident_limit("main").await;
        let state = registry.state.lock().await;
        assert!(state.scopes["main"].sessions[&first].harness.is_some());
        assert!(state.scopes["main"].sessions[&second].harness.is_some());
        drop(state);
        registry.close_all("main").await.unwrap();
    }

    #[tokio::test]
    async fn interrupt_and_close_stop_recursive_turns_and_preserve_continuation() {
        let (registry, _control, _updates) = super::channel(32);
        let parent_called = Arc::new(Notify::new());
        let child_called = Arc::new(Notify::new());
        let sibling_called = Arc::new(Notify::new());

        let parent = registry.reserve("main").await.unwrap();
        let (parent_agent, parent_events) = pending_agent(Arc::clone(&parent_called));
        let parent_session =
            insert_runtime_session(&registry, &parent, None, parent_agent, parent_events).await;
        registry
            .launch_initial_turn(
                &parent.root_session_id,
                parent.id,
                "parent work".to_owned(),
                registry.reserve_turn().unwrap(),
            )
            .await
            .unwrap();

        let child = registry.reserve(&parent_session).await.unwrap();
        let (child_agent, child_events) = pending_agent(Arc::clone(&child_called));
        insert_runtime_session(
            &registry,
            &child,
            Some(parent.id),
            child_agent,
            child_events,
        )
        .await;
        registry
            .launch_initial_turn(
                &child.root_session_id,
                child.id,
                "child work".to_owned(),
                registry.reserve_turn().unwrap(),
            )
            .await
            .unwrap();

        let sibling = registry.reserve("main").await.unwrap();
        let (sibling_agent, sibling_events) = pending_agent(Arc::clone(&sibling_called));
        insert_runtime_session(&registry, &sibling, None, sibling_agent, sibling_events).await;
        registry
            .launch_initial_turn(
                &sibling.root_session_id,
                sibling.id,
                "sibling work".to_owned(),
                registry.reserve_turn().unwrap(),
            )
            .await
            .unwrap();

        timeout(Duration::from_secs(5), parent_called.notified())
            .await
            .unwrap();
        timeout(Duration::from_secs(5), child_called.notified())
            .await
            .unwrap();
        timeout(Duration::from_secs(5), sibling_called.notified())
            .await
            .unwrap();

        let (running, timed_out) = registry
            .wait("main", &[parent.id, child.id], Duration::from_millis(1))
            .await
            .unwrap();
        assert!(timed_out);
        assert!(
            running
                .iter()
                .all(|summary| summary.status == AgentStatus::Running)
        );

        let interrupted = registry.interrupt("main", parent.id).await.unwrap();
        assert_eq!(
            interrupted
                .iter()
                .map(|summary| (&summary.agent_id, &summary.status))
                .collect::<Vec<_>>(),
            [
                (&child.id, &AgentStatus::Interrupted),
                (&parent.id, &AgentStatus::Interrupted),
            ]
        );
        let (finished, timed_out) = registry
            .wait("main", &[parent.id, child.id], Duration::from_secs(1))
            .await
            .unwrap();
        assert!(!timed_out);
        assert_eq!(finished.len(), 2);
        assert_eq!(
            registry
                .state
                .lock()
                .await
                .summaries("main", &[sibling.id])
                .unwrap()[0]
                .status,
            AgentStatus::Running
        );

        let receipt = registry
            .send_message(
                "main",
                parent.id,
                MessagePriority::Deferred,
                MessagePurpose::Delegate,
                None,
                "continue".to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.disposition, MessageDisposition::Started);
        timeout(Duration::from_secs(5), parent_called.notified())
            .await
            .unwrap();

        let closed = registry.close("main", parent.id).await.unwrap();
        assert_eq!(
            closed
                .iter()
                .map(|summary| (&summary.agent_id, &summary.status))
                .collect::<Vec<_>>(),
            [
                (&child.id, &AgentStatus::Closed),
                (&parent.id, &AgentStatus::Closed),
            ]
        );
        assert_eq!(registry.directory("main", true, false).await.len(), 3);

        let all_closed = registry.close_all("main").await.unwrap();
        assert_eq!(all_closed.len(), 3);
        assert!(
            all_closed
                .iter()
                .all(|summary| summary.status == AgentStatus::Closed)
        );
        let state = registry.state.lock().await;
        assert!(
            state.scopes["main"]
                .sessions
                .values()
                .all(|session| session.harness.is_none()
                    && session.harness_task.is_none()
                    && session.event_task.is_none())
        );
    }

    #[tokio::test]
    async fn same_root_agents_can_message_across_sibling_branches() {
        let (registry, _control, mut updates) = super::channel(32);
        let sender_called = Arc::new(Notify::new());
        let target_called = Arc::new(Notify::new());
        let (_sender, sender_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::clone(&sender_called))
                .await;
        let (target, _target_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::clone(&target_called))
                .await;
        mark_reusable(&registry, "main", target).await;

        let receipt = registry
            .send_message(
                &sender_session,
                target,
                MessagePriority::Deferred,
                MessagePurpose::Coordinate,
                None,
                "Compare our findings before either of us edits.".to_owned(),
            )
            .await
            .unwrap();

        assert_eq!(receipt.disposition, MessageDisposition::Started);
        timeout(Duration::from_secs(5), target_called.notified())
            .await
            .unwrap();
        let update = next_message_update(&mut updates).await;
        assert_eq!(update.message_id, receipt.message_id);
        assert_eq!(update.thread.messages.len(), 1);
        assert_eq!(
            update.delivery,
            MessageDeliveryState::Admitted {
                disposition: MessageDisposition::Started,
            }
        );

        registry.close_all("main").await.unwrap();
    }

    #[tokio::test]
    async fn pending_agents_cannot_receive_messages_before_their_initial_turn() {
        let (registry, _control, _updates) = super::channel(32);
        let (_sender, sender_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::new(Notify::new())).await;
        let (target, _target_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::new(Notify::new())).await;

        let error = registry
            .send_message(
                &sender_session,
                target,
                MessagePriority::Deferred,
                MessagePurpose::Coordinate,
                None,
                "Do not overtake the assigned initial task.".to_owned(),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("has not started"));
        registry.close_all("main").await.unwrap();
    }

    #[tokio::test]
    async fn sibling_messages_cannot_take_management_authority() {
        let (registry, _control, _updates) = super::channel(32);
        let (_sender, sender_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::new(Notify::new())).await;
        let (target, _target_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::new(Notify::new())).await;

        let error = registry
            .send_message(
                &sender_session,
                target,
                MessagePriority::Deferred,
                MessagePurpose::Delegate,
                None,
                "Replace the sibling's assigned task.".to_owned(),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("only manage its descendants"));
        registry.close_all("main").await.unwrap();
    }

    #[tokio::test]
    async fn delegate_messages_replace_assigned_tasks_for_descendants() {
        let (registry, _control, _updates) = super::channel(32);
        let (parent, parent_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::new(Notify::new())).await;
        let child_called = Arc::new(Notify::new());
        let (child, _child_session) = insert_pending_runtime_session(
            &registry,
            "main",
            Some(parent),
            Arc::clone(&child_called),
        )
        .await;
        mark_reusable(&registry, "main", child).await;

        let receipt = registry
            .send_message(
                &parent_session,
                child,
                MessagePriority::Deferred,
                MessagePurpose::Delegate,
                None,
                "Own the parser tests and report every uncovered branch.".to_owned(),
            )
            .await
            .unwrap();

        assert_eq!(receipt.disposition, MessageDisposition::Started);
        timeout(Duration::from_secs(5), child_called.notified())
            .await
            .unwrap();
        let task = registry.state.lock().await.scopes["main"].sessions[&child]
            .descriptor
            .task
            .clone();
        assert_eq!(
            task,
            "Own the parser tests and report every uncovered branch."
        );
        registry.close_all("main").await.unwrap();
    }

    #[tokio::test]
    async fn urgent_messages_steer_running_agents() {
        let (registry, _control, _updates) = super::channel(32);
        let (_sender, sender_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::new(Notify::new())).await;
        let target_called = Arc::new(Notify::new());
        let (target, _target_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::clone(&target_called))
                .await;
        registry
            .launch_initial_turn(
                "main",
                target,
                "Keep working until interrupted.".to_owned(),
                registry.reserve_turn().unwrap(),
            )
            .await
            .unwrap();
        timeout(Duration::from_secs(5), target_called.notified())
            .await
            .unwrap();

        let receipt = registry
            .send_message(
                &sender_session,
                target,
                MessagePriority::Urgent,
                MessagePurpose::Finding,
                None,
                "Stop duplicating the parser investigation.".to_owned(),
            )
            .await
            .unwrap();

        assert_eq!(receipt.disposition, MessageDisposition::Steered);
        registry.close_all("main").await.unwrap();
    }

    #[tokio::test]
    async fn interruption_marks_queued_messages_as_failed() {
        let (registry, _control, mut updates) = super::channel(32);
        let (_sender, sender_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::new(Notify::new())).await;
        let target_called = Arc::new(Notify::new());
        let (target, _target_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::clone(&target_called))
                .await;
        registry
            .launch_initial_turn(
                "main",
                target,
                "Keep working until interrupted.".to_owned(),
                registry.reserve_turn().unwrap(),
            )
            .await
            .unwrap();
        timeout(Duration::from_secs(5), target_called.notified())
            .await
            .unwrap();

        let receipt = registry
            .send_message(
                &sender_session,
                target,
                MessagePriority::Deferred,
                MessagePurpose::Question,
                None,
                "What remains in your investigation?".to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.disposition, MessageDisposition::Queued);
        let admitted = next_message_update(&mut updates).await;
        assert_eq!(admitted.message_id, receipt.message_id);

        registry.interrupt("main", target).await.unwrap();

        let failed = next_message_update(&mut updates).await;
        assert_eq!(failed.message_id, receipt.message_id);
        assert!(matches!(
            failed.delivery,
            MessageDeliveryState::Failed { .. }
        ));
        registry.close_all("main").await.unwrap();
    }

    #[tokio::test]
    async fn queued_delegation_changes_the_task_only_when_delivery_starts() {
        let (registry, _control, _updates) = super::channel(32);
        let target_called = Arc::new(Notify::new());
        let (target, _target_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::clone(&target_called))
                .await;
        registry
            .launch_initial_turn(
                "main",
                target,
                "Keep working until interrupted.".to_owned(),
                registry.reserve_turn().unwrap(),
            )
            .await
            .unwrap();
        timeout(Duration::from_secs(5), target_called.notified())
            .await
            .unwrap();

        let receipt = registry
            .send_message(
                "main",
                target,
                MessagePriority::Deferred,
                MessagePurpose::Delegate,
                None,
                "This task must not become current before delivery.".to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.disposition, MessageDisposition::Queued);
        let task = registry.state.lock().await.scopes["main"].sessions[&target]
            .descriptor
            .task
            .clone();
        assert_eq!(task, "wait forever");

        registry.interrupt("main", target).await.unwrap();
        let task = registry.state.lock().await.scopes["main"].sessions[&target]
            .descriptor
            .task
            .clone();
        assert_eq!(task, "wait forever");
        registry.close_all("main").await.unwrap();
    }

    #[tokio::test]
    async fn message_priorities_have_independent_mailbox_bounds() {
        let (registry, _control, _updates) = super::channel(0);
        let (_sender, sender_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::new(Notify::new())).await;
        let (target, _target_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::new(Notify::new())).await;
        mark_reusable(&registry, "main", target).await;

        for index in 0..crate::harness::DEFERRED_CAPACITY {
            let receipt = registry
                .send_message(
                    &sender_session,
                    target,
                    MessagePriority::Deferred,
                    MessagePurpose::Coordinate,
                    None,
                    format!("queued message {index}"),
                )
                .await
                .unwrap();
            assert_eq!(receipt.disposition, MessageDisposition::Queued);
        }
        let normal_error = registry
            .send_message(
                &sender_session,
                target,
                MessagePriority::Deferred,
                MessagePurpose::Coordinate,
                None,
                "one message too many".to_owned(),
            )
            .await
            .unwrap_err();
        assert!(normal_error.to_string().contains("mailbox"));

        for index in 0..crate::harness::URGENT_CAPACITY {
            let receipt = registry
                .send_message(
                    &sender_session,
                    target,
                    MessagePriority::Urgent,
                    MessagePurpose::Coordinate,
                    None,
                    format!("urgent queued message {index}"),
                )
                .await
                .unwrap();
            assert_eq!(receipt.disposition, MessageDisposition::Queued);
        }
        let urgent_error = registry
            .send_message(
                &sender_session,
                target,
                MessagePriority::Urgent,
                MessagePurpose::Coordinate,
                None,
                "one urgent message too many".to_owned(),
            )
            .await
            .unwrap_err();
        assert!(urgent_error.to_string().contains("mailbox"));
        registry.close_all("main").await.unwrap();
    }

    #[tokio::test]
    async fn messages_do_not_cross_root_scopes() {
        let (registry, _control, _updates) = super::channel(32);
        let (target, _target_session) =
            insert_pending_runtime_session(&registry, "main", None, Arc::new(Notify::new())).await;

        let error = registry
            .send_message(
                "other-root",
                target,
                MessagePriority::Deferred,
                MessagePurpose::Coordinate,
                None,
                "This must not reach the main tree.".to_owned(),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("unknown agent_id"));
        registry.close_all("main").await.unwrap();
    }

    fn insert_session(
        registry: &mut RegistryState,
        root_session_id: &str,
        id: AgentId,
        session_id: &str,
        parent: Option<AgentId>,
    ) {
        let session = test_session(id, session_id, parent);
        registry
            .insert(
                root_session_id.to_owned(),
                id,
                session.descriptor.session_id.clone(),
                session,
            )
            .unwrap();
    }

    #[test]
    fn root_sessions_number_subagents_independently() {
        let mut registry = RegistryState::default();

        let main = registry.reserve("main", None).unwrap();
        let fork = registry.reserve("fork", None).unwrap();

        assert_eq!(main.id, AgentId::new(1));
        assert_eq!(main.root_session_id, "main");
        assert_eq!(fork.id, AgentId::new(1));
        assert_eq!(fork.root_session_id, "fork");
    }

    #[test]
    fn descendant_sessions_use_their_root_namespace() {
        let mut registry = RegistryState::default();
        let root = registry.reserve("main", None).unwrap();
        registry
            .root_by_session
            .insert("child".to_owned(), root.root_session_id);

        let descendant = registry.reserve("child", None).unwrap();

        assert_eq!(descendant.id, AgentId::new(2));
        assert_eq!(descendant.root_session_id, "main");
    }

    #[tokio::test]
    async fn child_sessions_automatically_own_new_subagents() {
        let mut registry = RegistryState::default();
        let parent = registry.reserve("main", None).unwrap();
        insert_session(
            &mut registry,
            &parent.root_session_id,
            parent.id,
            "parent-session",
            None,
        );

        let child = registry.reserve_for("parent-session").unwrap();

        assert_eq!(child.root_session_id, "main");
        assert_eq!(child.parent, Some(parent.id));
    }

    #[tokio::test]
    async fn subagents_can_manage_descendants_but_not_siblings_or_ancestors() {
        let mut registry = RegistryState::default();
        let first = registry.reserve("main", None).unwrap();
        insert_session(
            &mut registry,
            &first.root_session_id,
            first.id,
            "first-session",
            None,
        );
        let second = registry.reserve("main", None).unwrap();
        insert_session(
            &mut registry,
            &second.root_session_id,
            second.id,
            "second-session",
            None,
        );
        let child = registry.reserve_for("first-session").unwrap();
        insert_session(
            &mut registry,
            &child.root_session_id,
            child.id,
            "child-session",
            Some(first.id),
        );

        assert!(registry.summaries("first-session", &[child.id]).is_ok());
        assert!(registry.summaries("first-session", &[second.id]).is_err());
        assert!(registry.summaries("second-session", &[child.id]).is_err());
        assert!(registry.summaries("child-session", &[first.id]).is_err());
        assert_eq!(registry.summaries("main", &[child.id]).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn directory_keeps_nonresident_agents_manageable_but_not_messageable() {
        let mut registry = RegistryState::default();
        let parent = registry.reserve("main", None).unwrap();
        insert_session(
            &mut registry,
            &parent.root_session_id,
            parent.id,
            "parent-session",
            None,
        );
        let child = registry.reserve("main", Some(parent.id)).unwrap();
        insert_session(
            &mut registry,
            &child.root_session_id,
            child.id,
            "child-session",
            Some(parent.id),
        );
        let sibling = registry.reserve("main", None).unwrap();
        insert_session(
            &mut registry,
            &sibling.root_session_id,
            sibling.id,
            "sibling-session",
            None,
        );

        for session in registry
            .scopes
            .get_mut("main")
            .unwrap()
            .sessions
            .values_mut()
        {
            session.status = AgentStatus::Completed {
                output: json!({ "report": "ready" }),
            };
        }

        let error = registry
            .prepare_message(
                "parent-session",
                child.id,
                MessagePriority::Deferred,
                MessagePurpose::Coordinate,
                None,
                "continue".to_owned(),
            )
            .err()
            .expect("nonresident agents cannot receive messages");
        assert!(error.to_string().contains("list_agents"));
        assert!(error.to_string().contains("can_message=true"));
        assert!(error.to_string().contains("spawn a replacement"));

        let directory = registry.directory("parent-session", true, false);

        assert_eq!(
            directory
                .iter()
                .map(|entry| (entry.agent_id, entry.can_message, entry.can_manage))
                .collect::<Vec<_>>(),
            [(child.id, false, true), (sibling.id, false, false)]
        );
    }

    #[tokio::test]
    async fn child_spawn_is_rejected_when_parent_closes_after_reservation() {
        let mut registry = RegistryState::default();
        let parent = registry.reserve("main", None).unwrap();
        insert_session(
            &mut registry,
            &parent.root_session_id,
            parent.id,
            "parent-session",
            None,
        );
        let child = registry.reserve_for("parent-session").unwrap();
        registry
            .scopes
            .get_mut("main")
            .unwrap()
            .sessions
            .get_mut(&parent.id)
            .unwrap()
            .status = AgentStatus::Closed;
        let session = test_session(child.id, "child-session", Some(parent.id));

        let result = registry.insert(
            child.root_session_id,
            child.id,
            session.descriptor.session_id.clone(),
            session,
        );

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn subtree_shutdown_order_includes_every_descendant_before_its_parent() {
        let mut registry = RegistryState::default();
        let parent = registry.reserve("main", None).unwrap();
        insert_session(
            &mut registry,
            &parent.root_session_id,
            parent.id,
            "parent-session",
            None,
        );
        let child = registry.reserve("parent-session", Some(parent.id)).unwrap();
        insert_session(
            &mut registry,
            &child.root_session_id,
            child.id,
            "child-session",
            Some(parent.id),
        );
        let grandchild = registry.reserve("child-session", Some(child.id)).unwrap();
        insert_session(
            &mut registry,
            &grandchild.root_session_id,
            grandchild.id,
            "grandchild-session",
            Some(child.id),
        );

        assert_eq!(
            registry.subtree_shutdown_order("main", parent.id).unwrap(),
            [grandchild.id, child.id, parent.id]
        );
    }

    #[tokio::test]
    async fn root_sessions_cannot_access_each_others_subagents() {
        let mut registry = RegistryState::default();
        let main = registry.reserve("main", None).unwrap();
        let session = test_session(main.id, "main-child", None);
        registry
            .insert(
                main.root_session_id,
                main.id,
                session.descriptor.session_id.clone(),
                session,
            )
            .unwrap();

        assert!(registry.summaries("fork", &[main.id]).is_err());
        assert!(registry.reserve("fork", Some(main.id)).is_err());
    }

    fn checkpoint_fixture() -> super::SubagentCheckpoint {
        super::SubagentCheckpoint {
            version: 1,
            root_session_id: "root".to_owned(),
            next_agent_id: 9,
            children: vec![super::ChildCheckpoint {
                descriptor: AgentDescriptor {
                    id: AgentId::new(7),
                    session_id: "018f1f9a-7b3c-7a17-8000-000000000107".to_owned(),
                    role: "reviewer".to_owned(),
                    task: "review persisted history".to_owned(),
                    parent: None,
                },
                runtime: Some(nanocodex_agent::ChildRuntimeSnapshot {
                    session_id: "018f1f9a-7b3c-7a17-8000-000000000107".to_owned(),
                    model: nanocodex_agent::Model::Sol,
                    thinking: nanocodex_agent::Thinking::High,
                    fast_mode: false,
                    conversation: None,
                }),
                output_schema: json!({"type":"object", "properties":{"answer":{"type":"integer"}},
                    "required":["answer"], "additionalProperties":false}),
                next_turn_token: 3,
                status: AgentStatus::Completed {
                    output: json!({"answer":42}),
                },
                last_output: Some(json!({"answer":42})),
                host_context: Some("opaque-test-context".to_owned()),
            }],
        }
    }

    #[test]
    fn checkpoint_rejects_invalid_version_identity_topology_contract_and_counters() {
        let valid = checkpoint_fixture();
        valid.validate("root").unwrap();
        let encoded = serde_json::to_string(&valid).unwrap();
        let roundtrip: super::SubagentCheckpoint = serde_json::from_str(&encoded).unwrap();
        roundtrip.validate("root").unwrap();
        assert!(valid.validate("other-root").is_err());
        let mut bad = valid.clone();
        bad.version = 2;
        assert!(bad.validate("root").is_err());
        let mut bad = valid.clone();
        bad.children[0].runtime.as_mut().unwrap().session_id = "different".to_owned();
        assert!(bad.validate("root").is_err());
        let mut bad = valid.clone();
        bad.children[0].descriptor.parent = Some(AgentId::new(7));
        assert!(bad.validate("root").is_err());
        let mut bad = valid.clone();
        bad.children.push(bad.children[0].clone());
        assert!(bad.validate("root").is_err());
        let mut bad = valid.clone();
        bad.children[0].runtime = None;
        assert!(bad.validate("root").is_err());
        let mut bad = valid.clone();
        bad.children[0].last_output = Some(json!({"answer":"invalid"}));
        assert!(bad.validate("root").is_err());
        let mut bad = valid.clone();
        bad.children[0].status = AgentStatus::Completed {
            output: json!({"answer":42,"extra":true}),
        };
        assert!(bad.validate("root").is_err());
        let mut bad = valid.clone();
        bad.children[0].next_turn_token = u64::MAX;
        assert!(bad.validate("root").is_err());
        let mut bad = valid.clone();
        bad.next_agent_id = 7;
        assert!(bad.validate("root").is_err());
        let mut bad = valid.clone();
        bad.next_agent_id = u64::MAX;
        assert!(bad.validate("root").is_err());
        let mut bad = valid;
        bad.children = vec![bad.children[0].clone(); super::MAX_CHECKPOINT_CHILDREN + 1];
        assert!(
            bad.validate("root")
                .unwrap_err()
                .to_string()
                .contains("limits")
        );
    }

    #[test]
    fn checkpoint_archives_only_interrupted_children_and_rejects_reusable_descendants() {
        let mut checkpoint = checkpoint_fixture();
        checkpoint.children[0].runtime = None;
        checkpoint.children[0].status = AgentStatus::Interrupted;
        checkpoint.validate("root").unwrap();
        for status in [
            AgentStatus::Pending,
            AgentStatus::Running,
            AgentStatus::Closing,
        ] {
            checkpoint.children[0].status = status;
            assert!(checkpoint.validate("root").is_err());
        }
        checkpoint.children[0].status = AgentStatus::Interrupted;
        let mut descendant = checkpoint_fixture().children.remove(0);
        descendant.descriptor.id = AgentId::new(8);
        descendant.descriptor.parent = Some(AgentId::new(7));
        descendant.descriptor.session_id = "018f1f9a-7b3c-7a17-8000-000000000108".to_owned();
        descendant.runtime.as_mut().unwrap().session_id = descendant.descriptor.session_id.clone();
        checkpoint.children.push(descendant);
        assert!(
            checkpoint
                .validate("root")
                .unwrap_err()
                .to_string()
                .contains("parent without a runtime")
        );
        checkpoint.children[1].runtime = None;
        checkpoint.children[1].status = AgentStatus::Interrupted;
        checkpoint.validate("root").unwrap();
    }

    #[tokio::test]
    async fn checkpoint_roundtrips_legacy_archives_without_making_them_messageable() {
        let (root, _events) = pending_agent(Arc::new(Notify::new()));
        let root_id = root.session_id();
        let (original, _, _updates) = super::channel(2);
        let parent = checkpoint_fixture().children.remove(0).descriptor;
        let mut child = parent.clone();
        child.id = AgentId::new(8);
        child.parent = Some(parent.id);
        child.session_id = "018f1f9a-7b3c-7a17-8000-000000000108".to_owned();
        original
            .restore_with_host_contexts(
                root_id,
                vec![child.clone(), parent.clone()],
                HashMap::from([
                    (parent.session_id.clone(), Some(Arc::from("parent-context"))),
                    (child.session_id.clone(), Some(Arc::from("child-context"))),
                ]),
            )
            .await
            .unwrap();
        let checkpoint = original.checkpoint(root_id).await.unwrap();
        assert!(
            checkpoint
                .children
                .iter()
                .all(|child| child.runtime.is_none() && child.status == AgentStatus::Interrupted)
        );
        let encoded = serde_json::to_string(&checkpoint).unwrap();
        let (restored, _, _updates) = super::channel(2);
        restored
            .restore_checkpoint(&root, serde_json::from_str(&encoded).unwrap())
            .await
            .unwrap();
        let directory = restored.directory(root_id, true, false).await;
        assert_eq!(directory.len(), 2);
        assert!(directory.iter().all(|child| !child.can_message));
        {
            let state = restored.state.lock().await;
            assert!(
                state.scopes[root_id]
                    .sessions
                    .values()
                    .all(|session| session.evicted
                        && session.harness.is_none()
                        && session.stored_runtime.is_none())
            );
        }
        assert!(
            restored
                .send_message(
                    root_id,
                    child.id,
                    MessagePriority::Deferred,
                    MessagePurpose::Coordinate,
                    None,
                    "Continue".to_owned()
                )
                .await
                .is_err()
        );
        assert_eq!(
            serde_json::to_value(restored.checkpoint(root_id).await.unwrap()).unwrap(),
            serde_json::to_value(checkpoint).unwrap()
        );
        root.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn checkpoint_preserves_empty_scope_id_high_watermark_and_rejects_duplicate_restore() {
        let (root, _events) = pending_agent(Arc::new(Notify::new()));
        let (registry, _, _updates) = super::channel(1);
        let empty = registry.checkpoint(root.session_id()).await.unwrap();
        assert!(empty.children.is_empty());
        assert_eq!(empty.next_agent_id, 1);
        let mut empty = empty;
        empty.next_agent_id = 13;
        registry
            .restore_checkpoint(&root, empty.clone())
            .await
            .unwrap();
        assert_eq!(
            registry.reserve(root.session_id()).await.unwrap().id,
            AgentId::new(13)
        );
        assert_eq!(
            registry
                .checkpoint(root.session_id())
                .await
                .unwrap()
                .next_agent_id,
            14
        );
        assert!(registry.restore_checkpoint(&root, empty).await.is_err());
        root.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn messaging_rehydrates_evicted_ancestors_once_and_keeps_live_work_running() {
        let (registry, _, _updates) = super::channel(4);
        let registrations = Arc::new(std::sync::Mutex::new(HashMap::<String, usize>::new()));
        let factory_registry = registry.clone();
        let counts = registrations.clone();
        let called = Arc::new(Notify::new());
        let service_called = called.clone();
        let openai = OpenAi::builder("test-key")
            .service(move || PendingService {
                called: service_called.clone(),
            })
            .build()
            .unwrap();
        let (root, _events) = Nanocodex::builder(openai)
            .tools_factory(move |handle| {
                *counts
                    .lock()
                    .unwrap()
                    .entry(handle.session_id().to_owned())
                    .or_default() += 1;
                factory_registry.register_handle(handle);
                nanocodex_tools::Tools::builder().without_defaults().build()
            })
            .build()
            .unwrap();
        let root_id = root.session_id();
        let reservation = registry.reserve(root_id).await.unwrap();
        let (parent, events) = root.spawn().await.unwrap();
        let (child, child_events) = parent.spawn().await.unwrap();
        child
            .append_developer_message("retain amber history across eviction")
            .await
            .unwrap();
        let parent_session =
            insert_runtime_session(&registry, &reservation, None, parent, events).await;
        mark_reusable(&registry, root_id, reservation.id).await;
        let child_reservation = registry.reserve(&parent_session).await.unwrap();
        let child_session = insert_runtime_session(
            &registry,
            &child_reservation,
            Some(reservation.id),
            child,
            child_events,
        )
        .await;
        mark_reusable(&registry, root_id, child_reservation.id).await;
        let sibling = registry.reserve(root_id).await.unwrap();
        let (agent, events) = root.spawn().await.unwrap();
        let sibling_session =
            insert_runtime_session(&registry, &sibling, None, agent, events).await;
        mark_reusable(&registry, root_id, sibling.id).await;
        registry.set_max_resident(1);
        registry.enforce_resident_limit(root_id).await;
        {
            let state = registry.state.lock().await;
            assert!(
                state.scopes[root_id].sessions[&reservation.id]
                    .harness
                    .is_none()
            );
            assert!(
                state.scopes[root_id].sessions[&child_reservation.id]
                    .harness
                    .is_none()
            );
        }
        assert!(
            registry
                .send_message(
                    &sibling_session,
                    child_reservation.id,
                    MessagePriority::Deferred,
                    MessagePurpose::Delegate,
                    None,
                    "unauthorized".into()
                )
                .await
                .is_err()
        );
        assert_eq!(registrations.lock().unwrap()[&child_session], 1);
        let send = || {
            registry.send_message(
                root_id,
                child_reservation.id,
                MessagePriority::Deferred,
                MessagePurpose::Coordinate,
                None,
                "continue with retained history".into(),
            )
        };
        let (first, second) = tokio::join!(send(), send());
        first.unwrap();
        second.unwrap();
        assert_eq!(registrations.lock().unwrap()[&parent_session], 2);
        assert_eq!(registrations.lock().unwrap()[&child_session], 2);
        timeout(Duration::from_secs(2), called.notified())
            .await
            .unwrap();
        let checkpoint = registry.live_checkpoint(root_id).await.unwrap();
        assert!(
            serde_json::to_string(&checkpoint)
                .unwrap()
                .contains("retain amber history")
        );
        assert!(
            registry.state.lock().await.scopes[root_id].sessions[&child_reservation.id].active,
            "live persistence must not interrupt running children or discard their mailbox"
        );
        registry.close(root_id, reservation.id).await.unwrap();
        assert!(
            send().await.is_err(),
            "explicitly closed children cannot be revived"
        );
        assert_eq!(registrations.lock().unwrap()[&child_session], 2);
        registry.close_all(root_id).await.unwrap();
        root.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn checkpoint_restores_evicted_nested_runtime_history_schema_tokens_and_host_context() {
        let called = Arc::new(Notify::new());
        let (root, _events) = pending_agent(Arc::clone(&called));
        let root_id = root.session_id();
        let (original, _, _updates) = super::channel(2);
        let parent_reservation = original.reserve(root_id).await.unwrap();
        let (parent, parent_events) = pending_agent(Arc::new(Notify::new()));
        parent
            .append_developer_message("Preserved parent marker cobalt.")
            .await
            .unwrap();
        let parent_session =
            insert_runtime_session(&original, &parent_reservation, None, parent, parent_events)
                .await;
        mark_reusable(&original, root_id, parent_reservation.id).await;
        let child_reservation = original.reserve(&parent_session).await.unwrap();
        let (child, child_events) = pending_agent(Arc::new(Notify::new()));
        child.set_model(nanocodex_agent::Model::Sol).await.unwrap();
        child
            .set_thinking(nanocodex_agent::Thinking::High)
            .await
            .unwrap();
        child
            .append_developer_message("Preserved descendant marker amber.")
            .await
            .unwrap();
        let child_session = insert_runtime_session(
            &original,
            &child_reservation,
            Some(parent_reservation.id),
            child,
            child_events,
        )
        .await;
        let contract = checkpoint_fixture().children.remove(0).output_schema;
        {
            let mut state = original.state.lock().await;
            let child = state
                .scopes
                .get_mut(root_id)
                .unwrap()
                .sessions
                .get_mut(&child_reservation.id)
                .unwrap();
            child.status = AgentStatus::Completed {
                output: json!({"answer":42}),
            };
            child.last_output = Some(json!({"answer":42}));
            child.next_turn_token = 7;
            child.output_schema = contract.clone();
            child.output_validator = OutputContract::compile(&contract).unwrap().validator;
            child.host_context = Some(Arc::from("opaque-test-context"));
        }
        original.set_max_resident(1);
        original.enforce_resident_limit(root_id).await;
        assert!(
            original.state.lock().await.scopes[root_id].sessions[&child_reservation.id]
                .harness
                .is_none()
        );
        let mut checkpoint = original.checkpoint(root_id).await.unwrap();
        assert!(
            serde_json::to_string(&checkpoint)
                .unwrap()
                .contains("Preserved descendant marker amber.")
        );
        assert_eq!(
            checkpoint.children[1].runtime.as_ref().unwrap().model,
            nanocodex_agent::Model::Sol
        );
        assert_eq!(
            checkpoint.children[1].runtime.as_ref().unwrap().thinking,
            nanocodex_agent::Thinking::High
        );
        // Parent-first reconstruction must not depend on serialized array order.
        checkpoint.children.reverse();
        original.close_all(root_id).await.unwrap();
        let (restored, _, _updates) = super::channel(2);
        restored
            .restore_checkpoint(&root, checkpoint)
            .await
            .unwrap();
        let directory = restored.directory(root_id, true, false).await;
        assert_eq!(directory.len(), 2);
        assert!(directory.iter().all(|child| child.can_message));
        assert_eq!(
            restored
                .host_context(root_id, child_reservation.id)
                .await
                .as_deref(),
            Some("opaque-test-context")
        );
        let again = restored.checkpoint(root_id).await.unwrap();
        assert!(
            serde_json::to_string(&again)
                .unwrap()
                .contains("Preserved descendant marker amber.")
        );
        assert_eq!(again.children[1].descriptor.session_id, child_session);
        assert_eq!(again.children[1].next_turn_token, 7);
        let receipt = restored
            .send_message(
                root_id,
                child_reservation.id,
                MessagePriority::Deferred,
                MessagePurpose::Coordinate,
                None,
                "Continue from the retained marker.".to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.disposition, MessageDisposition::Started);
        timeout(Duration::from_secs(5), called.notified())
            .await
            .unwrap();
        assert!(
            restored
                .submit_result(&child_session, 7, json!({"answer":43}))
                .await
                .is_err()
        );
        assert!(
            restored
                .submit_result(&child_session, 8, json!({"answer":43,"extra":true}))
                .await
                .is_err()
        );
        assert!(
            !restored
                .submit_result(&child_session, 8, json!({"answer":43}))
                .await
                .unwrap()
        );
        let interrupted = restored.checkpoint(root_id).await.unwrap();
        let child = &interrupted.children[1];
        assert_eq!(child.next_turn_token, 8);
        assert_eq!(child.status, AgentStatus::Interrupted);
        // Checkpoint cancellation preserves the latest accepted result as evidence.
        assert_eq!(child.last_output, Some(json!({"answer":43})));
        assert!(
            !restored.state.lock().await.scopes[root_id].sessions[&child_reservation.id].active
        );
        restored.close_all(root_id).await.unwrap();
        let closed = restored.checkpoint(root_id).await.unwrap();
        assert!(closed.children.is_empty());
        assert_eq!(closed.next_agent_id, 3);
        root.shutdown().await.unwrap();
    }
}
