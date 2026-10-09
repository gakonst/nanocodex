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
use nanocodex_agent::{HarnessModel, Lineage, SessionCheckpoint, Thinking};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
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
    /// Records one child's latest committed checkpoint as that child's own
    /// durable session, so it is listable, readable and resumable by its
    /// session ID like any other session.
    ///
    /// The checkpoint carries the child's distinct identity and its
    /// [`nanocodex_agent::Origin::Subagent`] lineage under `root_session_id`.
    /// It is called after the journal containing the same boundary was saved,
    /// and again for every later boundary. Stores without a session catalog
    /// keep only the journal.
    fn record_session<'a>(
        &'a self,
        _root_session_id: &'a str,
        _checkpoint: SessionCheckpoint,
    ) -> SubagentStoreFuture<'a, std::io::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

/// In-memory [`SubagentStore`], useful for tests and single-process hosts that
/// rebuild their runtime without losing the process.
#[derive(Clone, Default)]
pub struct MemorySubagentStore {
    values: Arc<Mutex<HashMap<String, String>>>,
    sessions: Arc<Mutex<HashMap<String, SessionCheckpoint>>>,
}

impl MemorySubagentStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The latest checkpoint recorded for a child session.
    #[must_use]
    pub fn session(&self, session_id: &str) -> Option<SessionCheckpoint> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .cloned()
    }

    /// Every recorded child session ID, sorted.
    #[must_use]
    pub fn sessions(&self) -> Vec<String> {
        let mut ids = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        ids.sort();
        ids
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

    fn record_session<'a>(
        &'a self,
        _root_session_id: &'a str,
        checkpoint: SessionCheckpoint,
    ) -> SubagentStoreFuture<'a, std::io::Result<()>> {
        Box::pin(async move {
            self.sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(checkpoint.session_id().to_owned(), checkpoint);
            Ok(())
        })
    }
}

/// Journal version written by this runtime.
///
/// Version 1 stored the removed per-family child snapshots: a Codex
/// `ChildRuntimeSnapshot` under `checkpoint` and a Claude form under
/// `native_checkpoint`. Version 2 stores one family-tagged
/// [`SessionCheckpoint`] under `checkpoint`. Both remain readable.
pub(super) const JOURNAL_VERSION: u32 = 2;
/// Oldest journal version this runtime still restores.
pub(super) const MIN_JOURNAL_VERSION: u32 = 1;

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
    /// Automatic restart resumes since the child last finished a turn.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) resume_attempts: u32,
    /// Latest committed boundary; absent when no portable checkpoint exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) checkpoint: Option<JournaledCheckpoint>,
    /// Version-1 boundary of a native (Claude) backend; read, never written.
    #[serde(default, skip_serializing)]
    pub(super) native_checkpoint: Option<PersistedNative>,
    /// Bounded tool calls observed during the unfinished turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) in_flight_calls: Vec<InFlightCall>,
    /// Observed calls dropped by the retention bound.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) in_flight_omitted: u32,
}

/// Bounded evidence of one tool call observed during a turn. It is kept
/// until the turn settles (no checkpoint pruning), so after a restart it may
/// or may not also be in the restored history. It is never replayed and is
/// not a receipt journal; it only names calls whose outcome must be reconciled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct InFlightCall {
    /// Full provider identity, used to match its result.
    pub(super) call_id: String,
    pub(super) tool: String,
    /// Display-bounded argument summary.
    pub(super) arguments: String,
    /// A result was observed before the restart.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) result_recorded: bool,
}

/// Calls retained per turn. Calls with an observed result are evicted before
/// calls still running; every eviction is counted and reported.
pub(super) const MAX_IN_FLIGHT_CALLS: usize = 8;
const MAX_IN_FLIGHT_FIELD_BYTES: usize = 240;

impl InFlightCall {
    pub(super) fn new(call_id: &str, tool: &str, arguments: Option<&Value>) -> Self {
        let arguments = match arguments {
            Some(Value::String(text)) => text.clone(),
            Some(value) => value.to_string(),
            None => String::new(),
        };
        Self {
            call_id: call_id.to_owned(),
            tool: bounded(tool),
            arguments: bounded(&arguments),
            result_recorded: false,
        }
    }
}

/// Appends a call, evicting the oldest call with an observed result first,
/// else the oldest call. Returns how many calls were evicted.
pub(super) fn retain_call(calls: &mut Vec<InFlightCall>, call: InFlightCall) -> u32 {
    calls.retain(|existing| existing.call_id != call.call_id);
    calls.push(call);
    let mut evicted = 0;
    while calls.len() > MAX_IN_FLIGHT_CALLS {
        let index = calls
            .iter()
            .position(|existing| existing.result_recorded)
            .unwrap_or(0);
        calls.remove(index);
        evicted += 1;
    }
    evicted
}

fn bounded(text: &str) -> String {
    if text.len() <= MAX_IN_FLIGHT_FIELD_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_IN_FLIGHT_FIELD_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Model-visible list of calls whose outcome a restart made unknown.
pub(super) fn in_flight_evidence(calls: &[InFlightCall], omitted: u32) -> Option<String> {
    if calls.is_empty() && omitted == 0 {
        return None;
    }
    let mut lines = calls
        .iter()
        .map(|call| {
            let state = if call.result_recorded {
                "a result was observed before the restart"
            } else {
                "started; no result was observed"
            };
            format!(
                "- {} (call_id {}; {state}): {}",
                call.tool,
                bounded(&call.call_id),
                call.arguments
            )
        })
        .collect::<Vec<_>>();
    if omitted > 0 {
        lines.push(format!(
            "- {omitted} additional observed call(s) omitted; this is not a complete history."
        ));
    }
    Some(format!(
        "Tool calls observed during the interrupted turn. Some may already appear in your \
         restored history; any that do not may or may not have taken effect. This summary is \
         not a receipt: consult your retained history and reconcile tool receipts or external \
         effects before repeating any of them:\n{}",
        lines.join("\n")
    ))
}

/// A journaled conversation boundary, decoded by its stored format.
pub(super) enum JournaledCheckpoint {
    /// Version-2 family-tagged portable checkpoint.
    Current(SessionCheckpoint),
    /// Version-1 Codex `ChildRuntimeSnapshot` JSON, upgraded on restore.
    LegacyCodex(Value),
}

impl Serialize for JournaledCheckpoint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Current(checkpoint) => checkpoint.serialize(serializer),
            Self::LegacyCodex(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for JournaledCheckpoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Every current checkpoint carries a format tag; version-1 Codex
        // snapshots never did. Decode the tagged form strictly so a corrupt
        // current checkpoint is reported instead of misread as legacy data.
        let value = Value::deserialize(deserializer)?;
        if value.get("format").is_some() {
            serde_json::from_value(value)
                .map(Self::Current)
                .map_err(serde::de::Error::custom)
        } else {
            Ok(Self::LegacyCodex(value))
        }
    }
}

impl JournaledCheckpoint {
    fn checkpoint(&self, lineage: &Lineage) -> std::io::Result<SessionCheckpoint> {
        match self {
            Self::Current(checkpoint) => {
                checkpoint.validate().map_err(std::io::Error::other)?;
                Ok(checkpoint.clone())
            }
            Self::LegacyCodex(value) => Ok(legacy_codex_checkpoint(value, lineage)?),
        }
    }
}

/// A version-1 journal checkpoint that cannot be upgraded to a
/// [`SessionCheckpoint`].
#[derive(Debug)]
pub(super) enum LegacyCheckpointError {
    /// The stored Codex `ChildRuntimeSnapshot` is malformed.
    Codex(String),
    /// The stored native (Claude) checkpoint is malformed.
    Native(String),
}

impl std::fmt::Display for LegacyCheckpointError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Codex(detail) => {
                write!(formatter, "invalid journaled Codex checkpoint: {detail}")
            }
            Self::Native(detail) => {
                write!(formatter, "invalid journaled native checkpoint: {detail}")
            }
        }
    }
}

impl std::error::Error for LegacyCheckpointError {}

impl From<LegacyCheckpointError> for std::io::Error {
    fn from(error: LegacyCheckpointError) -> Self {
        Self::new(std::io::ErrorKind::InvalidData, error)
    }
}

fn legacy_field<T: serde::de::DeserializeOwned>(
    fields: &mut serde_json::Map<String, Value>,
    name: &str,
) -> Result<T, LegacyCheckpointError> {
    let value = fields
        .remove(name)
        .ok_or_else(|| LegacyCheckpointError::Codex(format!("missing {name}")))?;
    serde_json::from_value(value)
        .map_err(|error| LegacyCheckpointError::Codex(format!("invalid {name}: {error}")))
}

/// Upgrades a version-1 Codex `ChildRuntimeSnapshot`.
///
/// Its non-identity fields (service tier, transport mode, and conversation)
/// are exactly the Codex checkpoint payload. Before the first turn a Codex
/// child's cache lineage was its own session ID; afterwards it is the
/// lineage recorded by the conversation itself.
fn legacy_codex_checkpoint(
    value: &Value,
    lineage: &Lineage,
) -> Result<SessionCheckpoint, LegacyCheckpointError> {
    let Value::Object(fields) = value else {
        return Err(LegacyCheckpointError::Codex("expected an object".into()));
    };
    let mut payload = fields.clone();
    let session_id: String = legacy_field(&mut payload, "session_id")?;
    let model: nanocodex_agent::Model = legacy_field(&mut payload, "model")?;
    let thinking: Thinking = legacy_field(&mut payload, "thinking")?;
    let conversation = payload
        .get("conversation")
        .filter(|conversation| !conversation.is_null());
    let has_conversation = conversation.is_some();
    let conversation_id = match conversation {
        Some(conversation) => conversation
            .get("lineage_id")
            .and_then(Value::as_str)
            .ok_or_else(|| LegacyCheckpointError::Codex("conversation has no lineage".into()))?
            .to_owned(),
        None => session_id.clone(),
    };
    Ok(SessionCheckpoint::native(
        session_id,
        HarnessModel::Codex(model),
        thinking,
        lineage.clone(),
        conversation_id,
        None,
        has_conversation,
        Value::Object(payload),
    ))
}

/// Version-1 portable form of a native (Claude) child checkpoint.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PersistedNative {
    model: String,
    session_id: String,
    thinking: Thinking,
    payload: String,
    has_conversation: bool,
}

impl PersistedNative {
    /// Upgrades to a [`SessionCheckpoint`]. Version 1 stored the backend's
    /// native state as JSON text; checkpoints carry the same state as a JSON
    /// value, decoded only by the owning family when the child is restored.
    fn into_checkpoint(
        self,
        lineage: &Lineage,
    ) -> Result<SessionCheckpoint, LegacyCheckpointError> {
        let model = self.model.parse::<HarnessModel>().map_err(|error| {
            LegacyCheckpointError::Native(format!("invalid journaled model: {error}"))
        })?;
        if self.session_id.trim().is_empty() {
            return Err(LegacyCheckpointError::Native("empty session ID".into()));
        }
        let payload = serde_json::from_str(&self.payload).map_err(|error| {
            LegacyCheckpointError::Native(format!("payload is not JSON: {error}"))
        })?;
        Ok(SessionCheckpoint::native(
            self.session_id.clone(),
            model,
            self.thinking,
            lineage.clone(),
            self.session_id,
            None,
            self.has_conversation,
            payload,
        ))
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

    fn record_session<'a>(
        &'a self,
        _root_session_id: &'a str,
        checkpoint: SessionCheckpoint,
    ) -> SubagentStoreFuture<'a, std::io::Result<()>> {
        self.0.record_child(checkpoint)
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
    checkpoint: Option<&SessionCheckpoint>,
) -> PersistedAgent {
    let checkpoint = checkpoint
        .or(session.stored_runtime.as_ref())
        .cloned()
        .map(JournaledCheckpoint::Current);
    PersistedAgent {
        descriptor: session.descriptor.clone(),
        binding_task: Some(session.binding_task.clone()),
        status: session.status.clone(),
        output_schema: session.output_schema.clone(),
        host_context: session.host_context.as_deref().map(str::to_owned),
        last_output: session.last_output.clone(),
        next_instruction_revision: session.next_instruction_revision,
        turn_in_flight: session.active || matches!(session.status, AgentStatus::Running),
        resume_attempts: session.resume_attempts,
        checkpoint,
        native_checkpoint: None,
        in_flight_calls: session.in_flight_calls.clone(),
        in_flight_omitted: session.in_flight_omitted,
    }
}

impl PersistedAgent {
    /// The journaled checkpoint for any backend family, upgrading version-1
    /// entries. `lineage` is assigned to legacy entries, which recorded none.
    pub(super) fn snapshot(&self, lineage: &Lineage) -> std::io::Result<Option<SessionCheckpoint>> {
        if let Some(checkpoint) = &self.checkpoint {
            return checkpoint.checkpoint(lineage).map(Some);
        }
        Ok(self
            .native_checkpoint
            .clone()
            .map(|native| native.into_checkpoint(lineage))
            .transpose()?)
    }
}

/// Lineage of a journaled child within its root's restored tree.
pub(super) fn journaled_lineage(
    root_session_id: &str,
    agents: &[PersistedAgent],
    id: AgentId,
) -> Lineage {
    let parent_of = |id: AgentId| {
        agents
            .iter()
            .find(|agent| agent.descriptor.id == id)
            .and_then(|agent| agent.descriptor.parent)
    };
    let parent_session_id = parent_of(id)
        .and_then(|parent| agents.iter().find(|agent| agent.descriptor.id == parent))
        .map_or_else(
            || root_session_id.to_owned(),
            |parent| parent.descriptor.session_id.clone(),
        );
    let mut depth = 1;
    let mut current = parent_of(id);
    while let Some(parent) = current
        && depth <= agents.len()
    {
        depth += 1;
        current = parent_of(parent);
    }
    Lineage::new(
        root_session_id,
        Some(parent_session_id),
        nanocodex_agent::Origin::Subagent,
        u32::try_from(depth).unwrap_or(u32::MAX),
    )
}

/// Consecutive restart resumes allowed before a turn must settle. Runtime loss
/// that recurs on every resume would otherwise restart the child forever.
pub(super) const MAX_RESUME_ATTEMPTS: u32 = 3;

const fn is_zero(value: &u32) -> bool {
    *value == 0
}

pub(super) fn restored_session(
    agent: PersistedAgent,
    snapshot: Option<SessionCheckpoint>,
) -> std::io::Result<(ChildSession, bool, bool)> {
    let contract = OutputContract::compile(&agent.output_schema)?;
    let recoverable = snapshot.is_some();
    let terminal = matches!(agent.status, AgentStatus::Closing | AgentStatus::Closed);
    let in_flight = !terminal
        && (agent.turn_in_flight
            || matches!(agent.status, AgentStatus::Running | AgentStatus::Pending));
    let exhausted = in_flight && recoverable && agent.resume_attempts >= MAX_RESUME_ATTEMPTS;
    let evidence = in_flight_evidence(&agent.in_flight_calls, agent.in_flight_omitted);
    let status = if terminal {
        AgentStatus::Closed
    } else if exhausted {
        let mut error = format!(
            "subagent recovery exhausted: the runtime restarted during each of the last \
             {MAX_RESUME_ATTEMPTS} automatic resumes of this turn. Inspect child evidence \
             before delegating a recovery; do not replay task side effects."
        );
        if let Some(evidence) = &evidence {
            error.push_str("\n\n");
            error.push_str(evidence);
        }
        AgentStatus::Failed { error }
    } else if in_flight && !recoverable {
        let mut error = "subagent could not be restored after a runtime restart: no portable \
                         checkpoint was available"
            .to_owned();
        if let Some(evidence) = &evidence {
            error.push_str("\n\n");
            error.push_str(evidence);
        }
        AgentStatus::Failed { error }
    } else if in_flight {
        AgentStatus::Interrupted
    } else {
        agent.status
    };
    let resume = in_flight && recoverable && !exhausted;
    let resume_attempts = if resume { agent.resume_attempts + 1 } else { 0 };
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
    session.resume_attempts = resume_attempts;
    // Retain until the turn settles: a second loss before then is still unknown.
    if in_flight {
        session.in_flight_calls = agent.in_flight_calls;
        session.in_flight_omitted = agent.in_flight_omitted;
    }
    Ok((session, resume, (!recoverable && !terminal) || exhausted))
}
