use std::{collections::BTreeSet, fmt};

use serde::{Deserialize, Serialize};

use crate::{ResponseItem, ResponsesError, Usage, responses::ResponseHistory};

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
    client_authored: BTreeSet<String>,
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
            client_authored: BTreeSet::new(),
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
        if state.context.replace_invalid_tool_images() > 0 {
            state.history_revision = state.history_revision.saturating_add(1);
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

    /// Appends client- or provider-authored typed items to the active tail.
    ///
    /// Unsupported non-API items are ignored and bounded tool-output policy is
    /// applied by the underlying context manager.
    pub fn append(&mut self, items: impl IntoIterator<Item = ResponseItem>) {
        self.context.record_items(items);
    }

    /// Records explicitly client-authored input, preserving developer provenance
    /// separately from the provider-visible response items.
    pub fn append_client(&mut self, items: impl IntoIterator<Item = ResponseItem>) {
        let mut items: Vec<_> = items.into_iter().collect();
        assign_missing_response_item_ids(&mut items);
        self.client_authored.extend(items.iter().filter_map(|item| {
            matches!(
                item,
                ResponseItem::Message {
                    role: crate::MessageRole::Developer,
                    ..
                }
            )
            .then(|| item.id().map(ToString::to_string))
            .flatten()
        }));
        self.append(items);
    }

    /// Client provenance sidecar for durable snapshots. Never send it to the model.
    #[must_use]
    pub const fn client_authored(&self) -> &BTreeSet<String> {
        &self.client_authored
    }

    /// Restores explicit provenance. Legacy histories without this sidecar have
    /// no client-authored developer messages; text is never used to infer origin.
    pub fn restore_client_authored(&mut self, ids: BTreeSet<String>) {
        self.client_authored = self
            .context
            .iter()
            .filter_map(|item| {
                let id = item.id()?;
                (matches!(
                    item,
                    ResponseItem::Message {
                        role: crate::MessageRole::Developer,
                        ..
                    }
                ) && ids.contains(id.as_str()))
                .then(|| id.to_string())
            })
            .collect();
    }

    /// Usage baseline needed to preserve compaction decisions across recovery.
    #[doc(hidden)]
    #[must_use]
    pub const fn context_usage(&self) -> (Option<&Usage>, bool) {
        (
            self.context.last_token_usage.as_ref(),
            self.server_reasoning_included,
        )
    }

    /// Whether the saved usage covers every retained item, including local input.
    #[doc(hidden)]
    #[must_use]
    pub const fn context_usage_is_estimate(&self) -> bool {
        self.context.token_usage_is_estimate
    }

    /// Restores accounting for unchanged durable history. Image repairs retain
    /// their newly computed baseline instead of reinstalling stale usage.
    #[doc(hidden)]
    pub fn restore_context_usage(
        &mut self,
        usage: Option<&Usage>,
        server_reasoning_included: bool,
        is_estimate: bool,
    ) {
        self.observe_server_reasoning(server_reasoning_included);
        if self.history_revision == 0 {
            self.context.update_token_info(usage);
            self.context.token_usage_is_estimate = usage.is_some() && is_estimate;
        }
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

    /// Derives the next `response.create` request from retained history.
    ///
    /// A repaired prompt (incomplete tool calls closed in an isolated copy)
    /// cannot continue the provider checkpoint and is sent as a full replay.
    #[must_use]
    pub fn generation_request(&self) -> RequestHistory {
        let (full, repaired) = self.context.prompt_items_with_repair();
        RequestHistory {
            full,
            incremental: self.context.shared_items(),
            incremental_start: self.delta_start,
            previous_response_id: self.previous_response_id.clone().filter(|_| !repaired),
            repaired,
        }
    }

    /// Records one completed generation derived from [`Self::generation_request`].
    ///
    /// `response_id` is `None` when the provider checkpoint cannot be reused
    /// (for example, a journal-replayed result from a replaced transport); the
    /// next request then replays complete history. The active tail is not
    /// committed.
    ///
    /// # Errors
    ///
    /// Returns [`ManagedSessionStateError::MissingResponseId`] without
    /// changing state when a reusable continuation has an empty ID.
    pub fn complete_generation(
        &mut self,
        request: RequestHistory,
        response_id: Option<String>,
        output: impl IntoIterator<Item = ResponseItem>,
        usage: Option<&Usage>,
        server_reasoning_included: bool,
    ) -> Result<(), ManagedSessionStateError> {
        if response_id.as_deref().is_some_and(str::is_empty) {
            return Err(ManagedSessionStateError::MissingResponseId);
        }
        self.observe_server_reasoning(server_reasoning_included);
        if request.repaired {
            self.context.adopt_prompt_items(request.full);
        }
        self.update_token_info(usage);
        match response_id {
            Some(response_id) => self.previous_response_id = Some(response_id),
            None => self.reset_for_full_request(),
        }
        self.append(output);
        Ok(())
    }

    /// Derives the next `response.compact` request from retained history.
    ///
    /// Callers may inspect [`RequestHistory::full`] (for example, to preserve
    /// the transcript) before [`RequestHistory::fit_context_window`] trims it.
    #[must_use]
    pub fn compaction_request(&self) -> RequestHistory {
        let (full, repaired) = self.context.prompt_items_with_repair();
        RequestHistory {
            incremental: full.clone(),
            full,
            incremental_start: if repaired { 0 } else { self.delta_start },
            previous_response_id: self.previous_response_id.clone().filter(|_| !repaired),
            repaired,
        }
    }

    /// Applies the client-owned history repair a provider rejection requires.
    ///
    /// Rejected image payloads are replaced with a stable text diagnostic and
    /// a rejected tool definition is removed from discovery metadata, so a
    /// later full replay cannot resend them. Any change discards the provider
    /// checkpoint. Returns whether retained history changed.
    pub fn repair_rejected_request(&mut self, repair: RejectedRequestRepair<'_>) -> bool {
        let mut changed = 0;
        if repair.replace_images {
            changed += self.context.replace_rejected_images();
        }
        if let Some(definition) = repair.tool_definition {
            changed += self.context.remove_tool_definition(definition);
        }
        if changed > 0 {
            self.reset_for_full_request();
            self.history_revision = self.history_revision.saturating_add(1);
        }
        changed > 0
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
        let history = compaction::install_history_with_provenance(
            &self.context.flattened_items(),
            &initial_context,
            item,
            &self.client_authored,
        );
        self.context.replace_and_recompute(history, request_prefix);
        let provenance = std::mem::take(&mut self.client_authored);
        self.restore_client_authored(provenance);
        self.reset_for_full_request();
        self.history_revision = self.history_revision.saturating_add(1);
    }

    /// Installs a summary of an immutable prefix followed by its complete tail.
    ///
    /// The caller verifies that `cutoff` is still the current conversation prefix.
    /// Only that prefix participates in retained-message pruning; `tail` is
    /// appended unchanged after the summary and becomes the full-replay baseline.
    pub fn install_compaction_with_tail(
        &mut self,
        item: ResponseItem,
        cutoff: &[ResponseItem],
        tail: impl IntoIterator<Item = ResponseItem>,
        request_prefix: &[ResponseItem],
    ) {
        let mut history =
            compaction::install_history_with_provenance(cutoff, &[], item, &self.client_authored);
        history.extend(tail);
        self.context.replace_and_recompute(history, request_prefix);
        let provenance = std::mem::take(&mut self.client_authored);
        self.restore_client_authored(provenance);
        self.reset_for_full_request();
        self.history_revision = self.history_revision.saturating_add(1);
    }

    /// Installs image-prepared replay history and marks its durable baseline changed.
    #[doc(hidden)]
    pub fn replace_prepared_history(&mut self, history: Vec<ResponseItem>) {
        self.context.replace_and_recompute(history, &[]);
        self.context.commit_tail();
        self.reset_for_full_request();
        self.history_revision = self.history_revision.saturating_add(1);
    }

    /// Returns the monotonic number of installed history replacements.
    #[must_use]
    pub const fn history_revision(&self) -> u64 {
        self.history_revision
    }
}

/// Continuation-aware history for one replayable provider request.
///
/// Produced by [`ManagedSessionState`] so every driver derives the full replay,
/// the incremental delta, and the provider checkpoint from one policy.
#[doc(hidden)]
#[derive(Clone)]
pub struct RequestHistory {
    pub(crate) full: ResponseHistory,
    pub(crate) incremental: ResponseHistory,
    pub(crate) incremental_start: usize,
    pub(crate) previous_response_id: Option<String>,
    pub(crate) repaired: bool,
}

impl RequestHistory {
    /// Returns the complete request-ready history.
    #[must_use]
    pub const fn full(&self) -> &ResponseHistory {
        &self.full
    }

    /// Returns the provider checkpoint this request continues, if any.
    #[must_use]
    pub fn previous_response_id(&self) -> Option<&str> {
        self.previous_response_id.as_deref()
    }

    /// Trims oversized tool outputs so a compaction request fits the context
    /// window. Rewritten history no longer matches the provider checkpoint, so
    /// any rewrite turns the request into a full replay.
    pub fn fit_context_window(
        &mut self,
        request_prefix: &[ResponseItem],
        context_window_tokens: u64,
    ) {
        let rewritten = compaction::trim_tool_outputs_to_fit_context_window(
            &mut self.full,
            request_prefix,
            context_window_tokens,
        );
        if rewritten > 0 {
            self.incremental = self.full.clone();
            self.incremental_start = 0;
            self.previous_response_id = None;
        }
    }
}

/// Client-owned history repair required after a provider rejected a request.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RejectedRequestRepair<'a> {
    /// Replace every retained image payload with a text diagnostic.
    pub replace_images: bool,
    /// Remove this rejected definition from retained discovery metadata.
    pub tool_definition: Option<&'a serde_json::Value>,
}

impl<'a> RejectedRequestRepair<'a> {
    /// Derives the repair required by one provider error.
    #[must_use]
    pub fn for_error(error: &'a ResponsesError) -> Self {
        Self {
            replace_images: matches!(error, ResponsesError::InvalidImageRequest { .. }),
            tool_definition: error.invalid_tool_schema(),
        }
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
    use serde_json::json;

    use super::{ManagedSessionState, ManagedSessionStateError};

    #[test]
    fn developer_provenance_survives_restore_and_compaction_without_leaking_to_wire() {
        use crate::{ContentItem, MessageRole, ResponseItem};
        let dev = |text: &str| {
            ResponseItem::message(MessageRole::Developer, [ContentItem::input_text(text)])
        };
        let mut state = ManagedSessionState::new(vec![dev("generated context")]);
        state.append_client([dev("client instructions")]);
        let ids = state.client_authored().clone();
        assert_eq!(ids.len(), 1);
        let wire = serde_json::to_string(&state.flattened_history()).unwrap();
        assert!(!wire.contains("client_authored"));
        let mut restored =
            ManagedSessionState::resume(serde_json::from_str(&wire).unwrap()).unwrap();
        assert!(restored.client_authored().is_empty());
        restored.restore_client_authored(ids.clone());
        restored.install_compaction(
            serde_json::from_value(json!({"type":"compaction", "encrypted_content":"opaque"}))
                .unwrap(),
            [],
            &[],
        );
        assert_eq!(restored.client_authored(), &ids);
        let history = serde_json::to_string(&restored.flattened_history()).unwrap();
        assert!(history.contains("client instructions"));
        assert!(!history.contains("generated context"));
    }

    #[test]
    fn removing_a_tool_definition_preserves_transcript_and_corrected_schemas() {
        let rejected = json!({
            "type": "function", "name": "lookup", "parameters": {
                "type": "object", "properties": { "limit": { "type": "integer" } },
                "required": []
            }
        });
        let mut corrected = rejected.clone();
        corrected["parameters"]["required"] = json!(["limit"]);
        let sibling = json!({
            "type": "function", "name": "other",
            "parameters": { "type": "object", "tools": [rejected] }
        });
        let mut state = ManagedSessionState::new(
            serde_json::from_value(json!([
                { "type": "message", "role": "user", "content": [] },
                { "type": "tool_search_call", "id": "tsc-original", "call_id": "search",
                    "execution": "client", "arguments": { "query": "lookup" } },
                { "type": "tool_search_output", "id": "tso-original", "call_id": "search",
                    "status": "completed", "execution": "client", "tools": [
                        rejected, corrected, sibling,
                        { "type": "namespace", "name": "keep", "description": "Keep metadata",
                            "tools": [rejected, corrected] },
                        { "type": "namespace", "name": "prune", "tools": [
                            { "type": "namespace", "name": "nested", "tools": [rejected] }
                        ]}
                    ] },
                { "type": "function_call", "call_id": "completed", "name": "other",
                    "arguments": "{}" },
            { "type": "function_call_output", "call_id": "completed", "output": "already done" },
            { "type": "tool_search_call", "call_id": "duplicate", "execution": "client",
                "arguments": { "query": "lookup" } },
            { "type": "tool_search_output", "call_id": "duplicate", "execution": "client",
                "status": "completed", "tools": [rejected] }
            ]))
            .unwrap(),
        );
        state.set_previous_response_id("resp-before");
        state.commit().unwrap();
        let mut expected = serde_json::to_value(state.flattened_history()).unwrap();
        expected[2]["tools"] = json!([
            corrected, sibling,
            { "type": "namespace", "name": "keep", "description": "Keep metadata",
                "tools": [corrected] }
        ]);

        expected[6]["tools"] = json!([]);
        let repair = super::RejectedRequestRepair {
            tool_definition: Some(&rejected),
            ..Default::default()
        };
        assert!(state.repair_rejected_request(repair));
        assert_eq!(
            serde_json::to_value(state.flattened_history()).unwrap(),
            expected
        );
        assert_eq!(state.previous_response_id(), None);
        assert_eq!(state.delta_start(), 0);
        assert_eq!(state.history_revision(), 1);
        assert!(!state.generation_request().repaired);

        state.set_previous_response_id("resp-after");
        state.commit().unwrap();
        assert!(!state.repair_rejected_request(repair));
        assert_eq!(state.previous_response_id(), Some("resp-after"));
        assert_eq!(state.delta_start(), state.history_len());
        assert_eq!(state.history_revision(), 1);
    }

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
