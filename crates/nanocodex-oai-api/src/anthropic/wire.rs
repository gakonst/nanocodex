//! Wire types for the Anthropic Messages API.
//!
//! Request types serialize; streaming event types deserialize. Deserialized types
//! deliberately tolerate unknown fields so that new server-side block and delta kinds
//! degrade to [`ContentBlock::Other`] / [`Delta::Other`] rather than failing the turn.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A `POST /v1/messages` request body.
#[derive(Clone, Debug, Serialize)]
pub struct MessagesRequest {
    pub model: String,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub system: Vec<SystemBlock>,
    pub messages: Vec<InputMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,
    pub stream: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl SystemBlock {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            kind: "text",
            text: text.into(),
            cache_control: None,
        }
    }

    /// Marks this block as the end of the cacheable prefix.
    #[must_use]
    pub const fn cached(mut self) -> Self {
        self.cache_control = Some(CacheControl::ephemeral());
        self
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

impl CacheControl {
    #[must_use]
    pub const fn ephemeral() -> Self {
        Self { kind: "ephemeral" }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug, Serialize)]
pub struct InputMessage {
    pub role: Role,
    pub content: Vec<InputBlock>,
}

/// A content block sent to the model.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputBlock {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Url { url: String },
    Base64 { media_type: String, data: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThinkingConfig {
    Adaptive {
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<&'static str>,
    },
    Disabled,
}

#[derive(Clone, Debug, Serialize)]
pub struct OutputConfig {
    pub effort: &'static str,
}

// ---------------------------------------------------------------------------
// Streaming events
// ---------------------------------------------------------------------------

/// One server-sent event from a streaming Messages response.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart {
        message: StreamMessage,
    },
    ContentBlockStart {
        index: u32,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        index: u32,
        delta: Delta,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        delta: MessageDeltaBody,
        #[serde(default)]
        usage: Option<MessageUsage>,
    },
    MessageStop,
    Ping,
    Error {
        #[serde(default)]
        error: Option<ApiError>,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize)]
pub struct StreamMessage {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Option<MessageUsage>,
}

/// The opening shape of a content block. Text and thinking blocks stream their body
/// through deltas; `tool_use` streams its arguments through `input_json_delta`.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    RedactedThinking {
        #[serde(default)]
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Option<Value>,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(
    clippy::enum_variant_names,
    reason = "variants mirror Anthropic wire discriminator names"
)]
pub enum Delta {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    SignatureDelta {
        signature: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct MessageDeltaBody {
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub stop_sequence: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct MessageUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ApiError {
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::{ContentBlock, Delta, StreamEvent};

    fn event(json: &str) -> StreamEvent {
        serde_json::from_str(json).expect("event should deserialize")
    }

    #[test]
    fn text_and_tool_use_blocks_deserialize() {
        let StreamEvent::ContentBlockStart { content_block, .. } = event(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ) else {
            panic!("expected a content block start");
        };
        assert!(matches!(content_block, ContentBlock::Text { .. }));

        let StreamEvent::ContentBlockStart { content_block, .. } = event(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"shell","input":{}}}"#,
        ) else {
            panic!("expected a content block start");
        };
        let ContentBlock::ToolUse { id, name, .. } = content_block else {
            panic!("expected a tool_use block");
        };
        assert_eq!(id, "toolu_1");
        assert_eq!(name, "shell");
    }

    #[test]
    fn unknown_block_and_delta_kinds_degrade_instead_of_failing() {
        let StreamEvent::ContentBlockStart { content_block, .. } = event(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"x"}}"#,
        ) else {
            panic!("expected a content block start");
        };
        assert!(matches!(content_block, ContentBlock::Other));

        let StreamEvent::ContentBlockDelta { delta, .. } = event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"citations_delta","citation":{}}}"#,
        ) else {
            panic!("expected a content block delta");
        };
        assert!(matches!(delta, Delta::Other));

        assert!(matches!(
            event(r#"{"type":"some_future_event","payload":1}"#),
            StreamEvent::Other
        ));
    }

    #[test]
    fn message_delta_carries_stop_reason_and_usage() {
        let StreamEvent::MessageDelta { delta, usage } = event(
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}"#,
        ) else {
            panic!("expected a message delta");
        };
        assert_eq!(delta.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(usage.expect("usage").output_tokens, 42);
    }

    #[test]
    fn input_json_deltas_carry_partial_arguments() {
        let StreamEvent::ContentBlockDelta { delta, .. } = event(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}"#,
        ) else {
            panic!("expected a content block delta");
        };
        let Delta::InputJsonDelta { partial_json } = delta else {
            panic!("expected an input_json delta");
        };
        assert_eq!(partial_json, "{\"a\":");
    }

    #[test]
    fn signature_deltas_retain_the_opaque_signature() {
        let StreamEvent::ContentBlockDelta { delta, .. } = event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"opaque-signature"}}"#,
        ) else {
            panic!("expected a content block delta");
        };
        let Delta::SignatureDelta { signature } = delta else {
            panic!("expected a signature delta");
        };
        assert_eq!(signature, "opaque-signature");
    }
}
