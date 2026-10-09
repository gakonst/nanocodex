//! Harness-neutral session contract shared by every agent backend.
//!
//! Codex and Claude sessions expose the same identity, provenance, checkpoint,
//! fork, capability, and persistence model. Only a checkpoint's opaque payload
//! is provider-specific; it is produced and decoded by the owning backend.

use std::{any::Any, fmt, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::{HarnessFamily, HarnessModel, NanocodexError, Result, Thinking, TurnResult};

/// Stable identity and provenance of one session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Stable session identity used by events, persistence, and resume.
    pub session_id: String,
    /// Native agent-loop family that owns the conversation.
    pub family: HarnessFamily,
    /// Where this session came from.
    pub lineage: Lineage,
}

/// How a session relates to the conversation tree it belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lineage {
    /// Root session of this conversation tree; equal to the session for a root.
    pub root_session_id: String,
    /// Directly preceding session, when this session was derived from another.
    pub parent_session_id: Option<String>,
    /// How this session was created.
    pub origin: Origin,
    /// Distance from the root session.
    pub depth: u32,
}

impl Lineage {
    /// Lineage of a fresh root session.
    #[must_use]
    pub fn root(session_id: impl Into<String>) -> Self {
        Self {
            root_session_id: session_id.into(),
            parent_session_id: None,
            origin: Origin::Root,
            depth: 0,
        }
    }

    /// Lineage of a session derived from `parent` with the given origin.
    #[must_use]
    pub fn child_of(parent: &Self, parent_session_id: impl Into<String>, origin: Origin) -> Self {
        Self {
            root_session_id: parent.root_session_id.clone(),
            parent_session_id: Some(parent_session_id.into()),
            origin,
            depth: parent.depth.saturating_add(1),
        }
    }
}

/// How a session was created.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// A session started directly by a host.
    Root,
    /// A conversation copy that continues independently.
    Fork,
    /// An ephemeral side exploration such as `/btw`.
    SideConversation,
    /// A clean child started by a parent agent.
    Subagent,
    /// A persisted history branch created from a stored session.
    Branch,
}

const CHECKPOINT_FORMAT: &str = "nanocodex-session-checkpoint/1";

/// Portable, versioned conversation boundary for any harness.
///
/// A checkpoint is the one value used to resume a session, fork at an exact
/// boundary, and evict or restore subagents. It contains the complete
/// unredacted model-visible conversation and must be protected accordingly.
/// The payload is opaque; only the backend family that produced it decodes it.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCheckpoint {
    format: String,
    session_id: String,
    model: HarnessModel,
    thinking: Thinking,
    lineage: Lineage,
    /// Conversation-tree identity used to reject checkpoints from another tree.
    conversation_id: String,
    turn_id: Option<String>,
    has_conversation: bool,
    payload: serde_json::Value,
}

impl SessionCheckpoint {
    /// Builds a checkpoint from backend-native state.
    #[doc(hidden)]
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn native(
        session_id: impl Into<String>,
        model: HarnessModel,
        thinking: Thinking,
        lineage: Lineage,
        conversation_id: impl Into<String>,
        turn_id: Option<String>,
        has_conversation: bool,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            format: CHECKPOINT_FORMAT.to_owned(),
            session_id: session_id.into(),
            model,
            thinking,
            lineage,
            conversation_id: conversation_id.into(),
            turn_id,
            has_conversation,
            payload,
        }
    }

    /// Parses and validates a serialized checkpoint.
    ///
    /// ```
    /// # use nanocodex_agent::{Nanocodex, Result, SessionCheckpoint};
    /// # async fn save_and_reload(agent: &Nanocodex) -> Result<()> {
    /// let json = agent.checkpoint().await?.to_json()?;
    /// // Persist `json` as a secret: it holds the unredacted conversation.
    /// let checkpoint = SessionCheckpoint::from_json(&json)?;
    /// assert_eq!(checkpoint.session_id(), agent.session_id());
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::InvalidCheckpoint`] for malformed JSON or
    /// when [`Self::validate`] rejects the decoded checkpoint.
    pub fn from_json(json: &str) -> Result<Self> {
        let checkpoint: Self = serde_json::from_str(json)
            .map_err(|error| NanocodexError::InvalidCheckpoint(error.to_string()))?;
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    /// Serializes this checkpoint, including its unredacted conversation.
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::InvalidCheckpoint`] only if the native
    /// payload cannot be encoded.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|error| NanocodexError::InvalidCheckpoint(error.to_string()))
    }

    /// Checks the harness-neutral invariants every backend relies on.
    ///
    /// A valid checkpoint has the supported format, non-empty session and
    /// conversation identities, a thinking level its pinned model supports,
    /// a non-empty turn identity when one is recorded, and self-consistent
    /// lineage (a root has no parent and depth 0; a derived session has a
    /// parent and depth of at least 1).
    ///
    /// The opaque payload is decoded, and its family checked, only by the
    /// receiving backend: restoring another family's checkpoint fails with
    /// [`NanocodexError::CheckpointFamilyMismatch`] (see
    /// [`Self::require_family`]), and a malformed payload with
    /// [`NanocodexError::InvalidCheckpoint`].
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::InvalidCheckpoint`] naming the first
    /// violated invariant.
    pub fn validate(&self) -> Result<()> {
        let invalid = |reason: String| Err(NanocodexError::InvalidCheckpoint(reason));
        if self.format != CHECKPOINT_FORMAT {
            return invalid(format!(
                "unsupported session checkpoint format {:?}; expected {CHECKPOINT_FORMAT:?}",
                self.format
            ));
        }
        if self.session_id.trim().is_empty() || self.conversation_id.trim().is_empty() {
            return invalid("session and conversation identities must not be empty".into());
        }
        if self
            .turn_id
            .as_deref()
            .is_some_and(|turn| turn.trim().is_empty())
        {
            return invalid("recorded turn identity must not be empty".into());
        }
        if !self.model.supports_thinking(self.thinking) {
            return invalid(format!(
                "model {} does not support {} thinking",
                self.model,
                self.thinking.as_str()
            ));
        }
        let lineage = &self.lineage;
        if lineage.root_session_id.trim().is_empty() {
            return invalid("lineage root session identity must not be empty".into());
        }
        let derived = lineage.parent_session_id.is_some();
        if (lineage.origin == Origin::Root) == derived || derived != (lineage.depth > 0) {
            return invalid(format!(
                "inconsistent {:?} lineage: parent {:?} at depth {}",
                lineage.origin, lineage.parent_session_id, lineage.depth
            ));
        }
        Ok(())
    }

    /// Session that produced this checkpoint.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    /// Family that owns, and alone can decode, this checkpoint.
    #[must_use]
    pub const fn family(&self) -> HarnessFamily {
        self.model.family()
    }
    /// Model pinned at this boundary.
    #[must_use]
    pub const fn model(&self) -> HarnessModel {
        self.model
    }
    /// Reasoning effort pinned at this boundary.
    #[must_use]
    pub const fn thinking(&self) -> Thinking {
        self.thinking
    }
    /// Provenance of the session that produced this checkpoint.
    #[must_use]
    pub const fn lineage(&self) -> &Lineage {
        &self.lineage
    }
    /// Conversation-tree identity shared by a session and its forks.
    #[must_use]
    pub fn conversation_id(&self) -> &str {
        &self.conversation_id
    }
    /// Completed turn this boundary follows.
    ///
    /// Set for checkpoints materialized from [`TurnResult::checkpoint`] by
    /// every backend. `None` for [`crate::Nanocodex::checkpoint`], which
    /// copies the latest committed boundary without naming a turn, and for
    /// checkpoints taken before the first completed turn.
    #[must_use]
    pub fn turn_id(&self) -> Option<&str> {
        self.turn_id.as_deref()
    }
    /// Whether the boundary contains at least one committed exchange.
    #[must_use]
    pub const fn has_conversation(&self) -> bool {
        self.has_conversation
    }
    /// Backend-native state, decoded only by the producing family.
    #[doc(hidden)]
    #[must_use]
    pub const fn payload(&self) -> &serde_json::Value {
        &self.payload
    }
    /// Consumes the checkpoint and returns its backend-native state.
    #[doc(hidden)]
    #[must_use]
    pub fn into_payload(self) -> serde_json::Value {
        self.payload
    }
    /// Rebinds this checkpoint to another session identity and lineage.
    #[doc(hidden)]
    #[must_use]
    pub fn with_identity(mut self, session_id: impl Into<String>, lineage: Lineage) -> Self {
        self.session_id = session_id.into();
        self.lineage = lineage;
        self
    }
    /// Records the completed turn this boundary follows.
    #[doc(hidden)]
    #[must_use]
    pub fn with_turn_id(mut self, turn_id: Option<String>) -> Self {
        self.turn_id = turn_id;
        self
    }

    /// Rejects a checkpoint owned by another family.
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::CheckpointFamilyMismatch`] when the families differ.
    pub fn require_family(&self, family: HarnessFamily) -> Result<()> {
        if self.family() == family {
            Ok(())
        } else {
            Err(NanocodexError::CheckpointFamilyMismatch {
                expected: family,
                found: self.family(),
            })
        }
    }
}

impl fmt::Debug for SessionCheckpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionCheckpoint")
            .field("session_id", &self.session_id)
            .field("model", &self.model)
            .field("lineage", &self.lineage)
            .field("turn_id", &self.turn_id)
            .field("has_conversation", &self.has_conversation)
            .finish_non_exhaustive()
    }
}

/// A backend's retained boundary after one completed turn.
///
/// Backends keep their live, zero-copy state here so forking at a recent turn
/// preserves transport and cache continuity; a portable [`SessionCheckpoint`]
/// is materialized only on request.
/// Encodes retained live state as a portable checkpoint.
type Materialize = dyn Fn(&(dyn Any + Send + Sync)) -> Result<SessionCheckpoint> + Send + Sync;

#[derive(Clone)]
#[doc(hidden)]
pub struct TurnBoundary {
    live: Arc<dyn Any + Send + Sync>,
    materialize: Arc<Materialize>,
}

impl TurnBoundary {
    /// Retains backend-native live state and how to make it portable.
    #[must_use]
    pub fn live<T, F>(state: Arc<T>, materialize: F) -> Self
    where
        T: Any + Send + Sync,
        F: Fn(&T) -> Result<SessionCheckpoint> + Send + Sync + 'static,
    {
        Self {
            live: state,
            materialize: Arc::new(move |state| {
                let state = state
                    .downcast_ref::<T>()
                    .expect("turn boundary state type is fixed at construction");
                materialize(state)
            }),
        }
    }

    /// Retains an already portable checkpoint, such as a replayed durable result.
    #[must_use]
    pub fn portable(checkpoint: SessionCheckpoint) -> Self {
        Self::live(Arc::new(checkpoint), |checkpoint: &SessionCheckpoint| {
            Ok(checkpoint.clone())
        })
    }

    /// Returns the live state when it has the requested backend type.
    #[must_use]
    pub fn downcast<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        Arc::clone(&self.live).downcast::<T>().ok()
    }

    /// Whether the live state has the requested backend type.
    #[must_use]
    pub fn is<T: Any + Send + Sync>(&self) -> bool {
        self.live.is::<T>()
    }

    /// Materializes the portable checkpoint for this boundary.
    ///
    /// # Errors
    ///
    /// Returns the backend's encoding failure.
    pub fn checkpoint(&self) -> Result<SessionCheckpoint> {
        (self.materialize)(self.live.as_ref())
    }
}

/// Which boundary a fork starts from.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ForkPoint {
    /// The latest committed safe boundary. Never waits for an active turn.
    Latest,
    /// The boundary retained by one completed turn of this conversation.
    Turn(TurnResult),
    /// A portable checkpoint of this conversation tree.
    Checkpoint(SessionCheckpoint),
}

/// One fork request accepted by every backend.
#[derive(Clone, Debug)]
pub struct ForkRequest {
    point: ForkPoint,
    origin: Origin,
}

impl ForkRequest {
    /// Forks from the latest committed safe boundary.
    #[must_use]
    pub const fn latest() -> Self {
        Self {
            point: ForkPoint::Latest,
            origin: Origin::Fork,
        }
    }

    /// Forks from the boundary retained by one completed turn.
    #[must_use]
    pub fn at_turn(result: &TurnResult) -> Self {
        Self {
            point: ForkPoint::Turn(result.clone()),
            origin: Origin::Fork,
        }
    }

    /// Forks from a portable checkpoint of this conversation tree.
    #[must_use]
    pub const fn at(checkpoint: SessionCheckpoint) -> Self {
        Self {
            point: ForkPoint::Checkpoint(checkpoint),
            origin: Origin::Fork,
        }
    }

    /// Marks the child as an ephemeral side conversation.
    #[must_use]
    pub const fn side_conversation(mut self) -> Self {
        self.origin = Origin::SideConversation;
        self
    }

    /// Boundary the fork starts from.
    #[must_use]
    pub const fn point(&self) -> &ForkPoint {
        &self.point
    }

    /// Origin recorded on the child's lineage: `Fork` or `SideConversation`.
    #[must_use]
    pub const fn origin(&self) -> Origin {
        self.origin
    }

    /// Splits the request for backend dispatch.
    #[doc(hidden)]
    #[must_use]
    pub fn into_parts(self) -> (ForkPoint, Origin) {
        (self.point, self.origin)
    }
}

impl Default for ForkRequest {
    fn default() -> Self {
        Self::latest()
    }
}

/// When a session setting may change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mutability {
    /// The setting is fixed when the session is built.
    #[default]
    Fixed,
    /// The setting may change until the first prompt is accepted.
    BeforeFirstPrompt,
    /// The setting applies to every subsequently accepted turn.
    Anytime,
}

/// Lifecycle operations a backend supports, so hosts can gate commands up front.
///
/// Unsupported operations fail with [`NanocodexError::UnsupportedCapability`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// [`crate::Nanocodex::checkpoint`] and [`TurnResult::checkpoint`].
    pub checkpoint: bool,
    /// Forking from the latest boundary.
    pub fork: bool,
    /// Forking from a completed turn or a portable checkpoint.
    pub fork_at: bool,
    /// Forks marked as side conversations keep that provenance.
    pub side_conversation: bool,
    /// Spawning clean children.
    pub spawn: bool,
    /// Steering an active turn.
    pub steering: bool,
    /// Steering with caller identities that can later be withdrawn.
    pub identified_steering: bool,
    /// Manual compaction.
    pub compaction: bool,
    /// Appending developer context.
    pub developer_messages: bool,
    /// Reading model-visible context.
    pub context: bool,
    /// When the model may change.
    pub model: Mutability,
    /// When reasoning effort may change.
    pub thinking: Mutability,
    /// When the processing tier may change.
    pub service_tier: Mutability,
}

/// Where and how a session is persisted.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Persistence {
    /// Durable store state that is the session's source of truth, when any.
    pub durable_state_id: Option<String>,
    /// Codex-compatible JSONL rollout mirror, when recording.
    #[cfg(all(feature = "rollout", not(target_family = "wasm")))]
    pub rollout: Option<crate::rollout::RolloutInfo>,
}

impl Persistence {
    /// A session whose source of truth is the given durable store state.
    #[must_use]
    pub fn durable(state_id: impl Into<String>) -> Self {
        Self {
            durable_state_id: Some(state_id.into()),
            ..Self::default()
        }
    }

    /// A session recorded only as a Codex-compatible rollout.
    #[cfg(all(feature = "rollout", not(target_family = "wasm")))]
    #[must_use]
    pub fn recorded(rollout: crate::rollout::RolloutInfo) -> Self {
        Self::default().with_rollout(rollout)
    }

    /// Adds the Codex-compatible rollout mirror of this session.
    #[cfg(all(feature = "rollout", not(target_family = "wasm")))]
    #[must_use]
    pub fn with_rollout(mut self, rollout: crate::rollout::RolloutInfo) -> Self {
        self.rollout = Some(rollout);
        self
    }

    /// Whether another process can resume this session.
    #[must_use]
    pub const fn resumable(&self) -> bool {
        #[cfg(all(feature = "rollout", not(target_family = "wasm")))]
        if self.rollout.is_some() {
            return true;
        }
        self.durable_state_id.is_some()
    }
}
