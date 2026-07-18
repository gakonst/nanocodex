use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Default, Deserialize, Serialize)]
pub(in crate::model) struct Usage {
    #[serde(default)]
    pub(in crate::model) input_tokens: u64,
    #[serde(default)]
    pub(in crate::model) input_tokens_details: Option<InputTokenDetails>,
    #[serde(default)]
    pub(in crate::model) output_tokens: u64,
    #[serde(default)]
    pub(in crate::model) output_tokens_details: Option<OutputTokenDetails>,
    #[serde(default)]
    pub(in crate::model) total_tokens: u64,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(in crate::model) struct InputTokenDetails {
    #[serde(default)]
    pub(in crate::model) cached_tokens: u64,
    #[serde(default)]
    pub(in crate::model) cache_write_tokens: u64,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(in crate::model) struct OutputTokenDetails {
    #[serde(default)]
    pub(in crate::model) reasoning_tokens: u64,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(in crate::model) enum ServerEvent {
    #[serde(rename = "response.created")]
    Created,
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta { delta: String },
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta { delta: String },
    #[serde(rename = "response.reasoning_summary.delta")]
    ReasoningSummaryDelta { delta: String },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone { item: Value },
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
pub(in crate::model) enum WarmupServerEvent {
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
pub(in crate::model) struct WarmupResponse {
    pub(in crate::model) id: String,
    #[serde(default)]
    pub(in crate::model) usage: Option<Usage>,
}

#[derive(Deserialize)]
pub(in crate::model) struct CompletedResponse {
    pub(in crate::model) id: String,
    #[serde(default = "completed_status")]
    pub(in crate::model) status: String,
    #[serde(default)]
    pub(in crate::model) end_turn: Option<bool>,
    #[serde(default)]
    pub(in crate::model) output: Vec<Value>,
    #[serde(default)]
    pub(in crate::model) usage: Option<Usage>,
}

fn completed_status() -> String {
    "completed".to_owned()
}
