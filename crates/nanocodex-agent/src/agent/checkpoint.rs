//! Codex encoding of the harness-neutral [`SessionCheckpoint`].

use super::*;
use crate::{HarnessFamily, HarnessModel};

/// Codex-native state carried in a checkpoint payload.
#[derive(serde::Serialize, serde::Deserialize)]
struct CodexPayload {
    /// Requested processing tier.
    #[serde(flatten, with = "crate::service_tier_serde")]
    service_tier: ServiceTier,
    /// Whether this session uses full-history HTTP independently of its parent.
    #[serde(default)]
    stateless_http: bool,
    /// Last safe conversation boundary; absent before the first model turn.
    conversation: Option<SessionSnapshot>,
}

/// One Codex driver's identity and residency state, decoded from or encoded
/// into a [`SessionCheckpoint`].
pub(super) struct ChildState {
    pub(super) session_id: String,
    pub(super) model: Model,
    pub(super) thinking: Thinking,
    pub(super) service_tier: ServiceTier,
    pub(super) stateless_http: bool,
    pub(super) conversation: Option<SessionSnapshot>,
    pub(super) lineage: Lineage,
    pub(super) conversation_id: Arc<str>,
}

impl ChildState {
    /// Decodes and validates a Codex checkpoint.
    pub(super) fn from_checkpoint(checkpoint: SessionCheckpoint) -> Result<Self> {
        checkpoint.validate()?;
        checkpoint.require_family(HarnessFamily::Codex)?;
        let HarnessModel::Codex(model) = checkpoint.model() else {
            return Err(NanocodexError::CheckpointFamilyMismatch {
                expected: HarnessFamily::Codex,
                found: checkpoint.family(),
            });
        };
        let session_id = checkpoint.session_id().to_owned();
        let thinking = checkpoint.thinking();
        let lineage = checkpoint.lineage().clone();
        let conversation_id = Arc::<str>::from(checkpoint.conversation_id());
        let has_conversation = checkpoint.has_conversation();
        let payload: CodexPayload = serde_json::from_value(checkpoint.into_payload())
            .map_err(|error| NanocodexError::InvalidSessionSnapshot(error.to_string()))?;
        if payload.conversation.is_some() != has_conversation {
            return Err(NanocodexError::InvalidSessionSnapshot(
                "checkpoint conversation flag does not match its payload".into(),
            ));
        }
        if payload
            .conversation
            .as_ref()
            .is_some_and(|conversation| conversation.lineage_id() != conversation_id.as_ref())
        {
            return Err(NanocodexError::InvalidSessionSnapshot(
                "checkpoint conversation belongs to another cache lineage".into(),
            ));
        }
        let state = Self {
            session_id,
            model,
            thinking,
            service_tier: payload.service_tier,
            stateless_http: payload.stateless_http,
            conversation: payload.conversation,
            lineage,
            conversation_id,
        };
        state.validate()?;
        Ok(state)
    }

    /// Validates stored identity, model policy, and the versioned conversation.
    pub(super) fn validate(&self) -> Result<()> {
        self.session_id.parse::<SessionId>().map_err(|error| {
            NanocodexError::InvalidSessionSnapshot(format!("invalid session ID: {error}"))
        })?;
        super::spawn::validate_model_thinking(self.model, self.thinking)?;
        if let Some(conversation) = &self.conversation {
            conversation.clone().into_resume()?;
        }
        Ok(())
    }

    /// Encodes this state as a portable checkpoint.
    pub(super) fn into_checkpoint(self) -> Result<SessionCheckpoint> {
        let has_conversation = self.conversation.is_some();
        let payload = serde_json::to_value(CodexPayload {
            service_tier: self.service_tier,
            stateless_http: self.stateless_http,
            conversation: self.conversation,
        })
        .map_err(|error| NanocodexError::InvalidSessionSnapshot(error.to_string()))?;
        Ok(SessionCheckpoint::native(
            self.session_id,
            HarnessModel::Codex(self.model),
            self.thinking,
            self.lineage,
            self.conversation_id.as_ref(),
            None,
            has_conversation,
            payload,
        ))
    }
}

impl SessionCheckpoint {
    /// Wraps a Codex-native session snapshot, such as one loaded from a
    /// durable store, as a portable checkpoint with the standard tier.
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::InvalidSessionSnapshot`] for an invalid
    /// session ID, model, or snapshot.
    #[doc(hidden)]
    pub fn codex(
        session_id: impl Into<String>,
        lineage: Lineage,
        thinking: Thinking,
        snapshot: SessionSnapshot,
    ) -> Result<Self> {
        let state = ChildState {
            session_id: session_id.into(),
            model: snapshot.model()?,
            thinking,
            service_tier: ServiceTier::Standard,
            stateless_http: false,
            lineage,
            conversation_id: Arc::from(snapshot.lineage_id()),
            conversation: Some(snapshot),
        };
        state.validate()?;
        state.into_checkpoint()
    }

    /// Decodes the pre-contract Codex child residency JSON (`session_id`,
    /// `model`, `thinking`, `service_tier` or `fast_mode`, `stateless_http`,
    /// `conversation`) stored by older durable journals.
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::InvalidSessionSnapshot`] when it is malformed.
    #[doc(hidden)]
    pub fn from_legacy_codex_child(value: serde_json::Value, lineage: Lineage) -> Result<Self> {
        #[derive(serde::Deserialize)]
        struct Legacy {
            session_id: String,
            model: Model,
            thinking: Thinking,
            #[serde(flatten)]
            payload: serde_json::Value,
        }
        let invalid =
            |error: serde_json::Error| NanocodexError::InvalidSessionSnapshot(error.to_string());
        let legacy: Legacy = serde_json::from_value(value).map_err(invalid)?;
        let payload: CodexPayload = serde_json::from_value(legacy.payload).map_err(invalid)?;
        let conversation_id = payload.conversation.as_ref().map_or_else(
            || Arc::from(legacy.session_id.as_str()),
            |conversation| Arc::from(conversation.lineage_id()),
        );
        let state = ChildState {
            session_id: legacy.session_id,
            model: legacy.model,
            thinking: legacy.thinking,
            service_tier: payload.service_tier,
            stateless_http: payload.stateless_http,
            conversation: payload.conversation,
            lineage,
            conversation_id,
        };
        state.validate()?;
        state.into_checkpoint()
    }

    /// Codex-native conversation carried by this checkpoint, when it has one.
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::CheckpointFamilyMismatch`] for another
    /// family's checkpoint, or [`NanocodexError::InvalidSessionSnapshot`]
    /// when the payload is malformed.
    #[doc(hidden)]
    pub fn codex_snapshot(&self) -> Result<Option<SessionSnapshot>> {
        Ok(ChildState::from_checkpoint(self.clone())?.conversation)
    }
}

/// Whether a driver uses full-history HTTP without stored responses.
pub(super) const fn is_stateless_http(config: &ModelConfig) -> bool {
    matches!(config.responses_transport, ResponsesTransport::Https)
        && matches!(config.responses_history, ResponsesHistory::FullReplay)
        && !config.store_responses
}

/// Identity a driver stamps onto every checkpoint it produces.
pub(super) struct CheckpointSource {
    session_id: Arc<str>,
    lineage: Lineage,
    conversation_id: Arc<str>,
    stateless_http: bool,
}

/// A durable policy-replayed boundary; portable but not a live fork point.
pub(super) struct ReplayedBoundary(SessionSnapshot);

impl CheckpointSource {
    pub(super) const fn new(
        session_id: Arc<str>,
        lineage: Lineage,
        conversation_id: Arc<str>,
        stateless_http: bool,
    ) -> Self {
        Self {
            session_id,
            lineage,
            conversation_id,
            stateless_http,
        }
    }

    /// Identity and lineage a fork of this session derives from.
    pub(super) fn child_info(&self, session_id: &SessionId, origin: crate::Origin) -> SessionInfo {
        SessionInfo {
            session_id: session_id.to_string(),
            family: HarnessFamily::Codex,
            lineage: Lineage::child_of(&self.lineage, self.session_id.as_ref(), origin),
        }
    }

    pub(super) fn conversation_id(&self) -> &str {
        &self.conversation_id
    }

    fn checkpoint(
        &self,
        model: Model,
        thinking: Thinking,
        service_tier: ServiceTier,
        conversation: Option<SessionSnapshot>,
    ) -> Result<SessionCheckpoint> {
        ChildState {
            session_id: self.session_id.to_string(),
            model,
            thinking,
            service_tier,
            stateless_http: self.stateless_http,
            conversation,
            lineage: self.lineage.clone(),
            conversation_id: Arc::clone(&self.conversation_id),
        }
        .into_checkpoint()
    }

    /// Retains a live committed boundary; materialized only on request.
    pub(super) fn live(
        self: &Arc<Self>,
        committed: Arc<CommittedSession>,
        thinking: Thinking,
        service_tier: ServiceTier,
    ) -> TurnBoundary {
        let source = Arc::clone(self);
        TurnBoundary::live(committed, move |committed: &CommittedSession| {
            source.checkpoint(
                committed.selected_model(),
                thinking,
                service_tier,
                Some(committed.snapshot()),
            )
        })
    }

    /// Retains a policy-replayed boundary that cannot seed a live fork.
    pub(super) fn replayed(
        self: &Arc<Self>,
        snapshot: SessionSnapshot,
        model: Model,
        thinking: Thinking,
        service_tier: ServiceTier,
    ) -> TurnBoundary {
        let source = Arc::clone(self);
        TurnBoundary::live(
            Arc::new(ReplayedBoundary(snapshot)),
            move |replayed: &ReplayedBoundary| {
                source.checkpoint(model, thinking, service_tier, Some(replayed.0.clone()))
            },
        )
    }
}
