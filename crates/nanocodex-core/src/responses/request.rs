use std::sync::Arc;

use serde::{Serialize, ser::SerializeSeq};

use super::ResponseItem;
use crate::{ModelConfig, Thinking};

/// Stable request metadata and prefix shared by every operation in a session.
#[derive(Clone)]
pub struct RequestProfile {
    session_id: String,
    prompt_cache_key: String,
    prefix: Arc<[ResponseItem]>,
}

impl RequestProfile {
    #[must_use]
    pub fn new(
        session_id: impl Into<String>,
        prompt_cache_key: impl Into<String>,
        prefix: Arc<[ResponseItem]>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            prompt_cache_key: prompt_cache_key.into(),
            prefix,
        }
    }

    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    #[must_use]
    pub fn prompt_cache_key(&self) -> &str {
        &self.prompt_cache_key
    }

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
    previous: Option<Arc<HistorySegment>>,
    items: Arc<Vec<ResponseItem>>,
    len: usize,
}

impl ResponseHistory {
    #[must_use]
    pub fn new(items: Vec<ResponseItem>) -> Self {
        Self {
            head: None,
            tail: Arc::new(items),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.head.as_ref().map_or(0, |segment| segment.len) + self.tail.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub fn tail(&self) -> &[ResponseItem] {
        &self.tail
    }

    #[must_use]
    pub fn shared_tail(&self) -> Arc<Vec<ResponseItem>> {
        Arc::clone(&self.tail)
    }

    pub fn push(&mut self, item: ResponseItem) {
        Arc::make_mut(&mut self.tail).push(item);
    }

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

    pub fn replace(&mut self, items: Vec<ResponseItem>) {
        self.head = None;
        self.tail = Arc::new(items);
    }

    #[must_use]
    pub fn iter(&self) -> ResponseHistoryIter<'_> {
        ResponseHistoryIter::new(self, 0)
    }

    #[must_use]
    pub fn iter_from(&self, start: usize) -> ResponseHistoryIter<'_> {
        ResponseHistoryIter::new(self, start)
    }

    #[cfg(test)]
    fn committed_head(&self) -> Option<&Arc<HistorySegment>> {
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

#[derive(Clone, Copy)]
pub struct ResponsesInput<'a> {
    first: &'a [ResponseItem],
    second: &'a [ResponseItem],
    history: Option<&'a ResponseHistory>,
    history_start: usize,
    tail: Option<&'a ResponseItem>,
}

impl<'a> ResponsesInput<'a> {
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

    #[must_use]
    pub fn len(self) -> usize {
        self.first.len()
            + self.second.len()
            + self.history.map_or(0, |history| {
                history.len().saturating_sub(self.history_start)
            })
            + usize::from(self.tail.is_some())
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

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
            sequence.serialize_element(&RequestResponseItem(item))?;
        }
        sequence.end()
    }
}

struct RequestResponseItem<'a>(&'a ResponseItem);

impl Serialize for RequestResponseItem<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if self.0.id().is_some_and(|id| !id.is_prefixed()) {
            let mut item = self.0.clone();
            item.set_id(None);
            item.serialize(serializer)
        } else {
            self.0.serialize(serializer)
        }
    }
}

#[derive(Serialize)]
pub struct ResponseCreate<'a> {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<&'a str>,
    input: ResponsesInput<'a>,
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
    #[must_use]
    pub fn warmup(
        config: &'a ModelConfig,
        thinking: Thinking,
        fast_mode: bool,
        profile: &'a RequestProfile,
        turn_state: Option<&'a str>,
    ) -> Self {
        Self::new(
            config,
            RequestPolicy {
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

    #[must_use]
    pub fn generation(
        config: &'a ModelConfig,
        thinking: Thinking,
        fast_mode: bool,
        input: ResponsesInput<'a>,
        previous_response_id: Option<&'a str>,
        profile: &'a RequestProfile,
        turn_state: Option<&'a str>,
    ) -> Self {
        Self::new(
            config,
            RequestPolicy {
                thinking,
                fast_mode,
            },
            input,
            previous_response_id,
            None,
            profile,
            turn_state,
        )
    }

    fn new(
        config: &'a ModelConfig,
        policy: RequestPolicy,
        input: ResponsesInput<'a>,
        previous_response_id: Option<&'a str>,
        generate: Option<bool>,
        profile: &'a RequestProfile,
        turn_state: Option<&'a str>,
    ) -> Self {
        let websocket = matches!(
            config.responses_transport,
            crate::ResponsesTransport::WebSocket
        );
        Self {
            kind: websocket.then_some("response.create"),
            model: crate::MODEL,
            previous_response_id,
            input,
            tool_choice: "auto",
            parallel_tool_calls: false,
            reasoning: ReasoningControls {
                mode: config.reasoning_mode.request_value(),
                effort: policy.thinking.as_str(),
                summary: None,
                context: "all_turns",
            },
            store: config.store_responses,
            stream: true,
            include: ["reasoning.encrypted_content"],
            prompt_cache_key: profile.prompt_cache_key(),
            text: TextControls { verbosity: "low" },
            service_tier: policy.fast_mode.then_some("priority"),
            generate,
            client_metadata: ClientMetadata {
                session_id: profile.session_id(),
                thread_id: profile.session_id(),
                responses_lite: websocket.then_some("true"),
                turn_state: websocket.then_some(turn_state).flatten(),
            },
        }
    }
}

#[derive(Clone, Copy)]
struct RequestPolicy {
    thinking: Thinking,
    fast_mode: bool,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentItem, MessageRole, ReasoningMode, Thinking};
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
        let profile = RequestProfile::new("branch-a", "lineage-a", prefix);
        let request = ResponseCreate::warmup(&config, Thinking::Low, false, &profile, None);
        let request = serde_json::to_value(request).expect("request should serialize");

        assert_eq!(request["prompt_cache_key"], json!("lineage-a"));
        assert_eq!(request["client_metadata"]["session_id"], json!("branch-a"));
        assert_eq!(request["client_metadata"]["thread_id"], json!("branch-a"));
        assert_eq!(request["store"], true);
        assert_eq!(request["generate"], false);
        assert!(request.get("tools").is_none());
        assert!(request.get("instructions").is_none());
        assert!(request["reasoning"].get("summary").is_none());
        assert!(request["reasoning"].get("mode").is_none());
        assert!(request.get("context_management").is_none());
    }

    #[test]
    fn request_serialization_preserves_client_ids_and_only_omits_unprefixed_server_ids() {
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
        let config = ModelConfig::default();
        let profile = RequestProfile::new("agent", "lineage", Arc::from([]));

        let request = serde_json::to_value(ResponseCreate::generation(
            &config,
            Thinking::Medium,
            false,
            ResponsesInput::history(&[], &history, None),
            None,
            &profile,
            None,
        ))
        .expect("request should serialize");

        assert_eq!(request["input"][0]["id"], "msg_stable");
        assert!(request["input"][1].get("id").is_none());
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
    fn thinking_defaults_to_medium() {
        assert_eq!(ModelConfig::default().thinking, Thinking::Medium);
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
                &config, thinking, false, &profile, None,
            ))
            .expect("request should serialize");

            assert_eq!(request["reasoning"]["mode"], json!("pro"));
            assert_eq!(request["reasoning"]["effort"], json!(expected));
            assert_eq!(request["reasoning"]["context"], json!("all_turns"));
        }
    }

    #[test]
    fn response_storage_support_tracks_auth_mode() {
        assert!(crate::OpenAiAuthMode::ApiKey.supports_stored_responses());
        assert!(!crate::OpenAiAuthMode::ChatGpt.supports_stored_responses());
    }

    #[test]
    fn fast_mode_selects_priority_service_tier() {
        let config = ModelConfig::default();
        let profile = RequestProfile::new("fast-agent", "fast-lineage", Arc::from([]));
        let standard = serde_json::to_value(ResponseCreate::warmup(
            &config,
            Thinking::Medium,
            false,
            &profile,
            None,
        ))
        .expect("standard request should serialize");
        let fast = serde_json::to_value(ResponseCreate::warmup(
            &config,
            Thinking::Medium,
            true,
            &profile,
            None,
        ))
        .expect("fast request should serialize");

        assert!(standard.get("service_tier").is_none());
        assert_eq!(fast["service_tier"], json!("priority"));
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
}
