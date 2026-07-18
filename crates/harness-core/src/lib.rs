mod events;
pub mod responses;

use std::{fmt, path::PathBuf, str::FromStr};

use serde::{Deserialize, Serialize};

pub use events::{AgentEvent, AgentEventKind, AgentEvents, EventError, EventSink};
pub use responses::{
    AgentMessageContent, ContentItem, CustomToolFormat, FunctionOutputBody, FunctionOutputContent,
    InternalMessageMetadata, ItemStatus, JsonSchema, JsonValue, LocalShellAction,
    LocalShellExecAction, LocalShellStatus, MessagePhase, MessageRole, OutputTextAnnotation,
    OutputTextLogprob, OutputTextTopLogprob, ReasoningContent, ReasoningSummary, ResponseItem,
    ToolCaller, ToolDefinition, Usage, WebSearchAction,
};

const SYSTEM_PROMPT: &str = include_str!("../prompts/system.md");

/// Input for one agent turn.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Prompt {
    pub instruction: PromptInput,
    #[serde(default)]
    pub workspace: Option<String>,
}

impl Prompt {
    #[must_use]
    pub fn new(instruction: impl Into<String>) -> Self {
        Self {
            instruction: PromptInput::Text(instruction.into()),
            workspace: None,
        }
    }

    /// Creates a prompt from ordered text, image, and audio input items.
    #[must_use]
    pub fn content(input: impl IntoIterator<Item = UserInput>) -> Self {
        Self {
            instruction: PromptInput::Content(input.into_iter().collect()),
            workspace: None,
        }
    }

    #[must_use]
    pub fn workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }
}

/// Ordered input for one agent turn.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum PromptInput {
    Text(String),
    Content(Vec<UserInput>),
}

impl PromptInput {
    #[must_use]
    pub fn text_bytes(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Content(items) => items.iter().map(UserInput::text_bytes).sum(),
        }
    }

    #[must_use]
    pub fn text_chars(&self) -> usize {
        match self {
            Self::Text(text) => text.chars().count(),
            Self::Content(items) => items.iter().map(UserInput::text_chars).sum(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Text(text) => text.trim().is_empty(),
            Self::Content(items) => items.is_empty() || items.iter().all(UserInput::is_empty),
        }
    }
}

impl From<String> for PromptInput {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for PromptInput {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

/// One ordered user-supplied prompt item.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum UserInput {
    Text {
        text: String,
    },
    Image {
        image_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
    },
    LocalImage {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
    },
    Audio {
        audio_url: String,
    },
    LocalAudio {
        path: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail {
    Auto,
    Low,
    High,
    Original,
}

impl UserInput {
    #[must_use]
    pub fn text_bytes(&self) -> usize {
        match self {
            Self::Text { text } => text.len(),
            Self::Image { .. }
            | Self::LocalImage { .. }
            | Self::Audio { .. }
            | Self::LocalAudio { .. } => 0,
        }
    }

    #[must_use]
    pub fn text_chars(&self) -> usize {
        match self {
            Self::Text { text } => text.chars().count(),
            Self::Image { .. }
            | Self::LocalImage { .. }
            | Self::Audio { .. }
            | Self::LocalAudio { .. } => 0,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Text { text } => text.trim().is_empty(),
            Self::Image { .. }
            | Self::LocalImage { .. }
            | Self::Audio { .. }
            | Self::LocalAudio { .. } => false,
        }
    }
}

/// OpenAI-specific settings for the deliberately single-provider harness.
#[derive(Clone)]
pub struct ModelConfig {
    pub model: String,
    pub api_key: String,
    pub effort: ReasoningEffort,
    pub web_search: bool,
    pub websocket_url: String,
    pub api_base_url: String,
}

impl ModelConfig {
    #[must_use]
    pub const fn orchestration() -> &'static str {
        "local_code_mode"
    }

    #[must_use]
    pub const fn system_prompt() -> &'static str {
        SYSTEM_PROMPT
    }

    #[must_use]
    pub fn search_endpoint(&self) -> String {
        format!("{}/alpha/search", self.api_base_url.trim_end_matches('/'))
    }
}

#[derive(Clone, Copy, Default)]
pub enum ReasoningEffort {
    #[default]
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ReasoningEffort {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ReasoningEffort {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::Xhigh),
            "max" => Ok(Self::Max),
            _ => Err(format!(
                "invalid reasoning effort {value:?}; expected low, medium, high, xhigh, or max"
            )),
        }
    }
}
