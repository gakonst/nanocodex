use std::fmt;
#[cfg(feature = "openai")]
use std::sync::Arc;

use nanocodex_oai_api::responses::{ResponseItem, Usage};
#[cfg(feature = "openai")]
use nanocodex_oai_api::{Model, responses::MessageRole};

#[cfg(feature = "openai")]
pub use nanocodex_oai_api::session::SessionId;

#[cfg(feature = "openai")]
use crate::{NanocodexError, Result, model::run::ModelCheckpoint};

#[cfg(feature = "openai")]
const SESSION_SNAPSHOT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(crate) struct ContextSnapshot {
    pub(crate) agents_md: Option<AgentsMdSnapshot>,
    pub(crate) environment: Option<EnvironmentSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", content = "snapshot", rename_all = "snake_case")]
pub(crate) enum ContextBaseline {
    Missing,
    Known(ContextSnapshot),
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(crate) struct AgentsMdSnapshot {
    pub(crate) directory: String,
    pub(crate) text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(crate) struct EnvironmentSnapshot {
    pub(crate) cwd: String,
    pub(crate) shell: String,
    pub(crate) current_date: String,
    pub(crate) timezone: String,
}

/// One immutable model boundary shared by forks, durable snapshots, and rollout projection.
#[derive(Clone)]
#[cfg(feature = "openai")]
pub(crate) struct CommittedSession {
    lineage_id: Arc<str>,
    selected_model: Model,
    model: ModelCheckpoint,
    // Runtime preparation may normalize context IDs, images, and request prefix.
    // Until another boundary is committed, snapshot the retained boundary exactly.
    retained_snapshot: Option<SessionSnapshot>,
}

#[cfg(feature = "openai")]
impl CommittedSession {
    pub(crate) const fn new(
        lineage_id: Arc<str>,
        selected_model: Model,
        model: ModelCheckpoint,
    ) -> Self {
        Self {
            lineage_id,
            selected_model,
            model,
            retained_snapshot: None,
        }
    }

    pub(crate) fn with_retained_snapshot(mut self, snapshot: Option<SessionSnapshot>) -> Self {
        self.retained_snapshot = snapshot;
        self
    }

    pub(crate) fn lineage_id(&self) -> &str {
        &self.lineage_id
    }

    pub(crate) const fn model(&self) -> &ModelCheckpoint {
        &self.model
    }

    #[cfg(all(feature = "openai", not(target_family = "wasm")))]
    pub(crate) const fn selected_model(&self) -> Model {
        self.selected_model
    }

    #[allow(dead_code, reason = "consumed by the native rollout boundary only")]
    pub(crate) fn rollout_history(&self) -> nanocodex_oai_api::responses::ResponseHistory {
        self.model.history()
    }

    #[allow(dead_code, reason = "consumed by the native rollout boundary only")]
    pub(crate) const fn history_revision(&self) -> u64 {
        self.model.history_revision()
    }

    #[cfg(all(feature = "openai", not(target_family = "wasm")))]
    pub(crate) const fn context_baseline(&self) -> &ContextBaseline {
        self.model.context_baseline()
    }

    pub(crate) fn snapshot(&self) -> SessionSnapshot {
        if let Some(snapshot) = &self.retained_snapshot {
            return snapshot.clone();
        }
        SessionSnapshot {
            version: SESSION_SNAPSHOT_VERSION,
            model: self.selected_model.as_str().to_owned(),
            lineage_id: self.lineage_id.to_string(),
            prompt_cache_key: self.model.prompt_cache_key().to_owned(),
            workspace: self.model.workspace().to_owned(),
            base_instructions: None,
            request_prefix: Some(self.model.request_prefix().to_vec()),
            canonical_context: self.model.canonical_context().clone(),
            history: self.model.snapshot_history(),
            client_authored: self.model.client_authored().clone(),
            context_snapshot: Some(self.model.context_baseline().clone()),
            context_usage: Some(self.model.context_usage()),
        }
    }
}

/// Accounting basis for exactly the history retained at a durable boundary.
#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct ContextUsage {
    pub(crate) usage: Option<Usage>,
    #[serde(default)]
    pub(crate) server_reasoning_included: bool,
    #[serde(default)]
    pub(crate) is_estimate: bool,
}

/// Versioned, serializable state for resuming a completed session boundary.
///
/// Its fields are intentionally private: callers may persist or transfer the
/// value, but Nanocodex remains responsible for interpreting model history and
/// cache state. Provider response IDs are deliberately excluded: the first
/// resumed request replays the authoritative typed history, then subsequent
/// requests follow the configured history policy. The stored request prefix
/// records the completed boundary; a resumed runtime replaces it with its
/// current instructions and tool definitions while retaining conversation
/// history and cache lineage.
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
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    client_authored: std::collections::BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_snapshot: Option<ContextBaseline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_usage: Option<ContextUsage>,
}

/// Session metadata separated from independently persisted conversation items.
#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct SessionSnapshotHead(SessionSnapshot);

impl SessionSnapshotHead {
    /// Reassembles the exact snapshot with its stored context records.
    #[must_use]
    pub fn with_context(
        mut self,
        history: Vec<ResponseItem>,
        prefix: Option<Vec<ResponseItem>>,
    ) -> SessionSnapshot {
        self.0.history = history;
        self.0.request_prefix = prefix;
        self.0
    }
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
    /// Separates metadata from conversation bodies for record-based persistence.
    #[must_use]
    pub fn into_context_parts(
        mut self,
    ) -> (
        SessionSnapshotHead,
        Vec<ResponseItem>,
        Option<Vec<ResponseItem>>,
    ) {
        let history = std::mem::take(&mut self.history);
        let prefix = self.request_prefix.take();
        (SessionSnapshotHead(self), history, prefix)
    }

    #[cfg(all(feature = "openai", not(target_family = "wasm")))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_rollout(
        model: Model,
        thread_id: String,
        prompt_cache_key: String,
        workspace: String,
        base_instructions: Option<String>,
        history: Vec<ResponseItem>,
        client_authored: std::collections::BTreeSet<String>,
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
            model: model.as_str().to_owned(),
            lineage_id: thread_id,
            prompt_cache_key,
            workspace,
            base_instructions,
            request_prefix: None,
            canonical_context,
            history,
            client_authored,
            context_snapshot,
            context_usage: None,
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

    #[cfg(feature = "openai")]
    pub(crate) fn into_resume(self) -> Result<SessionResume> {
        if self.version != SESSION_SNAPSHOT_VERSION {
            return Err(NanocodexError::InvalidSessionSnapshot(format!(
                "unsupported format version {}; expected {SESSION_SNAPSHOT_VERSION}",
                self.version
            )));
        }
        let model = self.model.parse::<Model>().map_err(|error| {
            NanocodexError::InvalidSessionSnapshot(format!(
                "snapshot model is unsupported: {error}"
            ))
        })?;
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
                let mut checkpoint = ModelCheckpoint::resume(
                    self.workspace.clone(),
                    Arc::clone(&lineage_id),
                    request_prefix,
                    Arc::clone(&prompt_cache_key),
                    self.canonical_context.clone(),
                    self.history.clone(),
                    self.client_authored.clone(),
                    None,
                    self.context_snapshot.clone(),
                )?;
                if let Some(usage) = self.context_usage.as_ref() {
                    checkpoint.restore_context_usage(usage);
                }
                Ok::<_, NanocodexError>(checkpoint)
            })
            .transpose()?;
        Ok(SessionResume {
            model,
            lineage_id,
            prompt_cache_key,
            workspace: self.workspace,
            canonical_context: self.canonical_context,
            history: self.history,
            client_authored: self.client_authored,
            context_baseline: self.context_snapshot,
            checkpoint,
        })
    }
}

#[cfg(feature = "openai")]
pub(crate) struct SessionResume {
    pub(crate) model: Model,
    pub(crate) lineage_id: Arc<str>,
    pub(crate) prompt_cache_key: Arc<str>,
    pub(crate) workspace: String,
    pub(crate) canonical_context: ResponseItem,
    pub(crate) history: Vec<ResponseItem>,
    pub(crate) client_authored: std::collections::BTreeSet<String>,
    pub(crate) context_baseline: Option<ContextBaseline>,
    pub(crate) checkpoint: Option<ModelCheckpoint>,
}
