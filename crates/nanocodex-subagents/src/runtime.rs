// Derived from clabby/tact@1d9ccaefd1d8613dab020812af04a91cd9b4c52c (Apache-2.0).
// Modified for Nanocodex's reusable native/WASM extension runtime.

//! Async child-agent sessions, turns, and lifecycle orchestration.

use super::{
    capacity::{Capacity, TurnCapacity},
    harness::{self, HarnessHandle},
    message::MessageThreads,
    model::{
        AgentDescriptor, AgentId, AgentLineage, AgentMessage, AgentMessageUpdate, AgentStatus,
        AgentTerminalCompletion, AgentThread, AgentUpdate, AgentUsage, MessageDeliveryState,
        MessageDisposition, MessageId, MessagePriority, MessagePurpose, MessageSender,
        ScopedAgentUpdate, SubagentRuntimeId, ThreadId,
    },
    platform::{self, Task, timeout_at},
    task_tree::TaskTree,
};
use futures_util::future::join_all;
use jsonschema::Validator;
use nanocodex_agent::{AgentEvents, Nanocodex, NanocodexError, Result as AgentResult, TurnResult};
use serde::Serialize;
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

// A terminal completion shares the bounded agent mailbox with ordinary
// coordination. Keeping the contract result below this limit means automatic
// parent delivery is exact rather than a lossy summary.
const MAX_TERMINAL_OUTPUT_BYTES: usize = 1024;

pub(super) struct ChildSession {
    pub(super) descriptor: AgentDescriptor,
    pub(super) event_task: Option<Task<()>>,
    pub(super) harness: Option<HarnessHandle>,
    pub(super) harness_task: Option<Task<()>>,
    pub(super) status: AgentStatus,
    pub(super) active: bool,
    pub(super) output_validator: Validator,
    pub(super) next_turn_token: u64,
    pub(super) active_turn_token: Option<u64>,
    pub(super) steering: bool,
    pub(super) submitted_output: Option<Value>,
    pub(super) last_output: Option<Value>,
    pub(super) last_used: u64,
    pub(super) evicted: bool,
    pub(super) lineage: AgentLineage,
    pub(super) usage: AgentUsage,
    pub(super) child_usage: AgentUsage,
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
         output schema below. The tool may be exposed directly or through Code Mode as \
         `tools.submit_result`. If validation rejects the value, correct it and retry. A turn \
         that ends without an accepted result fails.\n\nOutput schema:\n{schema}"
    )
}

pub struct Registry {
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
    pub lineage: AgentLineage,
    pub usage: AgentUsage,
    pub child_usage: AgentUsage,
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
    status: AgentStatus,
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
    ) -> std::io::Result<()> {
        let root_session_id = self.root_session_id(session_id).to_owned();
        let scope = self
            .scopes
            .get_mut(&root_session_id)
            .ok_or_else(|| std::io::Error::other("submit_result is only available to subagents"))?;
        let id = scope
            .topology
            .agent_for_session(session_id)
            .ok_or_else(|| std::io::Error::other("submit_result is only available to subagents"))?;
        let session = scope
            .sessions
            .get_mut(&id)
            .ok_or_else(|| std::io::Error::other("subagent session disappeared"))?;
        if !session.active {
            return Err(std::io::Error::other(
                "submit_result is only available during an active subagent turn",
            ));
        }
        if session.steering {
            return Err(std::io::Error::other(
                "the subagent turn is being steered; retry submit_result",
            ));
        }
        if session.active_turn_token != Some(turn_token) {
            return Err(std::io::Error::other(
                "submit_result used a stale or unknown turn_token",
            ));
        }
        if session.submitted_output.is_some() {
            return Err(std::io::Error::other(
                "submit_result already accepted one result for this turn",
            ));
        }
        let errors = session
            .output_validator
            .iter_errors(&output)
            .take(4)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        if !errors.is_empty() {
            return Err(std::io::Error::other(format!(
                "submitted output does not match the required schema: {}",
                errors.join("; ")
            )));
        }
        let encoded = serde_json::to_vec(&output).map_err(|error| {
            std::io::Error::other(format!("could not encode submitted output: {error}"))
        })?;
        if encoded.len() > MAX_TERMINAL_OUTPUT_BYTES {
            return Err(std::io::Error::other(format!(
                "submitted output exceeds the {MAX_TERMINAL_OUTPUT_BYTES}-byte terminal mailbox limit"
            )));
        }
        session.submitted_output = Some(output);
        Ok(())
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
                    && session.harness.is_some()
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
                "agent {to} is not resident and cannot receive messages"
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

    fn prepare_terminal_completion(
        &mut self,
        root_session_id: &str,
        from: AgentId,
        completion: AgentTerminalCompletion,
    ) -> std::io::Result<PreparedMessage> {
        let next_access = self.next_access.wrapping_add(1);
        self.next_access = next_access;
        let scope = self
            .scopes
            .get_mut(root_session_id)
            .ok_or_else(|| std::io::Error::other("subagent scope disappeared"))?;
        let parent = scope
            .sessions
            .get(&from)
            .and_then(|session| session.descriptor.parent)
            .ok_or_else(|| std::io::Error::other("root agents do not have a parent mailbox"))?;
        let target = scope
            .sessions
            .get_mut(&parent)
            .ok_or_else(|| std::io::Error::other("direct parent disappeared"))?;
        if matches!(target.status, AgentStatus::Closing | AgentStatus::Closed) {
            return Err(std::io::Error::other("direct parent mailbox is closed"));
        }
        let harness = target
            .harness
            .clone()
            .ok_or_else(|| std::io::Error::other("direct parent is not resident"))?;
        target.last_used = next_access;
        let message = scope
            .messages
            .prepare_terminal_completion(from, parent, completion);
        Ok(PreparedMessage {
            root_session_id: root_session_id.to_owned(),
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
        session.status = AgentStatus::Closed;
        Some(ResidentEviction {
            id: candidate,
            harness,
            status: AgentStatus::Closed,
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
            state: tokio::sync::Mutex::new(RegistryState::default()),
            updates,
            revision,
            capacity: Capacity::new(max_concurrency),
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
    ) -> std::io::Result<()> {
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
            schema,
        );
        let lineage = AgentLineage {
            root_session_id: root_session_id.clone(),
            parent_agent_id: descriptor.parent,
            initiating_turn_token: descriptor.parent.and_then(|parent| {
                state
                    .scopes
                    .get(&root_session_id)
                    .and_then(|scope| scope.sessions.get(&parent))
                    .and_then(|session| session.active_turn_token)
            }),
        };
        state.insert(
            root_session_id,
            descriptor.id,
            descriptor.session_id.clone(),
            ChildSession {
                descriptor,
                event_task: Some(event_task),
                harness: Some(harness),
                harness_task: Some(harness_task),
                status: AgentStatus::Pending,
                active: false,
                output_validator: validator,
                next_turn_token: 0,
                active_turn_token: None,
                steering: false,
                submitted_output: None,
                last_output: None,
                last_used: 0,
                evicted: false,
                lineage,
                usage: AgentUsage::default(),
                child_usage: AgentUsage::default(),
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
        let (status, completion) = {
            let mut state = self.state.lock().await;
            let reported_usage = result.as_ref().ok().and_then(|result| result.usage());
            let turn_usage = AgentUsage::from_turn(reported_usage);
            let (status, parent, lineage) = {
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
                session.usage.add(&turn_usage);
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
                (
                    session.status.clone(),
                    session.descriptor.parent,
                    session.lineage.clone(),
                )
            };
            {
                let mut ancestor_id = parent;
                while let Some(ancestor_id_value) = ancestor_id {
                    let Some(ancestor) = state
                        .scopes
                        .get_mut(root_session_id)
                        .and_then(|scope| scope.sessions.get_mut(&ancestor_id_value))
                    else {
                        break;
                    };
                    ancestor.child_usage.add(&turn_usage);
                    ancestor_id = ancestor.descriptor.parent;
                }
            }
            // Explicit subtree interruption/closure is already coordinated by
            // the managing caller. Feeding a cancellation notification back
            // into the parent while that subtree is draining can start fresh
            // mailbox work and prevent the close boundary from settling.
            let completion = parent
                .filter(|_| {
                    matches!(
                        status,
                        AgentStatus::Completed { .. } | AgentStatus::Failed { .. }
                    )
                })
                .map(|_| AgentTerminalCompletion {
                    agent_id: id,
                    lineage,
                    status: terminal_status(&status),
                    usage: turn_usage,
                });
            (status, completion)
        };
        self.send(root_session_id, AgentUpdate::Status { id, status });
        self.changed();
        if let Some(completion) = completion {
            // A parent mailbox is the only automatic delivery path. The
            // terminal state remains inspectable through wait_agent if a
            // closing parent cannot accept the notification.
            drop(
                self.send_terminal_completion(root_session_id, id, completion)
                    .await,
            );
        }
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
            let Some(ResidentEviction {
                id,
                harness,
                status,
            }) = eviction
            else {
                return;
            };
            self.changed();
            self.send(root_session_id, AgentUpdate::Status { id, status });
            if harness.close().await.is_err() {
                self.harness_closed(root_session_id, id).await;
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

    pub async fn send_message(
        &self,
        session_id: &str,
        to: AgentId,
        priority: MessagePriority,
        purpose: MessagePurpose,
        in_reply_to: Option<MessageId>,
        body: String,
    ) -> std::io::Result<MessageReceipt> {
        let _message_guard = self.message_lock.lock().await;
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

    async fn send_terminal_completion(
        &self,
        root_session_id: &str,
        from: AgentId,
        completion: AgentTerminalCompletion,
    ) -> std::io::Result<()> {
        let _message_guard = self.message_lock.lock().await;
        let prepared = self.state.lock().await.prepare_terminal_completion(
            root_session_id,
            from,
            completion,
        )?;
        let delivery = prepared
            .harness
            .enqueue_passive_delivery(prepared.message.clone())?;
        self.state
            .lock()
            .await
            .commit_message(&prepared.root_session_id, prepared.message.clone())?;
        if let Err(error) = delivery.release().await {
            self.state
                .lock()
                .await
                .rollback_message(&prepared.root_session_id, prepared.message.id);
            return Err(error);
        }
        Ok(())
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
            error: "subagent turn ended without a valid submit_result call".to_owned(),
        };
    };
    session.last_output = Some(output.clone());
    AgentStatus::Completed { output }
}

fn terminal_status(status: &AgentStatus) -> AgentStatus {
    match status {
        AgentStatus::Failed { error } => AgentStatus::Failed {
            error: bounded_bytes(error, 512),
        },
        status => status.clone(),
    }
}

impl AgentUsage {
    fn from_turn(usage: Option<&nanocodex_agent::TurnUsage>) -> Self {
        let mut recorded = Self {
            completed_turns: 1,
            ..Self::default()
        };
        if let Some(usage) = usage {
            recorded.turns_with_reported_usage = 1;
            recorded.input_tokens = usage.input_tokens();
            recorded.cached_input_tokens = usage.cached_input_tokens();
            recorded.cache_write_input_tokens = usage.cache_write_input_tokens();
            recorded.output_tokens = usage.output_tokens();
            recorded.reasoning_output_tokens = usage.reasoning_output_tokens();
            recorded.total_tokens = usage.total_tokens();
        }
        recorded
    }

    const fn add(&mut self, other: &Self) {
        self.completed_turns = self.completed_turns.saturating_add(other.completed_turns);
        self.turns_with_reported_usage = self
            .turns_with_reported_usage
            .saturating_add(other.turns_with_reported_usage);
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.cached_input_tokens = self
            .cached_input_tokens
            .saturating_add(other.cached_input_tokens);
        self.cache_write_input_tokens = self
            .cache_write_input_tokens
            .saturating_add(other.cache_write_input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.reasoning_output_tokens = self
            .reasoning_output_tokens
            .saturating_add(other.reasoning_output_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
    }
}

fn first_error(results: Vec<std::io::Result<()>>) -> std::io::Result<()> {
    results.into_iter().find(Result::is_err).unwrap_or(Ok(()))
}

fn bounded_summary(value: &str) -> String {
    bounded_bytes(value, 160)
}

fn bounded_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let end = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= max_bytes)
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
            lineage: self.lineage.clone(),
            usage: self.usage.clone(),
            child_usage: self.child_usage.clone(),
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
        AgentDescriptor, AgentId, AgentLineage, AgentStatus, AgentUsage, ChildSession,
        OutputContract, Registry, RegistryState, complete_session, completion_instructions,
        forward_events,
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

        assert!(instructions.contains("tools.submit_result"));
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
            event_task: Some(platform::spawn(async {})),
            harness: None,
            harness_task: None,
            status: AgentStatus::Pending,
            active: false,
            output_validator: test_contract().validator,
            next_turn_token: 0,
            active_turn_token: None,
            steering: false,
            submitted_output: None,
            last_output: None,
            last_used: 0,
            evicted: false,
            lineage: AgentLineage {
                root_session_id: "test-root".to_owned(),
                parent_agent_id: parent,
                initiating_turn_token: None,
            },
            usage: AgentUsage::default(),
            child_usage: AgentUsage::default(),
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
        assert!(invalid.unwrap_err().to_string().contains("required schema"));
        registry
            .submit_result("child-session", 1, json!({ "answer": 42 }))
            .unwrap();
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

        assert!(matches!(status, AgentStatus::Failed { error } if error.contains("submit_result")));
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
        registry.finish_turn_steer("main", steer, true);
        assert!(
            registry
                .submit_result("child-session", 1, json!({ "report": "stale" }))
                .is_err()
        );
        registry
            .submit_result("child-session", 2, json!({ "report": "current" }))
            .unwrap();

        assert!(registry.begin_turn_steer("main", reservation.id).is_none());
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
    async fn eviction_marks_an_unresumable_agent_closed_and_non_manageable() {
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
        assert_eq!(entry.status, AgentStatus::Closed);
        assert!(!entry.can_message);
        assert!(!entry.can_manage);
        let (summaries, timed_out) = registry
            .wait("main", &[interrupted], Duration::from_millis(1))
            .await
            .unwrap();
        assert!(!timed_out);
        assert_eq!(summaries[0].status, AgentStatus::Closed);
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
}
