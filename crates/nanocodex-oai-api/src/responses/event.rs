//! Typed events emitted by the Responses protocol.

use super::ResponseItem;
use serde::{Deserialize, Serialize};

/// Token accounting returned by a completed Responses operation.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Usage {
    #[serde(default)]
    /// Total input tokens.
    pub input_tokens: u64,
    #[serde(default)]
    /// Detailed input-token accounting when supplied.
    pub input_tokens_details: Option<InputTokenDetails>,
    #[serde(default)]
    /// Total output tokens.
    pub output_tokens: u64,
    #[serde(default)]
    /// Detailed output-token accounting when supplied.
    pub output_tokens_details: Option<OutputTokenDetails>,
    #[serde(default)]
    /// Input plus output tokens.
    pub total_tokens: u64,
}

/// Detailed input-token accounting.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct InputTokenDetails {
    #[serde(default)]
    /// Input tokens served from a prompt cache.
    pub cached_tokens: u64,
    #[serde(default)]
    /// Input tokens newly written to a prompt cache.
    pub cache_write_tokens: u64,
}

/// Detailed output-token accounting.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct OutputTokenDetails {
    #[serde(default)]
    /// Output tokens used for reasoning.
    pub reasoning_tokens: u64,
}

#[doc(hidden)]
#[allow(missing_docs)]
#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    #[serde(rename = "response.created")]
    Created {
        #[serde(default)]
        response: Option<serde_json::Value>,
    },
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded {
        #[serde(default)]
        output_index: Option<u32>,
        item: ResponseItem,
    },
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta {
        #[serde(default)]
        output_index: Option<u32>,
        delta: String,
    },
    #[serde(rename = "response.custom_tool_call_input.delta")]
    ToolCallInputDelta {
        #[serde(default)]
        item_id: Option<String>,
        #[serde(default)]
        call_id: Option<String>,
        delta: String,
    },
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta { delta: String, summary_index: i64 },
    #[serde(rename = "response.reasoning_summary.delta")]
    ReasoningSummaryDelta {
        delta: String,
        #[serde(default)]
        summary_index: Option<i64>,
    },
    #[serde(rename = "response.reasoning_summary_text.done")]
    ReasoningSummaryDone {
        item_id: String,
        text: String,
        summary_index: i64,
    },
    #[serde(rename = "response.reasoning_text.delta")]
    ReasoningContentDelta { delta: String, content_index: i64 },
    #[serde(rename = "response.reasoning_summary_part.added")]
    ReasoningSummaryPartAdded { summary_index: i64 },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone { item: ResponseItem },
    #[serde(rename = "response.completed")]
    Completed { response: CompletedResponse },
    #[serde(rename = "response.failed")]
    Failed,
    #[serde(rename = "response.incomplete")]
    Incomplete,
    #[serde(rename = "error")]
    Error,
    #[serde(other)]
    Other,
}

/// One stable, normalized event from a single Responses operation.
///
/// Provider events that do not affect portable response consumption remain in
/// full-fidelity tracing and may be projected by higher-level raw-firehose
/// adapters rather than being promoted into this enum.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ResponseEvent {
    /// The provider accepted and created a response.
    Created,
    /// A complete output item was added to the response.
    OutputItemAdded(ResponseItem),
    /// Incremental assistant output text.
    OutputTextDelta(String),
    /// Incremental custom-tool input.
    ToolCallInputDelta {
        /// Stable output item identity.
        item_id: String,
        /// Stable tool call identity when supplied separately.
        call_id: Option<String>,
        /// Newly received input fragment.
        delta: String,
    },
    /// Incremental reasoning summary text.
    ReasoningSummaryDelta {
        /// Newly received summary fragment.
        delta: String,
        /// Provider summary-part index.
        summary_index: i64,
    },
    /// A complete reasoning summary part.
    ReasoningSummaryDone {
        /// Stable reasoning item identity.
        item_id: String,
        /// Complete summary text.
        text: String,
        /// Provider summary-part index.
        summary_index: i64,
    },
    /// Incremental API-visible reasoning content.
    ReasoningContentDelta {
        /// Newly received reasoning fragment.
        delta: String,
        /// Provider reasoning-content index.
        content_index: i64,
    },
    /// The provider added an empty reasoning summary part.
    ReasoningSummaryPartAdded {
        /// Provider summary-part index.
        summary_index: i64,
    },
    /// A complete output item is ready for aggregation or tool dispatch.
    OutputItemDone(ResponseItem),
    /// The provider terminally completed the response.
    Completed {
        /// Token usage reported for this API operation.
        usage: Option<Usage>,
        /// Whether the model affirmatively ended its logical turn.
        end_turn: Option<bool>,
    },
}

impl ServerEvent {
    #[cfg(feature = "client")]
    pub(crate) fn normalized(&self) -> Option<ResponseEvent> {
        match self {
            Self::Created { .. } => Some(ResponseEvent::Created),
            Self::OutputItemAdded { item, .. } => {
                Some(ResponseEvent::OutputItemAdded(item.clone()))
            }
            Self::OutputTextDelta { delta, .. } => {
                Some(ResponseEvent::OutputTextDelta(delta.clone()))
            }
            Self::ToolCallInputDelta {
                item_id,
                call_id,
                delta,
            } => Some(ResponseEvent::ToolCallInputDelta {
                item_id: item_id.clone().or_else(|| call_id.clone())?,
                call_id: call_id.clone(),
                delta: delta.clone(),
            }),
            Self::ReasoningSummaryTextDelta {
                delta,
                summary_index,
            }
            | Self::ReasoningSummaryDelta {
                delta,
                summary_index: Some(summary_index),
            } => Some(ResponseEvent::ReasoningSummaryDelta {
                delta: delta.clone(),
                summary_index: *summary_index,
            }),
            Self::ReasoningSummaryDone {
                item_id,
                text,
                summary_index,
            } => Some(ResponseEvent::ReasoningSummaryDone {
                item_id: item_id.clone(),
                text: text.clone(),
                summary_index: *summary_index,
            }),
            Self::ReasoningContentDelta {
                delta,
                content_index,
            } => Some(ResponseEvent::ReasoningContentDelta {
                delta: delta.clone(),
                content_index: *content_index,
            }),
            Self::ReasoningSummaryPartAdded { summary_index } => {
                Some(ResponseEvent::ReasoningSummaryPartAdded {
                    summary_index: *summary_index,
                })
            }
            Self::OutputItemDone { item } => Some(ResponseEvent::OutputItemDone(item.clone())),
            Self::Completed { response } => Some(ResponseEvent::Completed {
                usage: response.usage.clone(),
                end_turn: response.end_turn,
            }),
            Self::ReasoningSummaryDelta {
                summary_index: None,
                ..
            }
            | Self::Failed
            | Self::Incomplete
            | Self::Error
            | Self::Other => None,
        }
    }
}

#[doc(hidden)]
#[allow(missing_docs)]
#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum WarmupServerEvent {
    #[serde(rename = "response.created")]
    Created { response: WarmupResponse },
    #[serde(rename = "response.completed")]
    Completed { response: WarmupResponse },
    #[serde(rename = "response.failed")]
    Failed,
    #[serde(rename = "response.incomplete")]
    Incomplete,
    #[serde(rename = "error")]
    Error,
    #[serde(other)]
    Other,
}

#[doc(hidden)]
#[allow(missing_docs)]
#[derive(Deserialize)]
pub struct WarmupResponse {
    pub id: String,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[doc(hidden)]
#[allow(missing_docs)]
#[derive(Deserialize)]
pub struct CompletedResponse {
    pub id: String,
    #[serde(default = "completed_status")]
    pub status: String,
    #[serde(default)]
    pub end_turn: Option<bool>,
    #[serde(default)]
    pub output: Vec<ResponseItem>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

fn completed_status() -> String {
    "completed".to_owned()
}

#[cfg(all(test, feature = "client"))]
mod tests {
    use serde_json::json;

    use super::{ResponseEvent, ServerEvent};

    fn normalized(value: serde_json::Value) -> Option<ResponseEvent> {
        serde_json::from_value::<ServerEvent>(value)
            .unwrap()
            .normalized()
    }

    #[test]
    fn normalizes_the_supported_responses_stream_contract() {
        assert!(matches!(
            normalized(json!({
                "type": "response.created",
                "response": { "id": "resp-1" }
            })),
            Some(ResponseEvent::Created)
        ));
        assert!(matches!(
            normalized(json!({
                "type": "response.output_text.delta",
                "output_index": 0,
                "delta": "hello"
            })),
            Some(ResponseEvent::OutputTextDelta(delta)) if delta == "hello"
        ));
        assert!(matches!(
            normalized(json!({
                "type": "response.custom_tool_call_input.delta",
                "item_id": "ctc-1",
                "call_id": "call-1",
                "delta": "*** Begin"
            })),
            Some(ResponseEvent::ToolCallInputDelta {
                item_id,
                call_id: Some(call_id),
                delta,
            }) if item_id == "ctc-1" && call_id == "call-1" && delta == "*** Begin"
        ));
        assert!(matches!(
            normalized(json!({
                "type": "response.reasoning_summary_text.delta",
                "summary_index": 2,
                "delta": "Inspecting"
            })),
            Some(ResponseEvent::ReasoningSummaryDelta {
                delta,
                summary_index: 2,
            }) if delta == "Inspecting"
        ));
        assert!(matches!(
            normalized(json!({
                "type": "response.reasoning_summary_text.done",
                "item_id": "rs-1",
                "summary_index": 2,
                "text": "Inspecting the parser."
            })),
            Some(ResponseEvent::ReasoningSummaryDone {
                item_id,
                text,
                summary_index: 2,
            }) if item_id == "rs-1" && text == "Inspecting the parser."
        ));
        assert!(matches!(
            normalized(json!({
                "type": "response.reasoning_text.delta",
                "content_index": 1,
                "delta": "private-visible"
            })),
            Some(ResponseEvent::ReasoningContentDelta {
                delta,
                content_index: 1,
            }) if delta == "private-visible"
        ));
        assert!(matches!(
            normalized(json!({
                "type": "response.reasoning_summary_part.added",
                "summary_index": 3
            })),
            Some(ResponseEvent::ReasoningSummaryPartAdded { summary_index: 3 })
        ));
        assert!(matches!(
            normalized(json!({
                "type": "response.completed",
                "response": {
                    "id": "resp-1",
                    "end_turn": true,
                    "usage": {
                        "input_tokens": 5,
                        "output_tokens": 3,
                        "total_tokens": 8
                    }
                }
            })),
            Some(ResponseEvent::Completed {
                usage: Some(usage),
                end_turn: Some(true),
            }) if usage.total_tokens == 8
        ));
    }

    #[test]
    fn does_not_promote_unknown_provider_events() {
        assert!(
            normalized(json!({
                "type": "response.future_metadata",
                "value": "retained raw"
            }))
            .is_none()
        );
    }
}
