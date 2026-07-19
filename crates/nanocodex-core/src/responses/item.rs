//! Conversation items sent to and received from the Responses protocol.

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use super::{
    AgentMessageContent, ContentItem, FunctionOutputBody, InternalMessageMetadata, ItemStatus,
    JsonValue, LocalShellAction, LocalShellStatus, MessagePhase, MessageRole, ReasoningContent,
    ReasoningSummary, ToolCaller, ToolDefinition, WebSearchAction,
};

/// A strongly typed conversation item sent to or received from the Responses API.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseItem {
    AdditionalTools {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<Box<str>>,
        role: MessageRole,
        tools: Vec<ToolDefinition>,
    },
    Message {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<Box<str>>,
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
        id: Option<Box<str>>,
        author: Box<str>,
        recipient: Box<str>,
        content: SmallVec<[AgentMessageContent; 1]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    Reasoning {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<Box<str>>,
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
        id: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<Box<str>>,
        status: LocalShellStatus,
        action: LocalShellAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    FunctionCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<Box<str>>,
        name: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<Box<str>>,
        arguments: Box<str>,
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
        id: Option<Box<str>>,
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
        id: Option<Box<str>>,
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
        id: Option<Box<str>>,
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
        id: Option<Box<str>>,
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
        id: Option<Box<str>>,
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
        id: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<WebSearchAction>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    ImageGenerationCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<Box<str>>,
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
        id: Option<Box<str>>,
        encrypted_content: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        created_by: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    CompactionTrigger {},
    ContextCompaction {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        internal_chat_message_metadata_passthrough: Option<InternalMessageMetadata>,
    },
    #[serde(untagged)]
    Other(JsonValue),
}

impl ResponseItem {
    #[must_use]
    pub fn additional_tools(tools: Vec<ToolDefinition>) -> Self {
        Self::AdditionalTools {
            id: None,
            role: MessageRole::Developer,
            tools,
        }
    }

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

    #[must_use]
    pub const fn compaction_trigger() -> Self {
        Self::CompactionTrigger {}
    }

    #[must_use]
    pub fn is_user_message(&self) -> bool {
        matches!(
            self,
            Self::Message {
                role: MessageRole::User,
                ..
            }
        )
    }

    /// Removes a server-assigned item ID before replaying local history.
    pub fn strip_id(&mut self) {
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
            | Self::ContextCompaction { id, .. } => *id = None,
            Self::CompactionTrigger {} | Self::Other(_) => {}
        }
    }
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
