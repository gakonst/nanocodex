//! Typed events emitted by the Responses protocol.

use super::ResponseItem;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub input_tokens_details: Option<InputTokenDetails>,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub output_tokens_details: Option<OutputTokenDetails>,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct InputTokenDetails {
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct OutputTokenDetails {
    #[serde(default)]
    pub reasoning_tokens: u64,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    #[serde(rename = "response.created")]
    Created,
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
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta { delta: String },
    #[serde(rename = "response.reasoning_summary.delta")]
    ReasoningSummaryDelta { delta: String },
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

#[derive(Deserialize)]
pub struct WarmupResponse {
    pub id: String,
    #[serde(default)]
    pub usage: Option<Usage>,
}

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
