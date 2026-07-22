mod auth;
mod events;
pub mod responses;

use std::{fmt, path::PathBuf, str::FromStr, sync::Arc};

use serde::{Deserialize, Serialize};

pub use auth::{
    OpenAiAuth, OpenAiAuthError, OpenAiAuthFuture, OpenAiAuthMode, OpenAiAuthSnapshot,
    OpenAiAuthSource,
};
pub use events::{
    AgentEvent, AgentEventKind, AgentEventTiming, AgentEvents, EventError, EventSink,
    TimedAgentEvent, monotonic_now_ns,
};
pub use responses::{
    AgentMessageContent, ContentItem, CustomToolFormat, FunctionOutputBody, FunctionOutputContent,
    InternalMessageMetadata, ItemStatus, JsonSchema, JsonValue, LocalShellAction,
    LocalShellExecAction, LocalShellStatus, MessagePhase, MessageRole, OutputTextAnnotation,
    OutputTextLogprob, OutputTextTopLogprob, ReasoningContent, ReasoningSummary, ResponseItem,
    ToolCaller, ToolDefinition, Usage, WebSearchAction,
};

const SYSTEM_PROMPT: &str = include_str!("../prompts/system.md");

/// The single Responses model contract supported by this SDK.
pub const MODEL: &str = "gpt-5.6-sol";

/// User input for one agent turn.
///
/// Session policy such as the filesystem workspace belongs to the agent
/// builder rather than an individual prompt.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Prompt {
    pub instruction: PromptInput,
}

impl Prompt {
    #[must_use]
    pub fn new(instruction: impl Into<String>) -> Self {
        Self {
            instruction: PromptInput::Text(instruction.into()),
        }
    }

    /// Creates a prompt from ordered text, image, and audio input items.
    #[must_use]
    pub fn content(input: impl IntoIterator<Item = UserInput>) -> Self {
        Self {
            instruction: PromptInput::Content(input.into_iter().collect()),
        }
    }
}

impl From<String> for Prompt {
    fn from(instruction: String) -> Self {
        Self::new(instruction)
    }
}

impl From<&str> for Prompt {
    fn from(instruction: &str) -> Self {
        Self::new(instruction)
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

/// OpenAI-specific settings for the deliberately single-provider nanocodex.
#[derive(Clone)]
pub struct ModelConfig {
    pub auth: OpenAiAuth,
    pub reasoning_mode: ReasoningMode,
    pub thinking: Thinking,
    pub websocket_url: String,
    pub api_base_url: String,
    pub system_prompt: Arc<str>,
}

impl ModelConfig {
    #[must_use]
    pub const fn orchestration() -> &'static str {
        "local_code_mode"
    }

    #[must_use]
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    #[must_use]
    pub fn search_endpoint(&self) -> String {
        format!("{}/alpha/search", self.api_base_url.trim_end_matches('/'))
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            auth: OpenAiAuth::api_key(String::new()),
            reasoning_mode: ReasoningMode::default(),
            thinking: Thinking::default(),
            websocket_url: "wss://api.openai.com/v1/responses".to_owned(),
            api_base_url: "https://api.openai.com/v1".to_owned(),
            system_prompt: SYSTEM_PROMPT.into(),
        }
    }
}

/// Responses reasoning execution mode for the supported GPT-5.6 model family.
///
/// Standard mode preserves the default request behavior. Pro mode performs
/// additional model work before returning one final answer and can increase
/// latency and token usage independently of [`Thinking`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReasoningMode {
    #[default]
    Standard,
    Pro,
}

impl ReasoningMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Pro => "pro",
        }
    }

    pub(crate) const fn request_value(self) -> Option<&'static str> {
        match self {
            Self::Standard => None,
            Self::Pro => Some("pro"),
        }
    }
}

impl fmt::Display for ReasoningMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ReasoningMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "standard" => Ok(Self::Standard),
            "pro" => Ok(Self::Pro),
            _ => Err(format!(
                "invalid reasoning mode {value:?}; expected standard or pro"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Thinking {
    None,
    Low,
    #[default]
    Medium,
    High,
    Xhigh,
    Max,
}

impl Thinking {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

impl fmt::Display for Thinking {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Thinking {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "none" => Ok(Self::None),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::Xhigh),
            "max" => Ok(Self::Max),
            _ => Err(format!(
                "invalid reasoning effort {value:?}; expected none, low, medium, high, xhigh, or max"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Prompt, ReasoningMode, Thinking};

    #[test]
    fn reasoning_configuration_parses_every_public_value() {
        assert_eq!("standard".parse(), Ok(ReasoningMode::Standard));
        assert_eq!("pro".parse(), Ok(ReasoningMode::Pro));

        for (value, expected) in [
            ("none", Thinking::None),
            ("low", Thinking::Low),
            ("medium", Thinking::Medium),
            ("high", Thinking::High),
            ("xhigh", Thinking::Xhigh),
            ("max", Thinking::Max),
        ] {
            assert_eq!(value.parse(), Ok(expected));
        }
    }

    #[test]
    fn prompt_serialization_contains_only_user_input() {
        let prompt = Prompt::new("inspect the repository");
        assert_eq!(
            serde_json::to_value(prompt).unwrap(),
            json!({ "instruction": "inspect the repository" })
        );
    }

    #[test]
    fn prompt_deserialization_rejects_session_policy() {
        let error = serde_json::from_value::<Prompt>(json!({
            "instruction": "inspect the repository",
            "workspace": "/work/project"
        }))
        .unwrap_err();
        assert!(error.to_string().contains("unknown field `workspace`"));
    }
}
