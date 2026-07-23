use std::{fmt, sync::Arc};

use nanocodex_core::{MODEL, MessageRole, ResponseItem};

use crate::{NanocodexError, Result, model::agent::ModelCheckpoint};

const SESSION_SNAPSHOT_VERSION: u32 = 1;

/// Versioned, serializable state for resuming a completed session boundary.
///
/// Its fields are intentionally private: callers may persist or transfer the
/// value, but Nanocodex remains responsible for interpreting model history and
/// cache state. Provider response IDs are deliberately excluded: the first
/// resumed request replays the authoritative typed history, then subsequent
/// requests return to incremental continuation. Resuming requires the same
/// model instructions and tool definitions used to create the snapshot.
#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct SessionSnapshot {
    version: u32,
    model: String,
    lineage_id: String,
    prompt_cache_key: String,
    workspace: String,
    request_prefix: Vec<ResponseItem>,
    canonical_context: ResponseItem,
    history: Vec<ResponseItem>,
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
    /// Snapshot format version understood by this Nanocodex release.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    pub(crate) fn from_checkpoint(lineage_id: &str, checkpoint: &ModelCheckpoint) -> Self {
        Self {
            version: SESSION_SNAPSHOT_VERSION,
            model: MODEL.to_owned(),
            lineage_id: lineage_id.to_owned(),
            prompt_cache_key: checkpoint.prompt_cache_key().to_owned(),
            workspace: checkpoint.workspace().to_owned(),
            request_prefix: checkpoint.request_prefix().to_vec(),
            canonical_context: checkpoint.canonical_context().clone(),
            history: checkpoint.snapshot_history(),
        }
    }

    pub(crate) fn into_checkpoint(self) -> Result<(Arc<str>, Arc<str>, ModelCheckpoint)> {
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
        if self.request_prefix.is_empty() {
            return Err(NanocodexError::InvalidSessionSnapshot(
                "request prefix must not be empty".to_owned(),
            ));
        }
        if !matches!(
            self.request_prefix.as_slice(),
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
        ) {
            return Err(NanocodexError::InvalidSessionSnapshot(
                "request prefix does not match the supported model contract".to_owned(),
            ));
        }
        let lineage_id = Arc::<str>::from(self.lineage_id);
        let prompt_cache_key = Arc::<str>::from(self.prompt_cache_key);
        let checkpoint = ModelCheckpoint::resume(
            self.workspace,
            Arc::from(self.request_prefix),
            Arc::clone(&prompt_cache_key),
            self.canonical_context,
            self.history,
        )?;
        Ok((lineage_id, prompt_cache_key, checkpoint))
    }
}
