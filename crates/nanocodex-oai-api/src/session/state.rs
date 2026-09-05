use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{ResponseItem, Usage, responses::ResponseHistory};

use super::{
    compaction,
    context::{ContextManager, assign_missing_response_item_ids, has_well_formed_tool_calls},
};

/// Stable client-owned identity for one managed conversation.
///
/// Session IDs are `UUIDv7` values so they remain globally unique while sorting
/// by creation time. They are not `OpenAI` response IDs and are safe to persist
/// as application lineage.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[repr(transparent)]
#[serde(try_from = "uuid::Uuid", into = "uuid::Uuid")]
pub struct SessionId(uuid::Uuid);

impl SessionId {
    /// Generates a new `UUIDv7` session identity.
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }

    /// Returns the UUID representation.
    #[must_use]
    pub const fn as_uuid(self) -> uuid::Uuid {
        self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("SessionId").field(&self.0).finish()
    }
}

impl std::str::FromStr for SessionId {
    type Err = SessionIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(uuid::Uuid::parse_str(value)?)
    }
}

impl TryFrom<uuid::Uuid> for SessionId {
    type Error = SessionIdError;

    fn try_from(value: uuid::Uuid) -> Result<Self, Self::Error> {
        if value.get_version_num() != 7 {
            return Err(SessionIdError::WrongVersion {
                version: value.get_version_num(),
            });
        }
        Ok(Self(value))
    }
}

impl From<SessionId> for uuid::Uuid {
    fn from(value: SessionId) -> Self {
        value.0
    }
}

/// Invalid persisted or caller-supplied session identity.
#[derive(Debug, thiserror::Error)]
pub enum SessionIdError {
    /// The value was not a UUID.
    #[error("invalid session UUID")]
    InvalidUuid(#[from] uuid::Error),
    /// The UUID used a version other than `UUIDv7`.
    #[error("session IDs must be UUIDv7, got UUIDv{version}")]
    WrongVersion {
        /// Parsed UUID version number.
        version: usize,
    },
}

/// OAI-owned mutable state shared by the standalone session and agent loop.
///
/// This is a lower-layer integration surface for `nanocodex-agent`. Normal
/// API consumers should use [`Session`], which prevents invalid mutations.
#[doc(hidden)]
#[derive(Clone)]
pub struct ManagedSessionState {
    context: ContextManager,
    delta_start: usize,
    previous_response_id: Option<String>,
    history_revision: u64,
    server_reasoning_included: bool,
}

impl ManagedSessionState {
    /// Creates fresh continuation state around uncommitted typed input.
    #[must_use]
    pub fn new(mut items: Vec<ResponseItem>) -> Self {
        assign_missing_response_item_ids(&mut items);
        Self {
            context: ContextManager::new(items),
            delta_start: 0,
            previous_response_id: None,
            history_revision: 0,
            server_reasoning_included: false,
        }
    }

    /// Restores complete committed history without trusting a provider
    /// continuation checkpoint.
    ///
    /// The next request performs a full replay. History must be non-empty,
    /// contain only supported API items, and have complete ordered tool-call
    /// pairs.
    ///
    /// # Errors
    ///
    /// Returns a typed structural error when the retained history is empty,
    /// contains unsupported items, or has malformed tool-call pairing.
    pub fn resume(mut items: Vec<ResponseItem>) -> Result<Self, ManagedSessionStateError> {
        if items.is_empty() {
            return Err(ManagedSessionStateError::EmptyHistory);
        }
        assign_missing_response_item_ids(&mut items);
        if !has_well_formed_tool_calls(&items) {
            return Err(ManagedSessionStateError::MalformedToolCalls);
        }
        let history_len = items.len();
        let mut state = Self::new(items);
        if state.context.len() != history_len {
            return Err(ManagedSessionStateError::UnsupportedHistoryItem);
        }
        state.context.commit_tail();
        state.delta_start = state.context.len();
        Ok(state)
    }

    /// Returns the number of retained typed history items.
    #[must_use]
    pub fn history_len(&self) -> usize {
        self.context.len()
    }

    /// Returns whether retained typed history is empty.
    #[must_use]
    pub fn history_is_empty(&self) -> bool {
        self.context.is_empty()
    }

    /// Iterates over retained typed history in provider order.
    #[must_use]
    pub fn history(&self) -> impl ExactSizeIterator<Item = &ResponseItem> {
        self.context.iter()
    }

    /// Materializes complete retained typed history.
    #[must_use]
    pub fn flattened_history(&self) -> Vec<ResponseItem> {
        self.context.flattened_items()
    }

    /// Returns an O(1) shared checkpoint of retained typed history.
    #[must_use]
    pub fn shared_history(&self) -> ResponseHistory {
        self.context.shared_items()
    }

    /// Returns request-ready history, repairing incomplete tool-call pairs in
    /// an isolated copy only when required.
    #[must_use]
    pub fn prompt_history(&self) -> ResponseHistory {
        self.context.prompt_items()
    }

    /// Returns request-ready history and whether it differs from retained
    /// authoritative history because incomplete tool calls were repaired.
    ///
    /// Higher orchestration layers use the flag to force a full replay and
    /// adopt the repaired baseline only after provider completion.
    #[doc(hidden)]
    #[must_use]
    pub fn prompt_history_with_repair(&self) -> (ResponseHistory, bool) {
        self.context.prompt_items_with_repair()
    }

    /// Replaces authoritative history with a successfully replayed repaired
    /// prompt baseline.
    #[doc(hidden)]
    pub fn adopt_prompt_history(&mut self, history: ResponseHistory) {
        self.context.adopt_prompt_items(history);
    }

    /// Appends client- or provider-authored typed items to the active tail.
    ///
    /// Unsupported non-API items are ignored and bounded tool-output policy is
    /// applied by the underlying context manager.
    pub fn append(&mut self, items: impl IntoIterator<Item = ResponseItem>) {
        self.context.record_items(items);
    }

    /// Records usage from the most recent completed provider operation.
    pub fn update_token_info(&mut self, usage: Option<&Usage>) {
        self.context.update_token_info(usage);
    }

    /// Records whether the active transport includes retained reasoning in
    /// provider-reported input usage.
    pub const fn observe_server_reasoning(&mut self, included: bool) {
        self.server_reasoning_included |= included;
    }

    /// Returns the best available estimate of active provider context tokens.
    #[must_use]
    pub fn active_context_tokens(&self) -> u64 {
        self.context
            .active_context_tokens(self.server_reasoning_included)
    }

    /// Returns the first history index not retained by the current provider
    /// continuation checkpoint.
    #[must_use]
    pub const fn delta_start(&self) -> usize {
        self.delta_start
    }

    /// Returns the private provider continuation checkpoint, when healthy.
    ///
    /// Higher layers use this only to construct requests and compatibility
    /// telemetry; it must not become an application-owned session identity.
    #[must_use]
    pub fn previous_response_id(&self) -> Option<&str> {
        self.previous_response_id.as_deref()
    }

    /// Installs a completed provider continuation checkpoint.
    pub fn set_previous_response_id(&mut self, response_id: impl Into<String>) {
        let response_id = response_id.into();
        self.previous_response_id = (!response_id.is_empty()).then_some(response_id);
    }

    /// Excludes all currently retained items from the next healthy
    /// continuation delta.
    pub fn clear_delta(&mut self) {
        self.delta_start = self.context.len();
    }

    /// Discards the provider checkpoint so the next request replays complete
    /// client-owned history.
    pub fn reset_for_full_request(&mut self) {
        self.delta_start = 0;
        self.previous_response_id = None;
    }

    /// Commits the active tail after a completed provider response.
    ///
    /// A provider continuation ID is required so a healthy next request cannot
    /// accidentally omit committed history.
    ///
    /// # Errors
    ///
    /// Returns [`ManagedSessionStateError::MissingResponseId`] when no
    /// completed provider continuation has been installed.
    pub fn commit(&mut self) -> Result<(), ManagedSessionStateError> {
        if self.previous_response_id.is_none() {
            return Err(ManagedSessionStateError::MissingResponseId);
        }
        self.context.commit_tail();
        self.delta_start = self.context.len();
        Ok(())
    }

    /// Commits repaired client-authored cancellation state and forces the next
    /// request to replay all retained history.
    pub fn commit_interrupted(&mut self) {
        self.reset_for_full_request();
        self.context.commit_tail();
    }

    /// Replaces image inputs after the provider rejects their encoded data.
    ///
    /// The returned count is the number of image parts replaced with a stable
    /// text diagnostic so a later full replay cannot resend poisoned bytes.
    #[doc(hidden)]
    pub fn replace_rejected_images(&mut self) -> usize {
        self.context.replace_rejected_images()
    }

    /// Commits the active tail without changing continuation state.
    ///
    /// The agent uses this only when publishing a safe in-turn fork boundary.
    pub fn commit_tail(&mut self) {
        self.context.commit_tail();
    }

    /// Installs one completed compaction item atomically and forces a full
    /// replay on the next request.
    ///
    /// `initial_context` contains caller-owned canonical items that must
    /// survive summarization, such as an agent's developer and task context.
    pub fn install_compaction(
        &mut self,
        item: ResponseItem,
        initial_context: impl IntoIterator<Item = ResponseItem>,
        request_prefix: &[ResponseItem],
    ) {
        let initial_context = initial_context.into_iter().collect::<Vec<_>>();
        let history =
            compaction::install_history(&self.context.flattened_items(), &initial_context, item);
        self.context.replace_and_recompute(history, request_prefix);
        self.reset_for_full_request();
        self.history_revision = self.history_revision.saturating_add(1);
    }

    /// Returns the monotonic number of installed history replacements.
    #[must_use]
    pub const fn history_revision(&self) -> u64 {
        self.history_revision
    }
}

/// Invalid state supplied to or produced by the managed session state engine.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ManagedSessionStateError {
    /// Restored history was empty.
    #[error("conversation history must not be empty")]
    EmptyHistory,
    /// Restored history contained an unsupported item.
    #[error("conversation history contains an unsupported item")]
    UnsupportedHistoryItem,
    /// Restored history contained an unmatched or misordered tool call.
    #[error("conversation history contains an unmatched or misordered tool call")]
    MalformedToolCalls,
    /// A completed response was missing its provider continuation identity.
    #[error("completed response did not have a response ID")]
    MissingResponseId,
}

#[cfg(test)]
mod tests {
    use super::{ManagedSessionState, ManagedSessionStateError};

    #[test]
    fn empty_response_id_cannot_commit_an_incremental_checkpoint() {
        let mut state = ManagedSessionState::new(Vec::new());
        state.set_previous_response_id("");

        assert!(matches!(
            state.commit(),
            Err(ManagedSessionStateError::MissingResponseId)
        ));
        assert_eq!(state.previous_response_id(), None);
    }
}
