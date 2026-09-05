//! Typed content nested inside Responses input and output items.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ImageDetail;

/// Role of a message in the Responses transcript.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// Stable application or harness instructions.
    Developer,
    /// User-supplied input.
    User,
    /// Model output.
    Assistant,
}

/// Presentation phase attached to an assistant message.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePhase {
    /// Intermediate commentary.
    Commentary,
    /// Terminal answer content.
    FinalAnswer,
}

/// Provider lifecycle status attached to an item.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    /// Work is still in progress.
    InProgress,
    /// Work completed successfully.
    Completed,
    /// Work ended before completion.
    Incomplete,
    /// Work failed.
    Failed,
}

/// Opaque first-party metadata retained with a message.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InternalMessageMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Provider turn identity when supplied.
    pub turn_id: Option<Box<str>>,
}

/// One ordered multimodal part of a message.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    /// Model-visible text input.
    InputText {
        /// Input text.
        text: Box<str>,
    },
    /// Model-visible image input.
    InputImage {
        /// Data URL or remote image URL.
        image_url: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// Requested image-detail policy.
        detail: Option<ImageDetail>,
    },
    /// Model-visible audio input.
    InputAudio {
        /// Data URL or remote audio URL.
        audio_url: Box<str>,
    },
    /// Assistant text output.
    OutputText {
        /// Output text.
        text: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// Provider citations and file annotations.
        annotations: Option<Vec<OutputTextAnnotation>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// Token log probabilities when requested.
        logprobs: Option<Vec<OutputTextLogprob>>,
    },
}

impl ContentItem {
    /// Creates model-visible text input.
    #[must_use]
    pub fn input_text(text: impl Into<Box<str>>) -> Self {
        Self::InputText { text: text.into() }
    }

    /// Creates model-visible image input with automatic detail selection.
    #[must_use]
    pub fn input_image(image_url: impl Into<Box<str>>) -> Self {
        Self::InputImage {
            image_url: image_url.into(),
            detail: None,
        }
    }

    /// Creates model-visible image input with an explicit detail policy.
    #[must_use]
    pub fn input_image_with_detail(image_url: impl Into<Box<str>>, detail: ImageDetail) -> Self {
        Self::InputImage {
            image_url: image_url.into(),
            detail: Some(detail),
        }
    }

    /// Creates model-visible audio input.
    #[must_use]
    pub fn input_audio(audio_url: impl Into<Box<str>>) -> Self {
        Self::InputAudio {
            audio_url: audio_url.into(),
        }
    }

    /// Creates assistant output text without annotations or log probabilities.
    #[must_use]
    pub fn output_text(text: impl Into<Box<str>>) -> Self {
        Self::OutputText {
            text: text.into(),
            annotations: None,
            logprobs: None,
        }
    }
}

/// Lifecycle status of a provider local-shell call.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalShellStatus {
    /// The command completed.
    Completed,
    /// The command remains in progress.
    InProgress,
    /// The command ended before completion.
    Incomplete,
}

/// Action requested by a provider local-shell call.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocalShellAction {
    /// Execute a process.
    Exec(LocalShellExecAction),
}

/// Process execution parameters from a provider local-shell call.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LocalShellExecAction {
    /// Command and arguments.
    pub command: Vec<String>,
    /// Optional timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Optional working directory.
    pub working_directory: Option<Box<str>>,
    /// Optional environment overrides.
    pub env: Option<HashMap<Box<str>, Box<str>>>,
    /// Optional operating-system user.
    pub user: Option<Box<str>>,
}

/// Action reported by a provider web-search call.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebSearchAction {
    /// Search the web with one or more queries.
    Search {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// Single query shape.
        query: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// Batched query shape.
        queries: Option<Vec<Box<str>>>,
    },
    /// Open a page.
    OpenPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// Target URL.
        url: Option<Box<str>>,
    },
    /// Find text inside a page.
    FindInPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// Target URL.
        url: Option<Box<str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// Search pattern.
        pattern: Option<Box<str>>,
    },
    /// A forward-compatible action retained without interpretation.
    #[serde(other)]
    Other,
}

/// Annotation attached to assistant output text.
#[allow(missing_docs)]
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

/// Log probability for one output token.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OutputTextLogprob {
    /// Decoded token text.
    pub token: Box<str>,
    /// Raw token bytes.
    pub bytes: Vec<u8>,
    /// Natural-log probability.
    pub logprob: f64,
    /// Highest-probability alternatives.
    pub top_logprobs: Vec<OutputTextTopLogprob>,
}

/// Alternative token and probability at one output position.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OutputTextTopLogprob {
    /// Decoded token text.
    pub token: Box<str>,
    /// Raw token bytes.
    pub bytes: Vec<u8>,
    /// Natural-log probability.
    pub logprob: f64,
}

/// Origin of a tool call.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolCaller {
    /// The model called the tool directly.
    Direct,
    /// A code-mode program called the tool.
    Program {
        /// Calling program identity.
        caller_id: Box<str>,
    },
}

/// Content carried by an agent-to-agent message.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessageContent {
    /// Plain model-visible text.
    InputText {
        /// Message text.
        text: Box<str>,
    },
    /// Opaque encrypted continuation content.
    EncryptedContent {
        /// Provider-generated encrypted payload.
        encrypted_content: Box<str>,
    },
}

/// One completed reasoning summary part.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningSummary {
    /// API-visible summary text.
    SummaryText {
        /// Summary text.
        text: Box<str>,
    },
}

/// API-visible content nested inside a reasoning item.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningContent {
    /// Reasoning-text wire shape.
    ReasoningText {
        /// Reasoning content.
        text: Box<str>,
    },
    /// Legacy plain-text wire shape.
    Text {
        /// Reasoning content.
        text: Box<str>,
    },
}

/// Output returned for a function or custom-tool call.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum FunctionOutputBody {
    /// Plain text output.
    Text(Box<str>),
    /// Ordered multimodal output.
    Content(Vec<FunctionOutputContent>),
}

/// One multimodal part of a function or custom-tool output.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FunctionOutputContent {
    /// Text output.
    InputText {
        /// Output text.
        text: Box<str>,
    },
    /// Image output.
    InputImage {
        /// Data URL or remote image URL.
        image_url: Box<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// Requested image-detail policy.
        detail: Option<ImageDetail>,
    },
    /// Audio output.
    InputAudio {
        /// Data URL or remote audio URL.
        audio_url: Box<str>,
    },
    /// Opaque encrypted provider content.
    EncryptedContent {
        /// Provider-generated encrypted payload.
        encrypted_content: Box<str>,
    },
}
