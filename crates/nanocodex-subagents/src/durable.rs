//! Durable subagent task trees.
//!
//! A host that persists its root agent can also persist that root's subagent
//! tree by installing a [`SubagentStore`]. The registry then journals one
//! versioned value per root session after every lifecycle change: topology,
//! identities, roles and tasks, output contracts, statuses, accepted outputs,
//! and a reference to each child's latest committed conversation boundary.
//!
//! Child conversations are stored as separate immutable records addressed by
//! [`checkpoint_key`]. A journal write encodes only the children whose
//! boundary changed, so its cost does not grow with the number of children.
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
/// must atomically store its records and replace the previous value for the
/// same root. Records are immutable JSON texts addressed by [`checkpoint_key`].
/// On WebAssembly hosts the registry is still shared through `Send + Sync`
/// tool objects, so JavaScript-backed stores wrap their single-threaded handles.
pub trait SubagentStore: Send + Sync {
    /// Loads the latest journal for a root session.
    fn load<'a>(
        &'a self,
        root_session_id: &'a str,
    ) -> SubagentStoreFuture<'a, std::io::Result<Option<String>>>;
    /// Atomically stores `records` and replaces the journal for a root
    /// session. Records the store already holds may be supplied again.
    fn save<'a>(
        &'a self,
        root_session_id: &'a str,
        payload: String,
        records: Vec<Arc<str>>,
    ) -> SubagentStoreFuture<'a, std::io::Result<()>>;
    /// Loads one record referenced by a saved journal.
    fn load_record<'a>(
        &'a self,
        root_session_id: &'a str,
        key: &'a str,
    ) -> SubagentStoreFuture<'a, std::io::Result<String>>;
}

/// In-memory [`SubagentStore`], useful for tests and single-process hosts that
/// rebuild their runtime without losing the process.
#[derive(Clone, Default)]
pub struct MemorySubagentStore {
    values: Arc<Mutex<HashMap<String, String>>>,
    records: Arc<Mutex<MemoryRecords>>,
}

/// Checkpoint records by (root session, key).
type MemoryRecords = HashMap<(String, String), Arc<str>>;

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
        records: Vec<Arc<str>>,
    ) -> SubagentStoreFuture<'a, std::io::Result<()>> {
        Box::pin(async move {
            let mut stored = self
                .records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for record in records {
                stored.insert(
                    (root_session_id.to_owned(), checkpoint_key(&record)),
                    record,
                );
            }
            self.values
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(root_session_id.to_owned(), payload);
            Ok(())
        })
    }

    fn load_record<'a>(
        &'a self,
        root_session_id: &'a str,
        key: &'a str,
    ) -> SubagentStoreFuture<'a, std::io::Result<String>> {
        Box::pin(async move {
            self.records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&(root_session_id.to_owned(), key.to_owned()))
                .map(|record| record.to_string())
                .ok_or_else(|| std::io::Error::other(format!("missing subagent checkpoint {key}")))
        })
    }
}

/// Identity of a journal record: the lowercase hex SHA-256 of its JSON text.
#[must_use]
pub fn checkpoint_key(json: &str) -> String {
    use sha2::{Digest, Sha256};
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut key = String::with_capacity(64);
    for byte in Sha256::digest(json.as_bytes()) {
        key.push(HEX[usize::from(byte >> 4)] as char);
        key.push(HEX[usize::from(byte & 15)] as char);
    }
    key
}

/// A child's latest committed boundary, encoded once when it is captured.
#[derive(Clone)]
pub(super) struct JournalCheckpoint {
    pub(super) key: Arc<str>,
    /// Encoded record not yet acknowledged by a journal save.
    pub(super) pending: Option<Arc<str>>,
}

impl JournalCheckpoint {
    pub(super) fn encode(snapshot: &ChildSnapshot) -> std::io::Result<Self> {
        let record = match snapshot {
            ChildSnapshot::Codex(snapshot) => CheckpointRecordRef {
                checkpoint: Some(snapshot),
                native_checkpoint: None,
            },
            ChildSnapshot::Native {
                model,
                session_id,
                thinking,
                payload,
                has_conversation,
            } => CheckpointRecordRef {
                checkpoint: None,
                native_checkpoint: Some(PersistedNativeRef {
                    model: model.as_str(),
                    session_id,
                    thinking: *thinking,
                    payload,
                    has_conversation: *has_conversation,
                }),
            },
        };
        let json: Arc<str> = serde_json::to_string(&record)
            .map_err(std::io::Error::other)?
            .into();
        Ok(Self {
            key: checkpoint_key(&json).into(),
            pending: Some(json),
        })
    }

    /// A boundary whose record a previous journal save already stored.
    pub(super) fn stored(key: String) -> Self {
        Self {
            key: key.into(),
            pending: None,
        }
    }
}

/// Decodes a checkpoint record loaded on demand, verifying its identity.
pub(super) fn decode_checkpoint(json: &str, key: &str) -> std::io::Result<ChildSnapshot> {
    if checkpoint_key(json) != key {
        return Err(std::io::Error::other(format!(
            "subagent checkpoint {key} does not match its record"
        )));
    }
    let record: CheckpointRecord = serde_json::from_str(json).map_err(|error| {
        std::io::Error::other(format!("invalid subagent checkpoint {key}: {error}"))
    })?;
    match (record.checkpoint, record.native_checkpoint) {
        (Some(snapshot), None) => Ok(ChildSnapshot::Codex(snapshot)),
        (None, Some(native)) => native.into_snapshot(),
        _ => Err(std::io::Error::other(format!(
            "subagent checkpoint {key} has no single backend"
        ))),
    }
}

/// Borrowed encoding of a checkpoint record; no conversation is cloned.
#[derive(Serialize)]
struct CheckpointRecordRef<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    checkpoint: Option<&'a ChildRuntimeSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    native_checkpoint: Option<PersistedNativeRef<'a>>,
}

#[derive(Serialize)]
struct PersistedNativeRef<'a> {
    model: &'a str,
    session_id: &'a str,
    thinking: nanocodex_agent::Thinking,
    payload: &'a str,
    has_conversation: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointRecord {
    #[serde(default)]
    checkpoint: Option<ChildRuntimeSnapshot>,
    #[serde(default)]
    native_checkpoint: Option<PersistedNative>,
}

/// Version 2 references child checkpoint records; version 1 embedded them.
pub(super) const JOURNAL_VERSION: u32 = 2;
pub(super) const EMBEDDED_JOURNAL_VERSION: u32 = 1;

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
    /// Latest committed boundary; absent for native backends without a
    /// portable checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) checkpoint: Option<ChildRuntimeSnapshot>,
    /// Latest committed boundary of a native (for example Claude) backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) native_checkpoint: Option<PersistedNative>,
    /// Record key of the latest committed boundary of any backend family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) checkpoint_ref: Option<String>,
    /// Bounded tool calls observed during the unfinished turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) in_flight_calls: Vec<InFlightCall>,
    /// Observed calls dropped by the retention bound.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) in_flight_omitted: u32,
    /// Result submit_result accepted for the running turn. Journaled before
    /// the child learns of acceptance, so a restart completes the turn with
    /// it rather than running the child again. Older readers ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) accepted_output: Option<Value>,
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
        records: Vec<Arc<str>>,
    ) -> SubagentStoreFuture<'a, std::io::Result<()>> {
        self.0.save(payload, records)
    }

    fn load_record<'a>(
        &'a self,
        _root_session_id: &'a str,
        key: &'a str,
    ) -> SubagentStoreFuture<'a, std::io::Result<String>> {
        self.0.load_record(key.to_owned())
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
    /// Agents whose interrupted turn had already accepted its result. They
    /// complete with it instead of running again, and are announced once.
    pub completed: Vec<AgentId>,
}

pub(super) const RESUME_MESSAGE: &str = "The sub-agent runtime restarted while your previous \
turn was running. Your conversation was restored from its latest committed checkpoint: every \
step in your history completed, but a tool call that was still running at the restart is not \
shown and may or may not have taken effect. Inspect the current workspace state where that \
matters, do not repeat side effects that already happened, and continue your delegated task \
from where your history ends. Submit a result only once the whole delegated task is complete.";

pub(super) fn persist_agent(
    session: &ChildSession,
    checkpoint: Option<&JournalCheckpoint>,
    records: &mut Vec<(Arc<str>, Arc<str>)>,
) -> std::io::Result<PersistedAgent> {
    // A retained runtime is normally captured as a journal checkpoint too;
    // encode it here only if no capture exists.
    let fallback = match (checkpoint, &session.stored_runtime) {
        (None, Some(snapshot)) => Some(JournalCheckpoint::encode(snapshot)?),
        _ => None,
    };
    let checkpoint_ref = checkpoint.or(fallback.as_ref()).map(|checkpoint| {
        if let Some(json) = &checkpoint.pending {
            records.push((Arc::clone(&checkpoint.key), Arc::clone(json)));
        }
        checkpoint.key.to_string()
    });
    Ok(PersistedAgent {
        descriptor: session.descriptor.clone(),
        binding_task: Some(session.binding_task.clone()),
        status: session.status.clone(),
        output_schema: session.output_schema.clone(),
        host_context: session.host_context.as_deref().map(str::to_owned),
        last_output: session.last_output.clone(),
        next_instruction_revision: session.next_instruction_revision,
        turn_in_flight: session.active || matches!(session.status, AgentStatus::Running),
        resume_attempts: session.resume_attempts,
        checkpoint: None,
        native_checkpoint: None,
        checkpoint_ref,
        in_flight_calls: session.in_flight_calls.clone(),
        in_flight_omitted: session.in_flight_omitted,
        accepted_output: session
            .active
            .then(|| session.submitted_output.clone())
            .flatten(),
    })
}

impl PersistedAgent {
    /// Replaces a checkpoint reference with its stored record.
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

/// Consecutive restart resumes allowed without committed progress. Runtime loss
/// that recurs on every resume would otherwise restart the child forever; a
/// resumed turn that journals a checkpoint after completing a tool call starts
/// the budget over, so unrelated restarts during a long turn do not exhaust it.
pub(super) const MAX_RESUME_ATTEMPTS: u32 = 3;

const fn is_zero(value: &u32) -> bool {
    *value == 0
}

pub(super) fn restored_session(
    agent: PersistedAgent,
) -> std::io::Result<(ChildSession, bool, bool)> {
    let contract = OutputContract::compile(&agent.output_schema)?;
    let snapshot = agent.snapshot()?;
    // A referenced record is loaded only when the child next runs, so a
    // restored task tree does not decode every idle conversation at once.
    let journaled = snapshot.is_none() && agent.checkpoint_ref.is_some();
    let recoverable = snapshot.is_some() || journaled;
    let terminal = matches!(agent.status, AgentStatus::Closing | AgentStatus::Closed);
    let running = !terminal
        && (agent.turn_in_flight
            || matches!(agent.status, AgentStatus::Running | AgentStatus::Pending));
    // An accepted result is that turn's logical completion: never rerun it.
    let accepted = running.then_some(agent.accepted_output).flatten();
    let in_flight = running && accepted.is_none();
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
    } else if let Some(output) = &accepted {
        AgentStatus::Completed {
            output: output.clone(),
        }
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
        accepted.or(agent.last_output),
    );
    if let Some(task) = binding_task {
        session.binding_task = task;
    }
    session.resume_attempts = resume_attempts;
    session.journaled_runtime = journaled;
    // Retain until the turn settles: a second loss before then is still unknown.
    if in_flight {
        session.in_flight_calls = agent.in_flight_calls;
        session.in_flight_omitted = agent.in_flight_omitted;
    }
    Ok((session, resume, (!recoverable && !terminal) || exhausted))
}
