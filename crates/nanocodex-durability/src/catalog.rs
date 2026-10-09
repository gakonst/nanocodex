//! Family-neutral catalog of the local sessions retained in one durable store.
//!
//! Durable state is the single source of truth for local sessions of every
//! harness. Each session's head carries a [`SessionRecord`] next to its
//! execution state, so listing, inspection, and branching need no sidecar
//! files. Every read here is non-fencing: listing or previewing sessions never
//! acquires an owner and therefore never interrupts a live process.

use std::path::PathBuf;

use nanocodex_agent::{HarnessFamily, HarnessModel, Lineage};
use serde::{Deserialize, Serialize};

/// Family-neutral metadata recorded inside a durable session's state head.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    /// Session identity; always equal to the durable state identity.
    pub session_id: String,
    /// Model selected when the session was last described. The checkpoint
    /// remains authoritative for interactive model changes.
    pub model: HarnessModel,
    /// Absolute workspace the session operates in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    /// Host-chosen display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Provenance within its conversation tree.
    pub lineage: Lineage,
    /// Milliseconds since the Unix epoch when the record was first described.
    #[serde(default)]
    pub created_at_ms: u64,
    /// Milliseconds since the Unix epoch of the latest committed write.
    #[serde(default)]
    pub updated_at_ms: u64,
    /// Whether receipt retention has removed older turns from this session.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub history_pruned: bool,
}

impl SessionRecord {
    /// Describes a fresh root session.
    #[must_use]
    pub fn root(
        session_id: impl Into<String>,
        model: HarnessModel,
        workspace: Option<PathBuf>,
    ) -> Self {
        let session_id = session_id.into();
        let now = now_ms();
        Self {
            lineage: Lineage::root(session_id.clone()),
            session_id,
            model,
            workspace,
            title: None,
            created_at_ms: now,
            updated_at_ms: now,
            history_pruned: false,
        }
    }

    /// Native agent-loop family that owns the session.
    #[must_use]
    pub const fn family(&self) -> HarnessFamily {
        self.model.family()
    }

    /// Sets the display title.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Sets the provenance recorded for this session.
    #[must_use]
    pub fn with_lineage(mut self, lineage: Lineage) -> Self {
        self.lineage = lineage;
        self
    }

    /// Describes a session derived from this one, such as a fork or branch.
    #[must_use]
    pub fn derive(&self, session_id: impl Into<String>, origin: nanocodex_agent::Origin) -> Self {
        let session_id = session_id.into();
        let now = now_ms();
        Self {
            lineage: Lineage::child_of(&self.lineage, self.session_id.clone(), origin),
            session_id,
            model: self.model,
            workspace: self.workspace.clone(),
            title: self.title.clone(),
            created_at_ms: now,
            updated_at_ms: now,
            history_pruned: false,
        }
    }

    pub(crate) fn touch(&mut self) {
        let now = now_ms();
        if now > self.updated_at_ms {
            self.updated_at_ms = now;
        }
    }

    /// Applies a newer description while keeping stored identity facts.
    pub(crate) fn merged(&self, next: Self) -> Self {
        let lineage = if next.lineage == Lineage::root(next.session_id.clone()) {
            // A plain reopen describes itself as a root; never erase provenance.
            self.lineage.clone()
        } else {
            next.lineage
        };
        Self {
            session_id: next.session_id,
            model: next.model,
            workspace: next.workspace.or_else(|| self.workspace.clone()),
            title: next.title.or_else(|| self.title.clone()),
            lineage,
            created_at_ms: if self.created_at_ms == 0 {
                next.created_at_ms
            } else {
                self.created_at_ms
            },
            updated_at_ms: self.updated_at_ms,
            history_pruned: self.history_pruned || next.history_pruned,
        }
    }
}

#[cfg(not(target_family = "wasm"))]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(target_family = "wasm")]
const fn now_ms() -> u64 {
    0
}

#[cfg(not(target_family = "wasm"))]
pub use native::*;

#[cfg(not(target_family = "wasm"))]
mod native {
    use std::{collections::HashSet, path::PathBuf};

    use nanocodex_agent::{HarnessFamily, HarnessModel, Lineage, Origin};
    use serde::Serialize;
    use serde_json::Value;

    use super::SessionRecord;
    use crate::{
        DurableSession, DurableState, EncodedPayload, Error, OperationStatus, OwnerId, Result,
        StateStore, Transition, session::reduce_peeked, shared_store::SharedStore,
    };

    /// One visible conversation entry, shared by every harness.
    pub use nanocodex_agent::session::TranscriptItem;

    /// Maximum sessions returned by [`SessionStore::list`], most recently
    /// updated first.
    pub const LIST_LIMIT: usize = 1000;

    /// A listed session.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    pub struct SessionSummary {
        /// Recorded (or, for older journals, inferred) metadata.
        pub record: SessionRecord,
        /// First user prompt, for pickers.
        pub preview: Option<String>,
    }

    /// Settlement of one retained turn.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum TurnStatus {
        /// Accepted and not yet settled.
        Pending,
        /// Completed with a checkpoint.
        Completed,
        /// Failed with a checkpoint.
        Failed,
        /// Cancelled.
        Cancelled,
    }

    /// One retained user turn, in submission order.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    pub struct StoredTurn {
        /// Durable operation identity, accepted by [`BranchPoint`].
        pub id: String,
        /// User-visible prompt text.
        pub preview: Option<String>,
        /// Settlement.
        pub status: TurnStatus,
        /// Original host input. Reference data, never executable work.
        pub input: Value,
    }

    /// A session loaded for display or resume.
    #[derive(Clone, Debug)]
    pub struct StoredSession {
        /// Metadata and preview.
        pub summary: SessionSummary,
        /// Visible conversation at the latest checkpoint.
        pub transcript: Vec<TranscriptItem>,
        /// Retained turns in submission order.
        pub turns: Vec<StoredTurn>,
        /// Fully hydrated provider-native latest checkpoint, decoded only by
        /// the owning family. Contains the unredacted conversation.
        pub checkpoint: Option<Value>,
    }

    impl StoredSession {
        /// The latest checkpoint as a portable [`nanocodex_agent::SessionCheckpoint`]
        /// for the owning family, keeping this session's identity and lineage.
        ///
        /// Durable sessions normally resume through
        /// [`crate::DurableAgentExt::durability`] instead, which restores the
        /// same boundary while keeping the durable owner.
        ///
        /// # Errors
        ///
        /// Returns [`Error::InvalidState`] when the checkpoint cannot be
        /// decoded, or for a Claude session when this crate is built without
        /// its `claude` feature.
        pub fn session_checkpoint(&self) -> Result<Option<nanocodex_agent::SessionCheckpoint>> {
            let Some(checkpoint) = &self.checkpoint else {
                return Ok(None);
            };
            let record = &self.summary.record;
            match record.family() {
                HarnessFamily::Codex => {
                    let snapshot: nanocodex_agent::session::SessionSnapshot =
                        serde_json::from_value(checkpoint.clone())
                            .map_err(Error::InvalidPayload)?;
                    nanocodex_agent::SessionCheckpoint::codex(
                        record.session_id.clone(),
                        record.lineage.clone(),
                        record.model.default_thinking(),
                        snapshot,
                    )
                    .map(Some)
                    .map_err(|error| Error::InvalidState(error.to_string()))
                }
                #[cfg(feature = "claude")]
                HarnessFamily::Claude => nanocodex_claude::session_checkpoint(
                    &record.session_id,
                    record.lineage.clone(),
                    checkpoint.clone(),
                )
                .map(Some)
                .map_err(|error| Error::InvalidState(error.to_string())),
                #[cfg(not(feature = "claude"))]
                HarnessFamily::Claude => Err(Error::InvalidState(
                    "portable Claude checkpoints require the claude feature of nanocodex-durability".into(),
                )),
            }
        }
    }

    /// Where a branch starts.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum BranchPoint {
        /// The latest committed checkpoint.
        Latest,
        /// The checkpoint committed by this settled turn, keeping it.
        Through(String),
        /// The checkpoint immediately before this turn, dropping it and later turns.
        Before(String),
    }

    /// Family-neutral catalog and factory of durable local sessions.
    ///
    /// Clones share one serialized store connection. Reads never acquire an
    /// owner; [`Self::session`], [`Self::resume`] and [`Self::branch`] use the
    /// normal fencing owner protocol for the state they write.
    #[derive(Clone)]
    pub struct SessionStore {
        store: SharedStore,
    }

    impl SessionStore {
        /// Wraps any host store that supports non-fencing reads.
        ///
        /// # Errors
        ///
        /// Returns [`Error::RuntimeUnavailable`] outside a Tokio runtime.
        pub fn new<S: StateStore + 'static>(store: S) -> Result<Self> {
            Ok(Self {
                store: SharedStore::new(store)?,
            })
        }

        /// Location of the one local session database under a Codex home.
        #[cfg(feature = "sqlite")]
        #[must_use]
        pub fn path(codex_home: &std::path::Path) -> PathBuf {
            codex_home.join("sessions.sqlite")
        }

        /// Opens (creating if needed) the local session database for every harness.
        ///
        /// # Errors
        ///
        /// Returns a store error when the database cannot be opened.
        #[cfg(feature = "sqlite")]
        pub fn open(codex_home: &std::path::Path) -> Result<Self> {
            std::fs::create_dir_all(codex_home).map_err(|error| {
                Error::Store(crate::StoreError::NotCommitted(format!(
                    "failed to create {}: {error}",
                    codex_home.display()
                )))
            })?;
            Self::open_path(Self::path(codex_home))
        }

        /// Opens a session database at an explicit path, such as a legacy store.
        ///
        /// # Errors
        ///
        /// Returns a store error when the database cannot be opened.
        #[cfg(feature = "sqlite")]
        pub fn open_path(path: impl AsRef<std::path::Path>) -> Result<Self> {
            Self::new(crate::SqliteStore::open(path)?)
        }

        /// Lists resumable sessions, most recently updated first.
        ///
        /// States without any committed turn or checkpoint, subagent journals,
        /// and unreadable states are skipped.
        ///
        /// # Errors
        ///
        /// Returns a store error when the identities cannot be listed.
        pub async fn list(&self) -> Result<Vec<SessionSummary>> {
            // Every state is considered: a creation-order window would hide an
            // old session that is still in use behind newer subagent journals
            // and other non-session states. Only the result is bounded.
            let ids = self.store.clone().list_states(usize::MAX).await?;
            let mut sessions = Vec::new();
            for id in ids {
                if is_child_journal(&id) {
                    continue;
                }
                if let Ok(Some(summary)) = self.summary(&id).await {
                    sessions.push(summary);
                }
            }
            sessions.sort_by_key(|session| std::cmp::Reverse(session.record.updated_at_ms));
            sessions.truncate(LIST_LIMIT);
            Ok(sessions)
        }

        /// Reads one session's metadata, or `None` when it is not a resumable session.
        ///
        /// # Errors
        ///
        /// Returns a store or decoding error.
        pub async fn summary(&self, id: &str) -> Result<Option<SessionSummary>> {
            let Some(state) = self.peek(id).await? else {
                return Ok(None);
            };
            if state.operations().is_empty() && state.latest_checkpoint().is_none() {
                return Ok(None);
            }
            let preview = self.first_prompt(id, &state).await;
            let record = match state.session() {
                Some(record) => record.clone(),
                None => {
                    let Some(checkpoint) = self.native_checkpoint(id, &state).await? else {
                        return Ok(None);
                    };
                    infer_record(id, &checkpoint)?
                }
            };
            Ok(Some(SessionSummary { record, preview }))
        }

        /// Loads one session's metadata, visible transcript, turns, and checkpoint.
        ///
        /// # Errors
        ///
        /// Returns [`Error::SessionNotFound`] for an unknown session.
        pub async fn load(&self, id: &str) -> Result<StoredSession> {
            let state = self.require(id).await?;
            let checkpoint = self.native_checkpoint(id, &state).await?;
            let record = match (state.session(), &checkpoint) {
                (Some(record), _) => record.clone(),
                (None, Some(checkpoint)) => infer_record(id, checkpoint)?,
                (None, None) => {
                    return Err(Error::SessionNotFound {
                        session_id: id.to_owned(),
                    });
                }
            };
            let transcript = checkpoint.as_ref().map(transcript).unwrap_or_default();
            let turns = self.state_turns(id, &state).await?;
            let preview = turns
                .iter()
                .find_map(|turn| turn.preview.clone())
                .or_else(|| {
                    transcript.iter().find_map(|item| match item {
                        TranscriptItem::User(text) => Some(text.clone()),
                        _ => None,
                    })
                });
            Ok(StoredSession {
                summary: SessionSummary { record, preview },
                transcript,
                turns,
                checkpoint,
            })
        }

        /// Lists retained user turns in submission order, for rewind previews.
        ///
        /// # Errors
        ///
        /// Returns [`Error::SessionNotFound`] for an unknown session.
        pub async fn turns(&self, id: &str) -> Result<Vec<StoredTurn>> {
            let state = self.require(id).await?;
            self.state_turns(id, &state).await
        }

        /// Opens or creates a session, records its metadata, and returns the
        /// owned state to attach with [`crate::DurableAgentExt::durability`].
        ///
        /// # Errors
        ///
        /// Returns a store error, or an error when another agent owns the state.
        pub async fn session(&self, record: SessionRecord) -> Result<DurableSession> {
            let session =
                DurableSession::open_shared(self.store.clone(), record.session_id.clone(), None)
                    .await?;
            session.describe(record).await?;
            Ok(session)
        }

        /// Reopens an existing session, keeping its recorded metadata.
        ///
        /// # Errors
        ///
        /// Returns [`Error::SessionNotFound`] for an unknown session.
        pub async fn resume(&self, id: &str) -> Result<DurableSession> {
            let state = self.require(id).await?;
            let session =
                DurableSession::open_shared(self.store.clone(), id.to_owned(), None).await?;
            if state.session().is_none()
                && let Some(checkpoint) = self.native_checkpoint(id, &state).await?
            {
                // Upgrade older journals so later listing needs no inference.
                session.describe(infer_record(id, &checkpoint)?).await?;
            }
            Ok(session)
        }

        /// Publishes a new resumable session starting at a stored boundary.
        ///
        /// The source is never modified or fenced. The branch records
        /// [`Origin::Branch`] provenance from the source.
        ///
        /// # Errors
        ///
        /// Returns [`Error::SessionNotFound`] or [`Error::InvalidState`] when
        /// the selected boundary is unavailable.
        pub async fn branch(
            &self,
            id: &str,
            at: BranchPoint,
            workspace: Option<PathBuf>,
        ) -> Result<SessionSummary> {
            self.branch_with(id, at, workspace, |selected, _| Ok(selected))
                .await
        }

        /// Like [`Self::branch`], letting the caller transform the selected
        /// provider-native checkpoint before publication (for example, a
        /// provider-specific rewind truncation). The transform receives the
        /// selected checkpoint (`None` when branching before the first turn)
        /// and the source's latest checkpoint, and returns the branch's
        /// starting checkpoint (`None` for an empty conversation).
        ///
        /// # Errors
        ///
        /// Returns the transform's error, or the errors of [`Self::branch`].
        pub async fn branch_with(
            &self,
            id: &str,
            at: BranchPoint,
            workspace: Option<PathBuf>,
            transform: impl FnOnce(Option<Value>, Option<&Value>) -> Result<Option<Value>>,
        ) -> Result<SessionSummary> {
            let state = self.require(id).await?;
            let mut record = match state.session() {
                Some(record) => record.clone(),
                None => {
                    let checkpoint =
                        self.native_checkpoint(id, &state).await?.ok_or_else(|| {
                            Error::InvalidState(
                                "source session has no checkpoint to branch from".into(),
                            )
                        })?;
                    infer_record(id, &checkpoint)?
                }
            };
            let selected = match &at {
                BranchPoint::Latest => state.latest_checkpoint().cloned(),
                BranchPoint::Through(turn) => {
                    let operation = state.operation(turn).ok_or_else(unknown_turn)?;
                    Some(
                        settled_checkpoint(&operation.status)
                            .cloned()
                            .ok_or_else(|| {
                                Error::InvalidState(
                                    "the selected turn has no settled checkpoint".into(),
                                )
                            })?,
                    )
                }
                BranchPoint::Before(turn) => {
                    let selected = state.operation(turn).ok_or_else(unknown_turn)?;
                    let prior = state
                        .operations()
                        .values()
                        .filter(|op| op.accepted_order < selected.accepted_order)
                        .max_by_key(|op| op.accepted_order);
                    // Without a prior retained turn the selected turn was the
                    // first one, unless retention may have removed older turns.
                    // Journals without a record fall back to their first write.
                    let complete_history = state
                        .session()
                        .map_or(selected.accepted_order == 1, |record| {
                            !record.history_pruned
                        });
                    match prior {
                        None if complete_history => None,
                        Some(prior) => {
                            Some(settled_checkpoint(&prior.status).cloned().ok_or_else(|| {
                                Error::InvalidState(
                                    "checkpoint before the selected turn is unavailable".into(),
                                )
                            })?)
                        }
                        None => {
                            return Err(Error::InvalidState(
                                "retained history before the selected turn was pruned".into(),
                            ));
                        }
                    }
                }
            };
            let branch_id = uuid::Uuid::now_v7().to_string();
            let selected = match selected {
                Some(payload) => Some(self.hydrate(id, &payload).await?),
                None => None,
            };
            let latest = self.native_checkpoint(id, &state).await?;
            let mut native = transform(selected, latest.as_ref())?;
            if let (Some(workspace), Some(Value::Object(object))) = (&workspace, native.as_mut())
                && object.contains_key("workspace")
            {
                object.insert(
                    "workspace".into(),
                    Value::String(workspace.display().to_string()),
                );
            }
            record = record.derive(branch_id, Origin::Branch);
            if workspace.is_some() {
                record.workspace = workspace;
            }
            let checkpoint = native
                .map(|value| encode_native(record.family(), value))
                .transpose()?;
            self.publish(record.clone(), checkpoint).await?;
            let preview = match &at {
                BranchPoint::Before(_) => None,
                _ => self.first_prompt(id, &state).await,
            };
            Ok(SessionSummary { record, preview })
        }

        async fn publish(
            &self,
            record: SessionRecord,
            checkpoint: Option<EncodedPayload>,
        ) -> Result<()> {
            let mut store = self.store.clone();
            let id = record.session_id.clone();
            let target = store.acquire(&id, OwnerId::new()).await?;
            if target.state.revision != 0 || target.state.payload.is_some() {
                return Err(Error::InvalidState(
                    "new branch identity unexpectedly exists".into(),
                ));
            }
            let mut state = DurableState::default();
            match checkpoint {
                Some(checkpoint) => {
                    state.apply_transition(1, Transition::CheckpointCommitted { checkpoint })?;
                }
                None => state.advance_revision(1)?,
            }
            state.set_session(Some(record));
            let records = state.stage_records();
            store
                .replace(
                    &id,
                    &target.owner,
                    0,
                    &state.checkpoint_payload()?,
                    &records,
                )
                .await?;
            Ok(())
        }

        async fn peek(&self, id: &str) -> Result<Option<DurableState>> {
            let stored = self.store.clone().peek(id).await?;
            if stored.payload.is_none() {
                return Ok(None);
            }
            reduce_peeked(stored).map(Some)
        }

        async fn require(&self, id: &str) -> Result<DurableState> {
            self.peek(id).await?.ok_or_else(|| Error::SessionNotFound {
                session_id: id.to_owned(),
            })
        }

        async fn load_payload(&self, id: &str, payload: &EncodedPayload) -> Result<EncodedPayload> {
            payload.load(&mut self.store.clone(), id).await
        }

        /// Loads the provider-native JSON of a stored checkpoint, hydrating
        /// Codex context pages into one complete session snapshot.
        async fn hydrate(&self, id: &str, payload: &EncodedPayload) -> Result<Value> {
            let value: Value = self.load_payload(id, payload).await?.decode()?;
            if is_claude(&value) {
                return Ok(value);
            }
            let saved: crate::context::Snapshot =
                serde_json::from_value(value).map_err(Error::InvalidPayload)?;
            let snapshot = crate::context::restore_snapshot(
                crate::context::Reader::Store(&self.store, id),
                saved,
            )
            .await?;
            serde_json::to_value(snapshot).map_err(Error::InvalidPayload)
        }

        async fn native_checkpoint(&self, id: &str, state: &DurableState) -> Result<Option<Value>> {
            match state.latest_checkpoint() {
                Some(payload) => self.hydrate(id, payload).await.map(Some),
                None => Ok(None),
            }
        }

        async fn state_turns(&self, id: &str, state: &DurableState) -> Result<Vec<StoredTurn>> {
            let mut operations: Vec<_> = state.operations().iter().collect();
            operations.sort_by_key(|(_, op)| op.accepted_order);
            let mut turns = Vec::new();
            for (turn, operation) in operations {
                let input: Value = self.load_payload(id, &operation.input).await?.decode()?;
                if input
                    .get("kind")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind != "prompt")
                {
                    continue;
                }
                turns.push(StoredTurn {
                    id: turn.clone(),
                    preview: prompt_text(&input),
                    status: match operation.status {
                        OperationStatus::Pending => TurnStatus::Pending,
                        OperationStatus::Completed { .. } => TurnStatus::Completed,
                        OperationStatus::Failed { .. } => TurnStatus::Failed,
                        OperationStatus::Cancelled { .. } => TurnStatus::Cancelled,
                    },
                    input,
                });
            }
            Ok(turns)
        }

        async fn first_prompt(&self, id: &str, state: &DurableState) -> Option<String> {
            let (_, first) = state
                .operations()
                .iter()
                .min_by_key(|(_, op)| op.accepted_order)?;
            let input: Value = self
                .load_payload(id, &first.input)
                .await
                .ok()?
                .decode()
                .ok()?;
            prompt_text(&input)
        }
    }

    fn unknown_turn() -> Error {
        Error::InvalidState("unknown or expired turn".into())
    }

    const fn settled_checkpoint(status: &OperationStatus) -> Option<&EncodedPayload> {
        match status {
            OperationStatus::Completed { checkpoint, .. }
            | OperationStatus::Failed { checkpoint, .. }
            | OperationStatus::Cancelled {
                checkpoint: Some(checkpoint),
            } => Some(checkpoint),
            _ => None,
        }
    }

    /// Re-encodes a hydrated native checkpoint for a new state of `family`.
    fn encode_native(family: HarnessFamily, value: Value) -> Result<EncodedPayload> {
        match family {
            HarnessFamily::Claude => EncodedPayload::encode(&value),
            HarnessFamily::Codex => {
                let snapshot: nanocodex_agent::session::SessionSnapshot =
                    serde_json::from_value(value).map_err(Error::InvalidPayload)?;
                Ok(crate::context::prepare_snapshot(snapshot, &HashSet::new())?.payload)
            }
        }
    }

    /// Durable task trees stored beside their root session, never sessions.
    fn is_child_journal(id: &str) -> bool {
        id.ends_with(":subagents")
    }

    fn is_claude(value: &Value) -> bool {
        value.get("provider").and_then(Value::as_str) == Some("claude")
    }

    /// Recovers metadata for journals written before records existed.
    fn infer_record(id: &str, checkpoint: &Value) -> Result<SessionRecord> {
        let model = checkpoint
            .get("model")
            .and_then(Value::as_str)
            .and_then(|model| model.parse::<HarnessModel>().ok())
            .ok_or_else(|| Error::InvalidState("stored checkpoint has no known model".into()))?;
        if is_claude(checkpoint) != (model.family() == HarnessFamily::Claude) {
            return Err(Error::InvalidState(
                "stored checkpoint model belongs to another family".into(),
            ));
        }
        let workspace = checkpoint
            .get("workspace")
            .and_then(Value::as_str)
            .filter(|workspace| !workspace.is_empty())
            .map(PathBuf::from);
        Ok(SessionRecord {
            session_id: id.to_owned(),
            model,
            workspace,
            title: None,
            lineage: Lineage::root(id),
            created_at_ms: 0,
            updated_at_ms: 0,
            history_pruned: false,
        })
    }

    /// First user-visible text of a stored prompt input.
    fn prompt_text(input: &Value) -> Option<String> {
        fn find(value: &Value, depth: usize) -> Option<&str> {
            if depth > 8 {
                return None;
            }
            match value {
                Value::Object(object) => {
                    for key in ["instruction", "text", "prompt", "message"] {
                        if let Some(text) = object.get(key).and_then(Value::as_str)
                            && !text.trim().is_empty()
                        {
                            return Some(text);
                        }
                    }
                    object.values().find_map(|value| find(value, depth + 1))
                }
                Value::Array(items) => items.iter().find_map(|value| find(value, depth + 1)),
                _ => None,
            }
        }
        find(input, 0).map(|text| text.chars().take(500).collect())
    }

    /// Projects a provider-native checkpoint into the shared visible transcript.
    /// Signed thinking, images, and other binary payloads are never exposed.
    fn transcript(checkpoint: &Value) -> Vec<TranscriptItem> {
        if is_claude(checkpoint) {
            claude_transcript(checkpoint)
        } else {
            codex_transcript(checkpoint)
        }
    }

    fn claude_transcript(checkpoint: &Value) -> Vec<TranscriptItem> {
        let mut items = Vec::new();
        if let Some(summary) = checkpoint["conversation"]["summary"]
            .as_str()
            .filter(|summary| !summary.is_empty())
        {
            items.push(TranscriptItem::Assistant(format!(
                "Retained conversation summary:\n{summary}"
            )));
        }
        for message in checkpoint["conversation"]["messages"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let assistant = message["role"].as_str() == Some("assistant");
            for block in message["content"].as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(text) = block["text"].as_str() {
                            items.push(if assistant {
                                TranscriptItem::Assistant(text.into())
                            } else {
                                TranscriptItem::User(text.into())
                            });
                        }
                    }
                    // Visible thinking only; signatures and redacted thinking
                    // stay model-bound, like Codex encrypted reasoning.
                    Some("thinking") => {
                        if let Some(text) =
                            block["thinking"].as_str().filter(|text| !text.trim().is_empty())
                        {
                            items.push(TranscriptItem::Reasoning(text.into()));
                        }
                    }
                    Some("tool_use" | "server_tool_use") => items.push(TranscriptItem::Tool {
                        call_id: block["id"].as_str().unwrap_or_default().into(),
                        name: block["name"].as_str().unwrap_or_default().into(),
                        arguments: block["input"].to_string(),
                    }),
                    Some("mcp_tool_use") => items.push(TranscriptItem::Tool {
                        call_id: block["id"].as_str().unwrap_or_default().into(),
                        name: format!(
                            "mcp__{}__{}",
                            block["server_name"].as_str().unwrap_or_default(),
                            block["name"].as_str().unwrap_or_default()
                        ),
                        arguments: block["input"].to_string(),
                    }),
                    _ => {}
                }
            }
        }
        items
    }

    fn codex_transcript(snapshot: &Value) -> Vec<TranscriptItem> {
        let mut items = Vec::new();
        for item in snapshot["history"].as_array().into_iter().flatten() {
            match item["type"].as_str() {
                Some("message") => {
                    let role = item["role"].as_str();
                    for content in item["content"].as_array().into_iter().flatten() {
                        let Some(text) = content["text"].as_str().filter(|text| !text.is_empty())
                        else {
                            continue;
                        };
                        match (role, content["type"].as_str()) {
                            (Some("user"), Some("input_text")) if !is_injected_context(text) => {
                                items.push(TranscriptItem::User(text.into()));
                            }
                            (Some("assistant"), _) => {
                                items.push(TranscriptItem::Assistant(text.into()));
                            }
                            _ => {}
                        }
                    }
                }
                Some("reasoning") => {
                    for summary in item["summary"].as_array().into_iter().flatten() {
                        if let Some(text) = summary["text"].as_str().filter(|text| !text.is_empty())
                        {
                            items.push(TranscriptItem::Reasoning(text.into()));
                        }
                    }
                }
                Some("function_call") => items.push(TranscriptItem::Tool {
                    call_id: item["call_id"].as_str().unwrap_or_default().into(),
                    name: item["name"].as_str().unwrap_or_default().into(),
                    arguments: item["arguments"].as_str().unwrap_or_default().into(),
                }),
                Some("custom_tool_call") => items.push(TranscriptItem::Tool {
                    call_id: item["call_id"].as_str().unwrap_or_default().into(),
                    name: item["name"].as_str().unwrap_or_default().into(),
                    arguments: item["input"].as_str().unwrap_or_default().into(),
                }),
                _ => {}
            }
        }
        items
    }

    /// Harness-injected user-role context, such as environment or AGENTS.md blocks.
    fn is_injected_context(text: &str) -> bool {
        let text = text.trim_start();
        text.starts_with("<environment_context")
            || text.starts_with("<user_instructions")
            || text.starts_with("# AGENTS.md")
    }
}
