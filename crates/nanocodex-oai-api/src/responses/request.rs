//! Byte-stable request profiles, persistent history, and wire serialization.

use std::{borrow::Cow, collections::BTreeMap, sync::Arc};

use serde::{Serialize, Serializer, ser::SerializeSeq};

use super::ResponseItem;
use crate::{ModelConfig, Thinking};

/// Stable request metadata and prefix shared by every operation in a session.
#[derive(Clone)]
pub struct RequestProfile {
    session_id: String,
    thread_id: String,
    prompt_cache_key: String,
    prefix: Arc<[ResponseItem]>,
    code_mode_tool_names: Arc<BTreeMap<String, CodeModeToolName>>,
    retained_config: Option<RetainedRequestConfig>,
}

#[derive(Clone)]
struct RetainedRequestConfig {
    model_id_prefix: Option<String>,
    reasoning_mode: crate::ReasoningMode,
    store_responses: bool,
}

impl RequestProfile {
    /// Creates stable session metadata and an immutable request prefix.
    #[must_use]
    pub fn new(
        session_id: impl Into<String>,
        prompt_cache_key: impl Into<String>,
        prefix: Arc<[ResponseItem]>,
    ) -> Self {
        let session_id = session_id.into();
        Self {
            thread_id: session_id.clone(),
            session_id,
            prompt_cache_key: prompt_cache_key.into(),
            prefix,
            code_mode_tool_names: Arc::default(),
            retained_config: None,
        }
    }

    /// Returns the client-owned session identity used in request metadata.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Returns the current agent thread identity.
    #[must_use]
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// Uses a fresh thread identity while retaining the provider session.
    #[must_use]
    pub fn with_thread_id(mut self, thread_id: impl Into<String>) -> Self {
        self.thread_id = thread_id.into();
        self
    }

    /// Returns the stable prompt-cache identity.
    #[must_use]
    pub fn prompt_cache_key(&self) -> &str {
        &self.prompt_cache_key
    }

    /// Returns immutable instructions and tool definitions.
    #[must_use]
    pub fn prefix(&self) -> &[ResponseItem] {
        &self.prefix
    }

    /// Shares the byte-stable request prefix with an internal checkpoint.
    #[doc(hidden)]
    #[must_use]
    pub fn shared_prefix(&self) -> Arc<[ResponseItem]> {
        Arc::clone(&self.prefix)
    }

    pub(crate) fn with_request_content(
        mut self,
        prompt_cache_key: String,
        prefix: Arc<[ResponseItem]>,
        model_id_prefix: Option<String>,
        reasoning_mode: crate::ReasoningMode,
        store_responses: bool,
    ) -> Self {
        self.prompt_cache_key = prompt_cache_key;
        self.prefix = prefix;
        self.retained_config = Some(RetainedRequestConfig {
            model_id_prefix,
            reasoning_mode,
            store_responses,
        });
        self
    }

    pub(crate) fn with_code_mode_tool_names(
        mut self,
        names: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        self.code_mode_tool_names = Arc::new(
            names
                .into_iter()
                .map(|(identifier, name)| (identifier, CodeModeToolName::from_flat_name(name)))
                .collect(),
        );
        self
    }
}

#[derive(Serialize)]
struct CodeModeToolName {
    name: Box<str>,
    namespace: Option<Box<str>>,
}

impl CodeModeToolName {
    fn from_flat_name(name: String) -> Self {
        if let Some(namespaced) = name.strip_prefix("mcp__")
            && let Some((server, tool)) = namespaced.split_once("__")
        {
            return Self {
                name: tool.into(),
                namespace: Some(format!("mcp__{server}").into()),
            };
        }
        Self {
            name: name.into(),
            namespace: None,
        }
    }
}

/// Persistent, immutable-segment Responses history.
///
/// Cloning or checkpointing this value shares all committed segments. Only the
/// active tail is mutable, so a branch allocates for its own new items without
/// copying the retained prefix.
#[derive(Clone, Default)]
pub struct ResponseHistory {
    head: Option<Arc<HistorySegment>>,
    tail: Arc<Vec<ResponseItem>>,
}

struct HistorySegment {
    previous: Option<Arc<Self>>,
    items: Arc<Vec<ResponseItem>>,
    len: usize,
}

impl ResponseHistory {
    /// Creates history with one mutable tail and no committed segments.
    #[must_use]
    pub fn new(items: Vec<ResponseItem>) -> Self {
        Self {
            head: None,
            tail: Arc::new(items),
        }
    }

    /// Returns the total item count across committed segments and the tail.
    #[must_use]
    pub fn len(&self) -> usize {
        self.head.as_ref().map_or(0, |segment| segment.len) + self.tail.len()
    }

    /// Returns whether the history contains no items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the current uncommitted tail.
    #[must_use]
    pub fn tail(&self) -> &[ResponseItem] {
        &self.tail
    }

    /// Shares the current tail allocation.
    #[must_use]
    pub fn shared_tail(&self) -> Arc<Vec<ResponseItem>> {
        Arc::clone(&self.tail)
    }

    /// Appends an item to the copy-on-write tail.
    pub fn push(&mut self, item: ResponseItem) {
        Arc::make_mut(&mut self.tail).push(item);
    }

    /// Returns mutable access to the copy-on-write tail.
    pub fn tail_mut(&mut self) -> &mut Vec<ResponseItem> {
        Arc::make_mut(&mut self.tail)
    }

    /// Seals the active tail into one shared segment and starts an empty tail.
    pub fn commit_tail(&mut self) {
        if self.tail.is_empty() {
            return;
        }
        let items = std::mem::take(&mut self.tail);
        let previous_len = self.head.as_ref().map_or(0, |segment| segment.len);
        self.head = Some(Arc::new(HistorySegment {
            previous: self.head.take(),
            len: previous_len + items.len(),
            items,
        }));
    }

    /// Replaces complete history with one new mutable tail.
    pub fn replace(&mut self, items: Vec<ResponseItem>) {
        self.head = None;
        self.tail = Arc::new(items);
    }

    /// Replaces every item from `start` onward while sharing complete prefix
    /// segments.
    ///
    /// This is an internal COW primitive used when a transport operation needs
    /// to rewrite a trailing portion of retained history.
    #[doc(hidden)]
    pub fn replace_suffix(&mut self, start: usize, replacement: Vec<ResponseItem>) {
        let start = start.min(self.len());
        let committed_len = self.head.as_ref().map_or(0, |segment| segment.len);
        if start >= committed_len {
            let tail_prefix_len = start - committed_len;
            let mut tail = Vec::with_capacity(tail_prefix_len + replacement.len());
            tail.extend(self.tail[..tail_prefix_len].iter().cloned());
            tail.extend(replacement);
            self.tail = Arc::new(tail);
            return;
        }
        let mut current = self.head.clone();
        while let Some(segment) = current.take() {
            let previous_len = segment.previous.as_ref().map_or(0, |previous| previous.len);
            if start >= previous_len {
                self.head.clone_from(&segment.previous);
                self.tail = Arc::new(segment.items[..start - previous_len].to_vec());
                break;
            }
            current.clone_from(&segment.previous);
        }
        Arc::make_mut(&mut self.tail).extend(replacement);
    }

    /// Iterates over all items from oldest to newest.
    #[must_use]
    pub fn iter(&self) -> ResponseHistoryIter<'_> {
        ResponseHistoryIter::new(self, 0)
    }

    /// Iterates from an absolute item index without flattening history.
    #[must_use]
    pub fn iter_from(&self, start: usize) -> ResponseHistoryIter<'_> {
        ResponseHistoryIter::new(self, start)
    }

    /// Iterates over all items from newest to oldest.
    #[must_use]
    pub fn iter_rev(&self) -> ResponseHistoryRevIter<'_> {
        ResponseHistoryRevIter {
            tail: self.tail.iter().rev(),
            segment: self.head.as_deref(),
            segment_items: None,
            remaining: self.len(),
        }
    }

    #[cfg(test)]
    const fn committed_head(&self) -> Option<&Arc<HistorySegment>> {
        self.head.as_ref()
    }
}

impl<'a> IntoIterator for &'a ResponseHistory {
    type Item = &'a ResponseItem;
    type IntoIter = ResponseHistoryIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Forward iterator across persistent history segments.
pub struct ResponseHistoryIter<'a> {
    segments: Vec<&'a HistorySegment>,
    segment_index: usize,
    item_index: usize,
    tail: std::slice::Iter<'a, ResponseItem>,
}

impl<'a> ResponseHistoryIter<'a> {
    fn new(history: &'a ResponseHistory, start: usize) -> Self {
        let mut segments = Vec::new();
        let committed_len = history.head.as_ref().map_or(0, |segment| segment.len);
        let start = start.min(history.len());
        let mut item_index = 0;
        if start < committed_len {
            let mut current = history.head.as_deref();
            while let Some(segment) = current {
                let previous_len = segment.previous.as_ref().map_or(0, |previous| previous.len);
                segments.push(segment);
                if start >= previous_len {
                    item_index = start - previous_len;
                    break;
                }
                current = segment.previous.as_deref();
            }
            segments.reverse();
        }
        let tail_start = start.saturating_sub(committed_len);
        Self {
            segments,
            segment_index: 0,
            item_index,
            tail: history.tail[tail_start..].iter(),
        }
    }
}

impl<'a> Iterator for ResponseHistoryIter<'a> {
    type Item = &'a ResponseItem;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(segment) = self.segments.get(self.segment_index) {
            if let Some(item) = segment.items.get(self.item_index) {
                self.item_index += 1;
                return Some(item);
            }
            self.segment_index += 1;
            self.item_index = 0;
        }
        self.tail.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self
            .segments
            .iter()
            .enumerate()
            .skip(self.segment_index)
            .map(|(index, segment)| {
                if index == self.segment_index {
                    segment.items.len().saturating_sub(self.item_index)
                } else {
                    segment.items.len()
                }
            })
            .sum::<usize>()
            + self.tail.len();
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for ResponseHistoryIter<'_> {}

/// Reverse iterator across persistent history segments.
pub struct ResponseHistoryRevIter<'a> {
    tail: std::iter::Rev<std::slice::Iter<'a, ResponseItem>>,
    segment: Option<&'a HistorySegment>,
    segment_items: Option<std::iter::Rev<std::slice::Iter<'a, ResponseItem>>>,
    remaining: usize,
}

impl<'a> Iterator for ResponseHistoryRevIter<'a> {
    type Item = &'a ResponseItem;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(item) = self.tail.next() {
            self.remaining -= 1;
            return Some(item);
        }
        loop {
            if let Some(item) = self.segment_items.as_mut().and_then(Iterator::next) {
                self.remaining -= 1;
                return Some(item);
            }
            let segment = self.segment.take()?;
            self.segment = segment.previous.as_deref();
            self.segment_items = Some(segment.items.iter().rev());
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for ResponseHistoryRevIter<'_> {}

/// Borrowed, allocation-free composition of request input sources.
#[derive(Clone, Copy)]
pub struct ResponsesInput<'a> {
    first: &'a [ResponseItem],
    second: &'a [ResponseItem],
    history: Option<&'a ResponseHistory>,
    history_start: usize,
    tail: Option<&'a ResponseItem>,
}

impl<'a> ResponsesInput<'a> {
    /// Concatenates two slices and an optional terminal item.
    #[must_use]
    pub const fn new(
        first: &'a [ResponseItem],
        second: &'a [ResponseItem],
        tail: Option<&'a ResponseItem>,
    ) -> Self {
        Self {
            first,
            second,
            history: None,
            history_start: 0,
            tail,
        }
    }

    /// Concatenates a slice, complete persistent history, and a terminal item.
    #[must_use]
    pub const fn history(
        first: &'a [ResponseItem],
        history: &'a ResponseHistory,
        tail: Option<&'a ResponseItem>,
    ) -> Self {
        Self {
            first,
            second: &[],
            history: Some(history),
            history_start: 0,
            tail,
        }
    }

    /// Concatenates a slice, a persistent-history suffix, and a terminal item.
    #[must_use]
    pub const fn history_suffix(
        first: &'a [ResponseItem],
        history: &'a ResponseHistory,
        history_start: usize,
        tail: Option<&'a ResponseItem>,
    ) -> Self {
        Self {
            first,
            second: &[],
            history: Some(history),
            history_start,
            tail,
        }
    }

    /// Iterates over the composed input in wire order.
    #[must_use]
    pub fn iter(self) -> ResponsesInputIter<'a> {
        ResponsesInputIter {
            first: self.first.iter(),
            second: self.second.iter(),
            history: self
                .history
                .map(|history| history.iter_from(self.history_start)),
            tail: self.tail.into_iter(),
        }
    }

    /// Returns the exact composed item count.
    #[must_use]
    pub fn len(self) -> usize {
        self.first.len()
            + self.second.len()
            + self.history.map_or(0, |history| {
                history.len().saturating_sub(self.history_start)
            })
            + usize::from(self.tail.is_some())
    }

    /// Returns whether the composed input has no items.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

/// Iterator over a borrowed [`ResponsesInput`].
pub struct ResponsesInputIter<'a> {
    first: std::slice::Iter<'a, ResponseItem>,
    second: std::slice::Iter<'a, ResponseItem>,
    history: Option<ResponseHistoryIter<'a>>,
    tail: std::option::IntoIter<&'a ResponseItem>,
}

impl<'a> Iterator for ResponsesInputIter<'a> {
    type Item = &'a ResponseItem;

    fn next(&mut self) -> Option<Self::Item> {
        self.first
            .next()
            .or_else(|| self.second.next())
            .or_else(|| self.history.as_mut().and_then(Iterator::next))
            .or_else(|| self.tail.next())
    }
}

impl Serialize for ResponsesInput<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.len()))?;
        for item in self.iter() {
            sequence.serialize_element(&RequestResponseItem { item })?;
        }
        sequence.end()
    }
}

#[derive(Clone, Copy)]
struct RequestInput<'a> {
    input: ResponsesInput<'a>,
}

impl Serialize for RequestInput<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.input.len()))?;
        for item in self.input.iter() {
            sequence.serialize_element(&RequestResponseItem { item })?;
        }
        sequence.end()
    }
}

struct RequestResponseItem<'a> {
    item: &'a ResponseItem,
}

impl Serialize for RequestResponseItem<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if self.item.id().is_some_and(|id| !id.is_prefixed()) {
            let mut item = self.item.clone();
            item.set_id(None);
            item.serialize(serializer)
        } else {
            self.item.serialize(serializer)
        }
    }
}

/// Fully typed wire request for `response.create` or WebSocket warmup.
#[derive(Serialize)]
pub(crate) struct ResponseCreate<'a> {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    model: Cow<'static, str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<&'a str>,
    input: RequestInput<'a>,
    tool_choice: &'static str,
    parallel_tool_calls: bool,
    reasoning: ReasoningControls,
    store: bool,
    stream: bool,
    include: [&'static str; 1],
    prompt_cache_key: &'a str,
    text: TextControls,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generate: Option<bool>,
    client_metadata: ClientMetadata<'a>,
}

impl<'a> ResponseCreate<'a> {
    /// Builds a non-generating WebSocket warmup request.
    #[must_use]
    pub(crate) fn warmup(
        config: &'a ModelConfig,
        model: crate::Model,
        thinking: Thinking,
        fast_mode: bool,
        profile: &'a RequestProfile,
        turn_state: Option<&'a str>,
    ) -> Self {
        Self::new(
            config,
            CreatePolicy {
                transport: config.responses_transport,
                model,
                thinking,
                fast_mode,
            },
            ResponsesInput::new(profile.prefix(), &[], None),
            None,
            Some(false),
            profile,
            turn_state,
        )
    }

    pub(crate) fn generation_with_policy(
        config: &'a ModelConfig,
        policy: CreatePolicy,
        input: ResponsesInput<'a>,
        previous_response_id: Option<&'a str>,
        profile: &'a RequestProfile,
        turn_state: Option<&'a str>,
    ) -> Self {
        Self::new(
            config,
            policy,
            input,
            previous_response_id,
            None,
            profile,
            turn_state,
        )
    }

    fn new(
        config: &'a ModelConfig,
        policy: CreatePolicy,
        input: ResponsesInput<'a>,
        previous_response_id: Option<&'a str>,
        generate: Option<bool>,
        profile: &'a RequestProfile,
        turn_state: Option<&'a str>,
    ) -> Self {
        let websocket = matches!(policy.transport, crate::ResponsesTransport::WebSocket);
        let retained = profile.retained_config.as_ref();
        let model = retained.map_or_else(
            || config.wire_model_id(policy.model),
            |retained| {
                retained.model_id_prefix.as_ref().map_or_else(
                    || Cow::Borrowed(policy.model.as_str()),
                    |prefix| Cow::Owned(format!("{prefix}/{}", policy.model.as_str())),
                )
            },
        );
        let reasoning_mode =
            retained.map_or(config.reasoning_mode, |retained| retained.reasoning_mode);
        let store_responses =
            retained.map_or(config.store_responses, |retained| retained.store_responses);
        Self {
            kind: websocket.then_some("response.create"),
            model,
            previous_response_id,
            input: RequestInput { input },
            tool_choice: "auto",
            // gpt-5.6-sol uses Responses Lite. Codex disables the provider
            // parallel-call request bit for Lite even though the client-side
            // scheduler still accepts multi-call responses and replays.
            parallel_tool_calls: false,
            reasoning: ReasoningControls {
                // Astra rejects the legacy `reasoning.mode` field. Standard
                // already serializes as absent; keep this model guard as a
                // final wire-level invariant for custom service factories.
                mode: (policy.model != crate::Model::Astra)
                    .then(|| reasoning_mode.request_value())
                    .flatten(),
                effort: policy.thinking.as_str(),
                summary: (policy.model != crate::Model::Astra).then_some("auto"),
                context: "all_turns",
            },
            store: store_responses,
            stream: true,
            include: ["reasoning.encrypted_content"],
            prompt_cache_key: profile.prompt_cache_key(),
            text: TextControls { verbosity: "low" },
            // The API accepts both `fast` and `priority`. Codex currently uses
            // `priority` as the compatibility request value for Fast mode.
            // Astra standard mode is explicit so a project-level Fast default
            // cannot silently change processing or the local cost estimate.
            service_tier: match (policy.model, policy.fast_mode) {
                (_, true) => Some("priority"),
                (crate::Model::Astra, false) => Some("default"),
                (_, false) => None,
            },
            generate,
            client_metadata: ClientMetadata {
                session_id: profile.session_id(),
                thread_id: profile.thread_id(),
                responses_lite: websocket.then_some("true"),
                turn_state: websocket.then_some(turn_state).flatten(),
                turn_metadata: (!profile.code_mode_tool_names.is_empty()).then_some(
                    SerializedCodeModeTurnMetadata(&profile.code_mode_tool_names),
                ),
            },
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CreatePolicy {
    transport: crate::ResponsesTransport,
    model: crate::Model,
    thinking: Thinking,
    fast_mode: bool,
}

impl CreatePolicy {
    pub(crate) const fn new(
        transport: crate::ResponsesTransport,
        model: crate::Model,
        thinking: Thinking,
        fast_mode: bool,
    ) -> Self {
        Self {
            transport,
            model,
            thinking,
            fast_mode,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
struct ReasoningControls {
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'static str>,
    effort: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<&'static str>,
    context: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct TextControls {
    verbosity: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct ClientMetadata<'a> {
    session_id: &'a str,
    thread_id: &'a str,
    #[serde(rename = "ws_request_header_x_openai_internal_codex_responses_lite")]
    #[serde(skip_serializing_if = "Option::is_none")]
    responses_lite: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "x-codex-turn-state")]
    turn_state: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "x-codex-turn-metadata")]
    turn_metadata: Option<SerializedCodeModeTurnMetadata<'a>>,
}

#[derive(Clone, Copy)]
struct SerializedCodeModeTurnMetadata<'a>(&'a BTreeMap<String, CodeModeToolName>);

impl Serialize for SerializedCodeModeTurnMetadata<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct TurnMetadata<'a> {
            code_mode_tool_names: &'a BTreeMap<String, CodeModeToolName>,
        }

        let value = serde_json::to_string(&TurnMetadata {
            code_mode_tool_names: self.0,
        })
        .map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentItem, MessageRole, Model, ReasoningMode, Thinking};
    use serde_json::json;

    #[test]
    fn prompt_cache_key_is_stable_across_the_session() {
        let config = ModelConfig {
            auth: crate::OpenAiAuth::api_key("test-key"),
            thinking: Thinking::Low,
            ..ModelConfig::default()
        };
        let prefix: Arc<[ResponseItem]> = Arc::from([ResponseItem::message(
            MessageRole::Developer,
            [ContentItem::InputText {
                text: "system prompt".into(),
            }],
        )]);
        let profile =
            RequestProfile::new("session-a", "lineage-a", prefix).with_thread_id("branch-a");
        let request =
            ResponseCreate::warmup(&config, Model::Sol, Thinking::Low, false, &profile, None);
        let request = serde_json::to_value(request).expect("request should serialize");

        assert_eq!(request["prompt_cache_key"], json!("lineage-a"));
        assert_eq!(request["client_metadata"]["session_id"], json!("session-a"));
        assert_eq!(request["client_metadata"]["thread_id"], json!("branch-a"));
        assert_eq!(request["store"], false);
        assert_eq!(request["generate"], false);
        assert_eq!(request["parallel_tool_calls"], false);
        assert!(request.get("tools").is_none());
        assert!(request.get("instructions").is_none());
        assert_eq!(request["reasoning"]["summary"], json!("auto"));
        assert!(request["reasoning"].get("mode").is_none());
        assert!(request.get("context_management").is_none());
    }

    #[test]
    fn responses_lite_metadata_maps_plain_and_namespaced_code_mode_tools() {
        let config = ModelConfig {
            auth: crate::OpenAiAuth::api_key("test-key"),
            responses_transport: crate::ResponsesTransport::Https,
            ..ModelConfig::default()
        };
        let profile = RequestProfile::new("branch-a", "lineage-a", Arc::from([]))
            .with_code_mode_tool_names([
                ("exec_command".to_owned(), "exec_command".to_owned()),
                (
                    "mcp__calendar__lookup".to_owned(),
                    "mcp__calendar__lookup".to_owned(),
                ),
            ]);
        let request = serde_json::to_value(ResponseCreate::warmup(
            &config,
            Model::Sol,
            Thinking::Low,
            false,
            &profile,
            None,
        ))
        .expect("request should serialize");
        let metadata = request["client_metadata"]["x-codex-turn-metadata"]
            .as_str()
            .and_then(|metadata| serde_json::from_str::<serde_json::Value>(metadata).ok())
            .expect("turn metadata should be encoded as JSON");

        assert_eq!(
            metadata["code_mode_tool_names"]["exec_command"],
            json!({"name": "exec_command", "namespace": null})
        );
        assert_eq!(
            metadata["code_mode_tool_names"]["mcp__calendar__lookup"],
            json!({"name": "lookup", "namespace": "mcp__calendar"})
        );
        assert!(
            request["client_metadata"]
                .get("ws_request_header_x_openai_internal_codex_responses_lite")
                .is_none()
        );
    }

    #[test]
    fn request_serialization_matches_codex_item_id_policy_without_mutating_history() {
        let mut client_item = ResponseItem::message(
            MessageRole::User,
            [ContentItem::InputText {
                text: "client".into(),
            }],
        );
        client_item.set_id(Some(super::super::ResponseItemId::with_suffix(
            "msg", "stable",
        )));
        let mut server_item = ResponseItem::message(
            MessageRole::Assistant,
            [ContentItem::OutputText {
                text: "server".into(),
                annotations: None,
                logprobs: None,
            }],
        );
        server_item.set_id(Some(super::super::ResponseItemId::from_server(
            "server-item-id",
        )));
        let history = ResponseHistory::new(vec![client_item, server_item]);
        let stored_config = ModelConfig {
            store_responses: true,
            ..ModelConfig::default()
        };
        let profile = RequestProfile::new("agent", "lineage", Arc::from([]));

        let stored_request = serde_json::to_value(ResponseCreate::generation_with_policy(
            &stored_config,
            CreatePolicy::new(
                stored_config.responses_transport,
                Model::Sol,
                Thinking::Medium,
                false,
            ),
            ResponsesInput::history(&[], &history, None),
            None,
            &profile,
            None,
        ))
        .expect("request should serialize");

        assert_eq!(stored_request["input"][0]["id"], "msg_stable");
        assert!(stored_request["input"][1].get("id").is_none());

        let ephemeral_config = ModelConfig {
            store_responses: false,
            ..ModelConfig::default()
        };
        let ephemeral_request = serde_json::to_value(ResponseCreate::generation_with_policy(
            &ephemeral_config,
            CreatePolicy::new(
                ephemeral_config.responses_transport,
                Model::Sol,
                Thinking::Medium,
                false,
            ),
            ResponsesInput::history(&[], &history, None),
            None,
            &profile,
            None,
        ))
        .expect("request should serialize");

        assert_eq!(ephemeral_request["input"][0]["id"], "msg_stable");
        assert!(ephemeral_request["input"][1].get("id").is_none());
        assert_eq!(
            history
                .iter()
                .nth(1)
                .and_then(ResponseItem::id)
                .map(super::super::ResponseItemId::as_str),
            Some("server-item-id"),
            "outbound preparation must not mutate authoritative history"
        );
    }

    #[test]
    fn thinking_defaults_to_high() {
        assert_eq!(ModelConfig::default().thinking, Thinking::High);
    }

    #[test]
    fn supported_models_serialize_as_selected() {
        for (model, expected) in [
            (Model::Sol, "gpt-5.6-sol"),
            (Model::Terra, "gpt-5.6-terra"),
            (Model::Luna, "gpt-5.6-luna"),
            (Model::Astra, "gpt-6-astra"),
        ] {
            let config = ModelConfig::default();
            let profile = RequestProfile::new("model-agent", "model-lineage", Arc::from([]));
            let request = serde_json::to_value(ResponseCreate::warmup(
                &config,
                model,
                Thinking::Medium,
                false,
                &profile,
                None,
            ))
            .expect("request should serialize");

            assert_eq!(request["model"], json!(expected));
        }
    }

    #[test]
    fn model_namespace_qualifies_only_the_wire_identifier() {
        let config = ModelConfig {
            model_id_prefix: Some(Arc::from("openai")),
            ..ModelConfig::default()
        };
        let profile = RequestProfile::new("gateway-agent", "gateway-lineage", Arc::from([]));
        let request = serde_json::to_value(ResponseCreate::warmup(
            &config,
            Model::Terra,
            Thinking::Medium,
            false,
            &profile,
            None,
        ))
        .expect("request should serialize");

        assert_eq!(request["model"], json!("openai/gpt-5.6-terra"));
    }

    #[test]
    fn retained_requests_keep_provider_settings_on_a_new_transport() {
        let config = ModelConfig {
            model_id_prefix: Some(Arc::from("updated")),
            responses_transport: crate::ResponsesTransport::Https,
            ..ModelConfig::default()
        };
        let profile = RequestProfile::new("current-session", "current-cache", Arc::from([]))
            .with_request_content(
                "original-cache".to_owned(),
                Arc::from([]),
                Some("original".to_owned()),
                ReasoningMode::Pro,
                true,
            );
        let request = serde_json::to_value(ResponseCreate::warmup(
            &config,
            Model::Terra,
            Thinking::Max,
            true,
            &profile,
            None,
        ))
        .unwrap();
        assert_eq!(request["model"], "original/gpt-5.6-terra");
        assert_eq!(request["reasoning"]["mode"], "pro");
        assert_eq!(request["reasoning"]["effort"], "max");
        assert_eq!(request["service_tier"], "priority");
        assert_eq!(request["store"], true);
        assert_eq!(request["prompt_cache_key"], "original-cache");
        assert_eq!(request["client_metadata"]["session_id"], "current-session");
        assert!(
            request.get("type").is_none(),
            "transport belongs to the current owner"
        );
    }

    #[test]
    fn pro_mode_and_every_effort_serialize_independently() {
        let prefix: Arc<[ResponseItem]> = Arc::from([ResponseItem::message(
            MessageRole::Developer,
            [ContentItem::InputText {
                text: "system prompt".into(),
            }],
        )]);
        let profile = RequestProfile::new("pro-agent", "pro-lineage", prefix);

        for (thinking, expected) in [
            (Thinking::None, "none"),
            (Thinking::Low, "low"),
            (Thinking::Medium, "medium"),
            (Thinking::High, "high"),
            (Thinking::Xhigh, "xhigh"),
            (Thinking::Max, "max"),
        ] {
            let config = ModelConfig {
                auth: crate::OpenAiAuth::api_key("test-key"),
                reasoning_mode: ReasoningMode::Pro,
                thinking,
                ..ModelConfig::default()
            };
            let request = serde_json::to_value(ResponseCreate::warmup(
                &config,
                Model::Sol,
                thinking,
                false,
                &profile,
                None,
            ))
            .expect("request should serialize");

            assert_eq!(request["reasoning"]["mode"], json!("pro"));
            assert_eq!(request["reasoning"]["effort"], json!(expected));
            assert_eq!(request["reasoning"]["context"], json!("all_turns"));
        }
    }

    #[test]
    fn astra_omits_reasoning_mode_and_default_summary() {
        let config = ModelConfig {
            reasoning_mode: ReasoningMode::Pro,
            ..ModelConfig::default()
        };
        let profile = RequestProfile::new("astra-agent", "astra-lineage", Arc::from([]));
        let request = serde_json::to_value(ResponseCreate::warmup(
            &config,
            Model::Astra,
            Thinking::Max,
            false,
            &profile,
            None,
        ))
        .expect("request should serialize");

        assert!(request["reasoning"].get("mode").is_none());
        assert!(request["reasoning"].get("summary").is_none());
        assert_eq!(request["reasoning"]["effort"], json!("max"));
    }

    #[test]
    fn response_storage_support_tracks_auth_mode() {
        assert!(crate::OpenAiAuthMode::ApiKey.supports_stored_responses());
        assert!(!crate::OpenAiAuthMode::ChatGpt.supports_stored_responses());
    }

    #[test]
    fn fast_mode_selects_the_codex_compatible_service_tier() {
        let config = ModelConfig::default();
        let profile = RequestProfile::new("fast-agent", "fast-lineage", Arc::from([]));
        let standard = serde_json::to_value(ResponseCreate::warmup(
            &config,
            Model::Sol,
            Thinking::Medium,
            false,
            &profile,
            None,
        ))
        .expect("standard request should serialize");
        let fast = serde_json::to_value(ResponseCreate::warmup(
            &config,
            Model::Sol,
            Thinking::Medium,
            true,
            &profile,
            None,
        ))
        .expect("fast request should serialize");
        assert!(standard.get("service_tier").is_none());
        assert_eq!(fast["service_tier"], json!("priority"));

        let astra_standard = serde_json::to_value(ResponseCreate::warmup(
            &config,
            Model::Astra,
            Thinking::Medium,
            false,
            &profile,
            None,
        ))
        .expect("Astra standard request should serialize");
        assert_eq!(astra_standard["service_tier"], json!("default"));
    }

    #[test]
    fn committed_history_is_shared_and_iterates_oldest_first() {
        let mut history = ResponseHistory::new(vec![ResponseItem::message(
            MessageRole::User,
            [ContentItem::InputText { text: "one".into() }],
        )]);
        history.commit_tail();
        let first_head = Arc::clone(history.committed_head().unwrap());
        history.push(ResponseItem::message(
            MessageRole::Assistant,
            [ContentItem::OutputText {
                text: "two".into(),
                annotations: None,
                logprobs: None,
            }],
        ));
        history.commit_tail();
        let fork = history.clone();

        assert_eq!(history.len(), 2);
        assert!(Arc::ptr_eq(
            history.committed_head().unwrap().previous.as_ref().unwrap(),
            &first_head
        ));
        assert!(Arc::ptr_eq(
            history.committed_head().unwrap(),
            fork.committed_head().unwrap()
        ));
        assert_eq!(history.iter().count(), 2);
    }

    #[test]
    fn sealing_a_boundary_reuses_the_tail_and_suffixes_cross_segments() {
        let item = |text: &'static str| {
            ResponseItem::message(
                MessageRole::User,
                [ContentItem::InputText { text: text.into() }],
            )
        };
        let mut history = ResponseHistory::new(vec![item("zero"), item("one")]);
        let active_tail = history.shared_tail();
        history.commit_tail();
        assert!(Arc::ptr_eq(
            &history.committed_head().unwrap().items,
            &active_tail,
        ));
        history.push(item("two"));
        history.commit_tail();
        history.push(item("three"));

        let suffix: Vec<_> = history.iter_from(1).cloned().collect();
        assert_eq!(
            serde_json::to_value(suffix).unwrap(),
            serde_json::to_value(vec![item("one"), item("two"), item("three")]).unwrap(),
        );
        assert_eq!(history.iter_from(99).count(), 0);
    }

    #[test]
    fn reverse_iteration_crosses_tail_and_segments_newest_first() {
        let item = |text: &'static str| {
            ResponseItem::message(
                MessageRole::User,
                [ContentItem::InputText { text: text.into() }],
            )
        };
        let mut history = ResponseHistory::new(vec![item("zero"), item("one")]);
        history.commit_tail();
        history.push(item("two"));
        history.commit_tail();
        history.push(item("three"));

        let reversed: Vec<_> = history.iter_rev().cloned().collect();
        assert_eq!(
            serde_json::to_value(reversed).unwrap(),
            serde_json::to_value(vec![item("three"), item("two"), item("one"), item("zero")])
                .unwrap(),
        );
    }

    #[test]
    fn replacing_a_suffix_shares_complete_prefix_segments() {
        let item = |text: &'static str| {
            ResponseItem::message(
                MessageRole::User,
                [ContentItem::InputText { text: text.into() }],
            )
        };
        let mut history = ResponseHistory::new(vec![item("zero"), item("one")]);
        history.commit_tail();
        let shared_prefix = Arc::clone(history.committed_head().unwrap());
        history.push(item("two"));
        history.commit_tail();
        history.push(item("three"));

        history.replace_suffix(2, vec![item("replacement")]);

        assert!(Arc::ptr_eq(
            history.committed_head().unwrap(),
            &shared_prefix
        ));
        assert_eq!(
            serde_json::to_value(history.iter().cloned().collect::<Vec<_>>()).unwrap(),
            serde_json::to_value(vec![item("zero"), item("one"), item("replacement")]).unwrap(),
        );
    }
}
