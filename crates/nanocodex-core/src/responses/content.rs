use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ImageDetail;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    Developer,
    User,
    Assistant,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePhase {
    Commentary,
    FinalAnswer,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    InProgress,
    Completed,
    Incomplete,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InternalMessageMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<Box<str>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    InputText {
        text: Box<str>,
    },
    InputImage {
        image_url: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
    },
    InputAudio {
        audio_url: Box<str>,
    },
    OutputText {
        text: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotations: Option<Vec<OutputTextAnnotation>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        logprobs: Option<Vec<OutputTextLogprob>>,
    },
}

impl ContentItem {
    #[must_use]
    pub fn output_text(text: impl Into<Box<str>>) -> Self {
        Self::OutputText {
            text: text.into(),
            annotations: None,
            logprobs: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalShellStatus {
    Completed,
    InProgress,
    Incomplete,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocalShellAction {
    Exec(LocalShellExecAction),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LocalShellExecAction {
    pub command: Vec<String>,
    pub timeout_ms: Option<u64>,
    pub working_directory: Option<Box<str>>,
    pub env: Option<HashMap<Box<str>, Box<str>>>,
    pub user: Option<Box<str>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebSearchAction {
    Search {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queries: Option<Vec<Box<str>>>,
    },
    OpenPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<Box<str>>,
    },
    FindInPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<Box<str>>,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputTextAnnotation {
    FileCitation {
        file_id: Box<str>,
        filename: Box<str>,
        index: u64,
    },
    UrlCitation {
        end_index: u64,
        start_index: u64,
        title: Box<str>,
        url: Box<str>,
    },
    ContainerFileCitation {
        container_id: Box<str>,
        end_index: u64,
        file_id: Box<str>,
        filename: Box<str>,
        start_index: u64,
    },
    FilePath {
        file_id: Box<str>,
        index: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OutputTextLogprob {
    pub token: Box<str>,
    pub bytes: Vec<u8>,
    pub logprob: f64,
    pub top_logprobs: Vec<OutputTextTopLogprob>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OutputTextTopLogprob {
    pub token: Box<str>,
    pub bytes: Vec<u8>,
    pub logprob: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolCaller {
    Direct,
    Program { caller_id: Box<str> },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessageContent {
    InputText { text: Box<str> },
    EncryptedContent { encrypted_content: Box<str> },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningSummary {
    SummaryText { text: Box<str> },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningContent {
    ReasoningText { text: Box<str> },
    Text { text: Box<str> },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum FunctionOutputBody {
    Text(Box<str>),
    Content(Vec<FunctionOutputContent>),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FunctionOutputContent {
    InputText {
        text: Box<str>,
    },
    InputImage {
        image_url: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
    },
    EncryptedContent {
        encrypted_content: Box<str>,
    },
}
