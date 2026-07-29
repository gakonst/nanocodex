use std::{fmt, sync::Arc};

use nanocodex_oai_api::{
    MODEL,
    responses::{MessageRole, ResponseItem},
};

pub use nanocodex_oai_api::session::SessionId;

use crate::{
    NanocodexError, Result,
    model::{context::ContextBaseline, run::ModelCheckpoint},
};

const SESSION_SNAPSHOT_VERSION: u32 = 1;

/// One immutable model boundary shared by forks, durable snapshots, and rollout projection.
#[derive(Clone)]
pub(crate) struct CommittedSession {
    lineage_id: Arc<str>,
    model: ModelCheckpoint,
}

impl CommittedSession {
    pub(crate) const fn new(lineage_id: Arc<str>, model: ModelCheckpoint) -> Self {
        Self { lineage_id, model }
    }

    pub(crate) fn lineage_id(&self) -> &str {
        &self.lineage_id
    }

    pub(crate) const fn model(&self) -> &ModelCheckpoint {
        &self.model
    }

    #[allow(dead_code, reason = "consumed by the native durability boundary only")]
    pub(crate) fn rollout_history(&self) -> nanocodex_oai_api::responses::ResponseHistory {
        self.model.history()
    }

    #[allow(dead_code, reason = "consumed by the native durability boundary only")]
    pub(crate) const fn history_revision(&self) -> u64 {
        self.model.history_revision()
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) const fn context_baseline(&self) -> &ContextBaseline {
        self.model.context_baseline()
    }

    pub(crate) fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            version: SESSION_SNAPSHOT_VERSION,
            model: MODEL.to_owned(),
            lineage_id: self.lineage_id.to_string(),
            prompt_cache_key: self.model.prompt_cache_key().to_owned(),
            workspace: self.model.workspace().to_owned(),
            base_instructions: None,
            request_prefix: Some(self.model.request_prefix().to_vec()),
            canonical_context: self.model.canonical_context().clone(),
            history: self.model.snapshot_history(),
            context_snapshot: Some(self.model.context_baseline().clone()),
        }
    }
}

/// Versioned, serializable state for resuming a completed session boundary.
///
/// Its fields are intentionally private: callers may persist or transfer the
/// value, but Nanocodex remains responsible for interpreting model history and
/// cache state. Provider response IDs are deliberately excluded: the first
/// resumed request replays the authoritative typed history, then subsequent
/// requests follow the configured history policy. Resuming requires the same
/// model instructions and tool definitions used to create the snapshot.
#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct SessionSnapshot {
    version: u32,
    model: String,
    lineage_id: String,
    prompt_cache_key: String,
    workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_prefix: Option<Vec<ResponseItem>>,
    canonical_context: ResponseItem,
    history: Vec<ResponseItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_snapshot: Option<ContextBaseline>,
}

impl fmt::Debug for SessionSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionSnapshot")
            .field("version", &self.version)
            .field("model", &self.model)
            .field("history_items", &self.history.len())
            .finish_non_exhaustive()
    }
}

impl SessionSnapshot {
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn from_rollout(
        thread_id: String,
        workspace: String,
        base_instructions: Option<String>,
        history: Vec<ResponseItem>,
        context_snapshot: Option<ContextBaseline>,
    ) -> Result<Self> {
        let canonical_context = history
            .iter()
            .find(|item| item.is_user_message())
            .cloned()
            .ok_or_else(|| {
                NanocodexError::InvalidSessionSnapshot(
                    "rollout does not contain a user message".to_owned(),
                )
            })?;
        Ok(Self {
            version: SESSION_SNAPSHOT_VERSION,
            model: MODEL.to_owned(),
            lineage_id: thread_id.clone(),
            prompt_cache_key: thread_id,
            workspace,
            base_instructions,
            request_prefix: None,
            canonical_context,
            history,
            context_snapshot,
        })
    }

    /// Snapshot format version understood by this Nanocodex release.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// Returns the absolute workspace retained by this session boundary.
    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    pub(crate) fn into_resume(self) -> Result<SessionResume> {
        if self.version != SESSION_SNAPSHOT_VERSION {
            return Err(NanocodexError::InvalidSessionSnapshot(format!(
                "unsupported format version {}; expected {SESSION_SNAPSHOT_VERSION}",
                self.version
            )));
        }
        if self.model != MODEL {
            return Err(NanocodexError::InvalidSessionSnapshot(format!(
                "snapshot model {} is incompatible with {MODEL}",
                self.model
            )));
        }
        if self.lineage_id.trim().is_empty() {
            return Err(NanocodexError::InvalidSessionSnapshot(
                "cache lineage must not be empty".to_owned(),
            ));
        }
        if self.prompt_cache_key.trim().is_empty() {
            return Err(NanocodexError::InvalidSessionSnapshot(
                "prompt cache key must not be empty".to_owned(),
            ));
        }
        if self.workspace.trim().is_empty() {
            return Err(NanocodexError::InvalidSessionSnapshot(
                "workspace must not be empty".to_owned(),
            ));
        }
        if let Some(request_prefix) = self.request_prefix.as_ref()
            && !matches!(
                request_prefix.as_slice(),
                [
                    ResponseItem::AdditionalTools {
                        role: MessageRole::Developer,
                        ..
                    },
                    ResponseItem::Message {
                        role: MessageRole::Developer,
                        ..
                    }
                ]
            )
        {
            return Err(NanocodexError::InvalidSessionSnapshot(
                "request prefix does not match the supported model contract".to_owned(),
            ));
        }
        let lineage_id = Arc::<str>::from(self.lineage_id);
        let prompt_cache_key = Arc::<str>::from(self.prompt_cache_key);
        let checkpoint = self
            .request_prefix
            .map(|request_prefix| {
                ModelCheckpoint::resume(
                    self.workspace.clone(),
                    request_prefix,
                    Arc::clone(&prompt_cache_key),
                    self.canonical_context.clone(),
                    self.history.clone(),
                    None,
                    self.context_snapshot.clone(),
                )
            })
            .transpose()?;
        Ok(SessionResume {
            lineage_id,
            prompt_cache_key,
            workspace: self.workspace,
            base_instructions: self.base_instructions,
            canonical_context: self.canonical_context,
            history: self.history,
            context_baseline: self.context_snapshot,
            checkpoint,
        })
    }
}

pub(crate) struct SessionResume {
    pub(crate) lineage_id: Arc<str>,
    pub(crate) prompt_cache_key: Arc<str>,
    pub(crate) workspace: String,
    pub(crate) base_instructions: Option<String>,
    pub(crate) canonical_context: ResponseItem,
    pub(crate) history: Vec<ResponseItem>,
    pub(crate) context_baseline: Option<ContextBaseline>,
    pub(crate) checkpoint: Option<ModelCheckpoint>,
}
