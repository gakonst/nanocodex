//! Complete typed transcript items used by Responses input and output.

use std::time::{SystemTime, UNIX_EPOCH};
use std::{fmt, ops::Deref};

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use super::{
    AgentMessageContent, ContentItem, FunctionOutputBody, InternalMessageMetadata, ItemStatus,
    JsonValue, LocalShellAction, LocalShellStatus, MessagePhase, MessageRole, ReasoningContent,
    ReasoningSummary, ToolCaller, ToolDefinition, WebSearchAction,
};

/// A stable Responses API item identifier.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ResponseItemId(Box<str>);

impl ResponseItemId {
    /// Builds a deterministic client-owned ID from a type prefix and suffix.
    #[must_use]
    pub fn with_suffix(prefix: &str, suffix: impl fmt::Display) -> Self {
        Self(format!("{prefix}_{suffix}").into_boxed_str())
    }

    /// Retains a provider-supplied item ID.
    #[must_use]
    pub fn from_server(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// Returns the encoded item ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns whether the encoded item ID is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns whether this is a deterministic Nanocodex-owned ID.
    #[must_use]
    pub fn is_prefixed(&self) -> bool {
        self.split_once('_')
            .is_some_and(|(prefix, suffix)| !prefix.is_empty() && !suffix.is_empty())
    }
}

impl Deref for ResponseItemId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl AsRef<str> for ResponseItemId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ResponseItemId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<String> for ResponseItemId {
    fn from(value: String) -> Self {
        Self(value.into_boxed_str())
    }
}

impl From<&str> for ResponseItemId {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}

/// One typed Responses transcript item.
///
/// Variants preserve the provider's complete known wire fields. Unknown future
/// item types are retained by [`ResponseItem::Other`] without converting known
/// history into an untyped JSON document.
#[allow(missing_docs)]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseItem {
    AdditionalTools {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ResponseItemId>,
        role: MessageRole,
        tools: Vec<ToolDefinition>,
    },
    Message {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ResponseItemId>,
        role: MessageRole,
        content: Vec<ContentItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<ItemStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<MessagePhase>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    AgentMessage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ResponseItemId>,
        author: Box<str>,
        recipient: Box<str>,
        content: SmallVec<[AgentMessageContent; 1]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    Reasoning {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ResponseItemId>,
        #[serde(default)]
        summary: Vec<ReasoningSummary>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Vec<ReasoningContent>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<ItemStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    LocalShellCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ResponseItemId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<Box<str>>,
        status: LocalShellStatus,
        action: LocalShellAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    FunctionCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ResponseItemId>,
        name: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<Box<str>>,
        arguments: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_function_args: Option<Vec<Box<str>>>,
        call_id: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller: Option<ToolCaller>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<ItemStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        created_by: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    FunctionCallOutput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ResponseItemId>,
        call_id: Box<str>,
        output: FunctionOutputBody,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller: Option<ToolCaller>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<ItemStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        created_by: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    ToolSearchCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ResponseItemId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<Box<str>>,
        execution: Box<str>,
        arguments: JsonValue,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    CustomToolCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ResponseItemId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<ItemStatus>,
        call_id: Box<str>,
        name: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<Box<str>>,
        input: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller: Option<ToolCaller>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        created_by: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    CustomToolCallOutput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ResponseItemId>,
        call_id: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<Box<str>>,
        output: FunctionOutputBody,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller: Option<ToolCaller>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<ItemStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        created_by: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    ToolSearchOutput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ResponseItemId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<Box<str>>,
        status: Box<str>,
        execution: Box<str>,
        tools: Vec<JsonValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    WebSearchCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ResponseItemId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<WebSearchAction>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    ImageGenerationCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ResponseItemId>,
        status: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revised_prompt: Option<Box<str>>,
        result: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    #[serde(alias = "compaction_summary")]
    Compaction {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ResponseItemId>,
        encrypted_content: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        created_by: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    CompactionTrigger {},
    ContextCompaction {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<ResponseItemId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    #[serde(untagged)]
    Other(JsonValue),
}

impl ResponseItem {
    /// Creates the stable developer item that declares session tools.
    #[must_use]
    pub const fn additional_tools(tools: Vec<ToolDefinition>) -> Self {
        Self::AdditionalTools {
            id: None,
            role: MessageRole::Developer,
            tools,
        }
    }

    /// Creates a message without provider status or internal metadata.
    #[must_use]
    pub fn message(role: MessageRole, content: impl IntoIterator<Item = ContentItem>) -> Self {
        Self::Message {
            id: None,
            role,
            content: content.into_iter().collect(),
            status: None,
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }
    }

    /// Creates completed output for one custom-tool call.
    #[must_use]
    pub fn custom_tool_output(
        call_id: String,
        name: Option<String>,
        output: FunctionOutputBody,
    ) -> Self {
        Self::CustomToolCallOutput {
            id: None,
            call_id: call_id.into_boxed_str(),
            name: name.map(String::into_boxed_str),
            output,
            caller: None,
            status: None,
            created_by: None,
            internal_chat_message_metadata_passthrough: None,
        }
    }

    /// Creates completed output for one function call.
    #[must_use]
    pub fn function_call_output(call_id: String, output: FunctionOutputBody) -> Self {
        Self::FunctionCallOutput {
            id: None,
            call_id: call_id.into_boxed_str(),
            output,
            caller: None,
            status: None,
            created_by: None,
            internal_chat_message_metadata_passthrough: None,
        }
    }

    /// Creates the typed terminal item for a remote compaction request.
    #[must_use]
    pub const fn compaction_trigger() -> Self {
        Self::CompactionTrigger {}
    }

    /// Returns whether this item is a user-role message.
    #[must_use]
    pub const fn is_user_message(&self) -> bool {
        matches!(
            self,
            Self::Message {
                role: MessageRole::User,
                ..
            }
        )
    }

    /// Returns whether this is a developer-role message.
    #[doc(hidden)]
    #[must_use]
    pub const fn is_developer_message(&self) -> bool {
        matches!(
            self,
            Self::Message {
                role: MessageRole::Developer,
                ..
            }
        )
    }

    /// Returns semantic content kinds carried by a message.
    #[doc(hidden)]
    #[must_use]
    pub fn message_content_item_kinds(&self) -> Option<&[super::ContentItemKind]> {
        let Self::Message {
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } = self
        else {
            return None;
        };
        metadata.content_item_kinds.as_deref()
    }

    /// Labels a model-visible message fragment with one open semantic kind.
    #[doc(hidden)]
    pub fn set_message_content_item_kind(&mut self, kind: impl Into<Box<str>>) {
        self.set_message_content_item_kinds([super::ContentItemKind(kind.into())]);
    }

    /// Labels each content entry in a model-visible message with an open semantic kind.
    #[doc(hidden)]
    pub fn set_message_content_item_kinds(
        &mut self,
        kinds: impl IntoIterator<Item = super::ContentItemKind>,
    ) {
        let Self::Message {
            internal_chat_message_metadata_passthrough,
            ..
        } = self
        else {
            return;
        };
        let metadata = internal_chat_message_metadata_passthrough
            .get_or_insert_with(InternalMessageMetadata::default);
        metadata.content_item_kinds = Some(kinds.into_iter().collect());
    }

    /// Returns the Responses API item ID, if present.
    #[must_use]
    pub const fn id(&self) -> Option<&ResponseItemId> {
        match self {
            Self::AdditionalTools { id, .. }
            | Self::Message { id, .. }
            | Self::AgentMessage { id, .. }
            | Self::Reasoning { id, .. }
            | Self::LocalShellCall { id, .. }
            | Self::FunctionCall { id, .. }
            | Self::FunctionCallOutput { id, .. }
            | Self::ToolSearchCall { id, .. }
            | Self::CustomToolCall { id, .. }
            | Self::CustomToolCallOutput { id, .. }
            | Self::ToolSearchOutput { id, .. }
            | Self::WebSearchCall { id, .. }
            | Self::ImageGenerationCall { id, .. }
            | Self::Compaction { id, .. }
            | Self::ContextCompaction { id, .. } => id.as_ref(),
            Self::CompactionTrigger {} | Self::Other(_) => None,
        }
    }

    /// Sets or clears the Responses API item ID for variants that carry one.
    pub fn set_id(&mut self, new_id: Option<ResponseItemId>) {
        match self {
            Self::AdditionalTools { id, .. }
            | Self::Message { id, .. }
            | Self::AgentMessage { id, .. }
            | Self::Reasoning { id, .. }
            | Self::LocalShellCall { id, .. }
            | Self::FunctionCall { id, .. }
            | Self::FunctionCallOutput { id, .. }
            | Self::ToolSearchCall { id, .. }
            | Self::CustomToolCall { id, .. }
            | Self::CustomToolCallOutput { id, .. }
            | Self::ToolSearchOutput { id, .. }
            | Self::WebSearchCall { id, .. }
            | Self::ImageGenerationCall { id, .. }
            | Self::Compaction { id, .. }
            | Self::ContextCompaction { id, .. } => *id = new_id,
            Self::CompactionTrigger {} | Self::Other(_) => {}
        }
    }

    /// Returns the Responses API item ID prefix for variants that carry an ID.
    #[must_use]
    pub const fn id_prefix(&self) -> Option<&'static str> {
        match self {
            Self::AdditionalTools { .. } => Some("at"),
            Self::Message { .. } => Some("msg"),
            Self::AgentMessage { .. } => Some("amsg"),
            Self::Reasoning { .. } => Some("rs"),
            Self::LocalShellCall { .. } => Some("lsh"),
            Self::FunctionCall { .. } => Some("fc"),
            Self::FunctionCallOutput { .. } => Some("fco"),
            Self::ToolSearchCall { .. } => Some("tsc"),
            Self::CustomToolCall { .. } => Some("ctc"),
            Self::CustomToolCallOutput { .. } => Some("ctco"),
            Self::ToolSearchOutput { .. } => Some("tso"),
            Self::WebSearchCall { .. } => Some("ws"),
            Self::ImageGenerationCall { .. } => Some("ig"),
            Self::Compaction { .. } | Self::ContextCompaction { .. } => Some("cmp"),
            Self::CompactionTrigger {} | Self::Other(_) => None,
        }
    }

    /// Removes the item ID from a derived copy that starts a separate history.
    pub fn strip_id(&mut self) {
        self.set_id(None);
    }

    /// Removes generated identity fields from a copy used to fingerprint a
    /// durable provider step while preserving semantic content provenance.
    #[doc(hidden)]
    pub fn strip_for_durable_identity(&mut self) {
        self.strip_id();
        let Some(metadata) = self.internal_metadata_mut() else {
            return;
        };
        let Some(value) = metadata else {
            return;
        };
        value.create_time = None;
        if value.content_item_kinds.is_none() {
            *metadata = None;
        }
    }

    /// Adds missing first-party history provenance without overwriting provider values.
    #[doc(hidden)]
    pub fn stamp_for_history(&mut self) {
        let compaction_summary = matches!(
            self,
            Self::Compaction { .. } | Self::ContextCompaction { .. }
        );
        let user_kinds = match self {
            Self::Message {
                role: MessageRole::User,
                content,
                ..
            } => Some(content.iter().map(user_content_kind).collect::<Vec<_>>()),
            _ => None,
        };
        let Some(metadata) = self.internal_metadata_mut() else {
            return;
        };
        let metadata = metadata.get_or_insert_with(InternalMessageMetadata::default);
        if metadata.create_time.is_none() {
            let elapsed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            metadata.create_time = serde_json::Number::from_f64(elapsed.as_secs_f64())
                .or_else(|| Some(serde_json::Number::from(elapsed.as_secs())));
        }
        if metadata.content_item_kinds.is_none() {
            metadata.content_item_kinds = user_kinds.or_else(|| {
                compaction_summary
                    .then(|| vec![super::ContentItemKind("compaction.summary".into())])
            });
        }
    }

    const fn internal_metadata_mut(&mut self) -> Option<&mut Option<InternalMessageMetadata>> {
        match self {
            Self::Message {
                internal_chat_message_metadata_passthrough,
                ..
            }
            | Self::AgentMessage {
                internal_chat_message_metadata_passthrough,
                ..
            }
            | Self::Reasoning {
                internal_chat_message_metadata_passthrough,
                ..
            }
            | Self::LocalShellCall {
                internal_chat_message_metadata_passthrough,
                ..
            }
            | Self::FunctionCall {
                internal_chat_message_metadata_passthrough,
                ..
            }
            | Self::FunctionCallOutput {
                internal_chat_message_metadata_passthrough,
                ..
            }
            | Self::ToolSearchCall {
                internal_chat_message_metadata_passthrough,
                ..
            }
            | Self::CustomToolCall {
                internal_chat_message_metadata_passthrough,
                ..
            }
            | Self::CustomToolCallOutput {
                internal_chat_message_metadata_passthrough,
                ..
            }
            | Self::ToolSearchOutput {
                internal_chat_message_metadata_passthrough,
                ..
            }
            | Self::WebSearchCall {
                internal_chat_message_metadata_passthrough,
                ..
            }
            | Self::ImageGenerationCall {
                internal_chat_message_metadata_passthrough,
                ..
            }
            | Self::Compaction {
                internal_chat_message_metadata_passthrough,
                ..
            }
            | Self::ContextCompaction {
                internal_chat_message_metadata_passthrough,
                ..
            } => Some(internal_chat_message_metadata_passthrough),
            Self::AdditionalTools { .. } | Self::CompactionTrigger {} | Self::Other(_) => None,
        }
    }
}

fn user_content_kind(content: &ContentItem) -> super::ContentItemKind {
    let kind = match content {
        ContentItem::InputText { .. } | ContentItem::OutputText { .. } => "user.text",
        ContentItem::InputImage { .. } => "user.image",
        ContentItem::InputAudio { .. } => "user.audio",
    };
    super::ContentItemKind(kind.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn known_items_round_trip_and_unknown_types_map_to_other() {
        let message = r#"{"id":"msg-1","type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}],"phase":"final_answer","internal_chat_message_metadata_passthrough":{"turn_id":"turn-1"}}"#;
        let item: ResponseItem = serde_json::from_str(message).unwrap();
        assert!(matches!(
            item,
            ResponseItem::Message {
                role: MessageRole::Assistant,
                ..
            }
        ));
        assert_eq!(
            serde_json::to_value(&item).unwrap(),
            serde_json::from_str::<Value>(message).unwrap()
        );

        let unknown = r#"{"type":"future_item","id":"future-1","payload":[1,2,3]}"#;
        let item: ResponseItem = serde_json::from_str(unknown).unwrap();
        assert!(matches!(item, ResponseItem::Other(_)));
        assert_eq!(
            serde_json::to_value(&item).unwrap(),
            serde_json::from_str::<Value>(unknown).unwrap()
        );
    }

    #[test]
    fn function_calls_preserve_encrypted_argument_markers() {
        for value in [
            serde_json::json!({
                "type": "function_call",
                "name": "send_message",
                "arguments": "{}",
                "encrypted_function_args": [],
                "call_id": "call-empty"
            }),
            serde_json::json!({
                "type": "function_call",
                "name": "send_message",
                "arguments": "{}",
                "encrypted_function_args": ["opaque"],
                "call_id": "call-opaque"
            }),
        ] {
            let item: ResponseItem = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(item).unwrap(), value);
        }
    }

    #[test]
    fn response_item_ids_distinguish_client_prefixes_from_server_ids() {
        let client = ResponseItemId::with_suffix("msg", "stable");
        let server = ResponseItemId::from_server("server-item-id");

        assert_eq!(client.as_str(), "msg_stable");
        assert!(client.is_prefixed());
        assert!(!server.is_prefixed());
    }

    #[test]
    fn history_stamp_adds_user_kinds_and_preserves_provider_provenance() {
        let mut user = ResponseItem::message(
            MessageRole::User,
            [
                ContentItem::input_text("hello"),
                ContentItem::input_image("https://example.com/image.png"),
                ContentItem::input_audio("data:audio/wav;base64,AA=="),
            ],
        );
        user.stamp_for_history();
        let encoded = serde_json::to_value(&user).unwrap();
        assert!(encoded["internal_chat_message_metadata_passthrough"]["create_time"].is_number());
        assert_eq!(
            encoded["internal_chat_message_metadata_passthrough"]["content_item_kinds"],
            serde_json::json!(["user.text", "user.image", "user.audio"])
        );

        let mut provider: ResponseItem = serde_json::from_value(serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "retained"}],
            "internal_chat_message_metadata_passthrough": {
                "create_time": 1234.567890123,
                "content_item_kinds": ["future.semantic_kind"]
            }
        }))
        .unwrap();
        provider.stamp_for_history();
        let encoded = serde_json::to_value(&provider).unwrap();
        assert_eq!(
            encoded["internal_chat_message_metadata_passthrough"]["create_time"],
            serde_json::json!(1234.567890123)
        );
        assert_eq!(
            encoded["internal_chat_message_metadata_passthrough"]["content_item_kinds"],
            serde_json::json!(["future.semantic_kind"])
        );

        provider.strip_for_durable_identity();
        let durable = serde_json::to_value(provider).unwrap();
        assert!(
            durable["internal_chat_message_metadata_passthrough"]
                .get("create_time")
                .is_none()
        );
        assert_eq!(
            durable["internal_chat_message_metadata_passthrough"]["content_item_kinds"],
            serde_json::json!(["future.semantic_kind"])
        );
    }

    #[test]
    fn output_text_preserves_annotations_logprobs_and_item_status() {
        let message = r#"{"id":"msg-1","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","annotations":[{"type":"url_citation","end_index":4,"start_index":0,"title":"source","url":"https://example.com"}],"logprobs":[{"token":"done","bytes":[100,111,110,101],"logprob":-0.1,"top_logprobs":[{"token":"done","bytes":[100,111,110,101],"logprob":-0.1}]}],"text":"done"}],"phase":"commentary"}"#;
        let item: ResponseItem = serde_json::from_str(message).unwrap();
        assert!(matches!(
            &item,
            ResponseItem::Message {
                role: MessageRole::Assistant,
                content,
                status: Some(ItemStatus::Completed),
                phase: Some(MessagePhase::Commentary),
                ..
            } if matches!(content.as_slice(), [ContentItem::OutputText { text, annotations: Some(annotations), logprobs: Some(logprobs) }] if text.as_ref() == "done" && annotations.len() == 1 && logprobs.len() == 1)
        ));
        assert_eq!(
            serde_json::to_value(&item).unwrap(),
            serde_json::from_str::<Value>(message).unwrap()
        );
    }

    #[test]
    fn auxiliary_api_items_round_trip_without_dynamic_history() {
        let value = serde_json::json!([
            {
                "type": "local_shell_call",
                "id": "lsh-1",
                "call_id": "call-1",
                "status": "completed",
                "action": {
                    "type": "exec",
                    "command": ["echo", "ok"],
                    "timeout_ms": 1000,
                    "working_directory": "/tmp",
                    "env": {"A": "B"},
                    "user": null
                }
            },
            {
                "type": "tool_search_call",
                "id": "tsc-1",
                "call_id": "search-1",
                "status": "completed",
                "execution": "client",
                "arguments": {"query": "tool"}
            },
            {
                "type": "tool_search_output",
                "id": "tso-1",
                "call_id": "search-1",
                "status": "completed",
                "execution": "client",
                "tools": [{"name": "exec"}]
            },
            {
                "type": "web_search_call",
                "id": "ws-1",
                "status": "completed",
                "action": {"type": "search", "query": "weather"}
            },
            {
                "type": "image_generation_call",
                "id": "ig-1",
                "status": "completed",
                "revised_prompt": "revised",
                "result": "AAAA"
            },
            {
                "type": "context_compaction",
                "id": "cmp-1",
                "encrypted_content": "opaque"
            },
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_audio", "audio_url": "data:audio/wav;base64,AAAA"}]
            }
        ]);
        let items: Vec<ResponseItem> = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(items).unwrap(), value);
    }
}
