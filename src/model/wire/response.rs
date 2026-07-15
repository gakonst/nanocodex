use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::model) struct Usage {
    pub(in crate::model) input_tokens: u64,
    pub(in crate::model) input_tokens_details: InputTokenDetails,
    pub(in crate::model) output_tokens: u64,
    pub(in crate::model) output_tokens_details: OutputTokenDetails,
    pub(in crate::model) total_tokens: u64,
}

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::model) struct InputTokenDetails {
    pub(in crate::model) cached_tokens: u64,
    pub(in crate::model) cache_write_tokens: u64,
}

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::model) struct OutputTokenDetails {
    pub(in crate::model) reasoning_tokens: u64,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(in crate::model) enum ServerEvent {
    #[serde(rename = "response.created")]
    Created { response: CreatedResponse },
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta { delta: String },
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta { delta: String },
    #[serde(rename = "response.reasoning_summary.delta")]
    ReasoningSummaryDelta { delta: String },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone { item: OutputItem },
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

impl ServerEvent {
    pub(in crate::model) const fn is_output(&self) -> bool {
        matches!(
            self,
            Self::OutputTextDelta { .. }
                | Self::ReasoningSummaryTextDelta { .. }
                | Self::ReasoningSummaryDelta { .. }
                | Self::OutputItemDone { .. }
        )
    }
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
pub(in crate::model) struct CreatedResponse {
    pub(in crate::model) id: String,
}

#[derive(Deserialize)]
pub(in crate::model) struct CompletedResponse {
    pub(in crate::model) id: String,
    pub(in crate::model) status: String,
    pub(in crate::model) output: Vec<OutputItem>,
    pub(in crate::model) usage: Usage,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(in crate::model) enum OutputItem {
    #[serde(rename = "shell_call")]
    ShellCall {
        call_id: String,
        action: ShellAction,
        caller: Caller,
        #[serde(default)]
        created_by: Option<Value>,
    },
    #[serde(rename = "message")]
    Message {
        #[serde(default)]
        content: Vec<OutputContent>,
    },
    #[serde(rename = "program")]
    Program,
    #[serde(rename = "program_output")]
    ProgramOutput,
    #[serde(other)]
    Other,
}

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::model) struct ShellAction {
    pub(in crate::model) commands: Vec<String>,
    #[serde(default)]
    pub(in crate::model) timeout_ms: Option<i64>,
    #[serde(default)]
    pub(in crate::model) max_output_length: Option<i64>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub(in crate::model) enum Caller {
    #[serde(rename = "program")]
    Program { caller_id: String },
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(in crate::model) enum OutputContent {
    #[serde(rename = "output_text")]
    OutputText { text: String },
    #[serde(other)]
    Other,
}
