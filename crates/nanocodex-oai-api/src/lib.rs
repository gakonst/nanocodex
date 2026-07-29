#![cfg_attr(feature = "client", doc = include_str!("../README.md"))]
#![cfg_attr(not(feature = "client"), doc = include_str!("../CONTRACTS.md"))]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(all(target_family = "wasm", not(target_os = "unknown")))]
compile_error!(
    "nanocodex-oai-api supports native targets and hosted wasm*-unknown-unknown targets; WASI is not yet supported"
);

/// Authentication sources and managed credential snapshots.
#[cfg(feature = "client")]
pub mod auth;
/// Complete typed lifecycle events emitted around Responses operations.
#[cfg(feature = "client")]
pub mod events;
#[cfg(feature = "client")]
mod openai;
/// Automatic `gpt-5.6-sol` USD estimates from provider token usage.
#[cfg(feature = "client")]
pub mod pricing;
/// Complete typed request, event, and item model for the Responses protocol.
pub mod responses;
/// Managed session identities, inputs, and compaction results.
#[cfg(feature = "client")]
pub mod session;
/// Tool contracts shared by agent loops and concrete tool runtimes.
pub mod tools;
/// Generic Tower attempt, service, retry, and streamed-output contracts.
#[cfg(feature = "client")]
pub mod tower;
/// Responses transport policy, errors, and connection statistics.
#[cfg(feature = "client")]
pub mod transport;

use std::{fmt, path::PathBuf, str::FromStr};

use serde::{Deserialize, Serialize};

#[cfg(feature = "client")]
pub(crate) use auth::{OpenAiAuth, OpenAiAuthError, OpenAiAuthMode, OpenAiAuthSnapshot};
#[cfg(feature = "client")]
pub(crate) use events::stream::EventSink;
#[cfg(feature = "client")]
pub(crate) use events::{
    AgentEventData, AgentEventKind, AssistantEvent, ContextEvent, EventError, ModelEvent,
    ReasoningEvent, RunEvent, ToolEvent, TransportEvent, monotonic_now_ns,
};
#[cfg(feature = "client")]
pub(crate) use openai::ModelConfig;
#[cfg(feature = "client")]
pub use openai::{OpenAi, OpenAiBuilder, OpenAiError};
#[cfg(feature = "client")]
pub(crate) use pricing::{CostStatus, EstimatedUsdCost};
pub use responses::ResponseEvent;
pub(crate) use responses::ResponseItem;
#[cfg(feature = "client")]
pub(crate) use responses::{
    ContentItem, FunctionOutputBody, FunctionOutputContent, MessagePhase, MessageRole,
    ResponseItemId, ToolDefinition, Usage,
};
#[cfg(feature = "client")]
pub use session::{
    CompletedResponse, Response, ResponseError, ResponseErrorKind, ResponseTurn, Session,
    SessionBuildError, SessionBuilder,
};
#[cfg(feature = "client")]
pub(crate) use tools::ToolOutputBody;
#[cfg(feature = "client")]
pub(crate) use tower::attempt::{
    ResponsesAttempt, ResponsesAttemptFactory, ResponsesOutput, ResponsesServiceResponse,
    TransportStats,
};
#[cfg(feature = "client")]
pub(crate) use tower::service::ResponsesService;
#[cfg(feature = "client")]
pub(crate) use tower::{
    DefaultResponsesService, ResponsesClient, ResponsesRetryPolicy, ResponsesServiceError,
};
#[cfg(feature = "client")]
pub(crate) use transport::EncodedRequest;
#[cfg(feature = "client")]
pub(crate) use transport::{ResponsesError, ResponsesHistory, ResponsesTransport, RetryAdvice};

#[cfg(feature = "client")]
pub(crate) use tower::{attempt, middleware, service, service_error, stream};
#[cfg(all(feature = "client", not(target_family = "wasm")))]
pub(crate) use transport::{connector, http};
#[cfg(feature = "client")]
pub(crate) use transport::{socket, telemetry};

/// Internal bridge for the higher-level `nanocodex-agent` crate.
///
/// This namespace is not a supported caller API. It keeps mutable model
/// configuration, event emission authority, and attempt construction out of
/// the normal rustdoc surface while allowing the separately versioned agent
/// crate to compose this crate without duplicating those mechanics.
#[doc(hidden)]
#[cfg(feature = "client")]
pub mod __private {
    pub use crate::{
        events::stream::EventSink,
        openai::{
            CallerServiceFactory, LayeredServiceFactory, ModelConfig, ResponsesServiceFactory,
        },
        session::{
            context::{ContextManager, assign_missing_response_item_id},
            state::{ManagedSessionState, ManagedSessionStateError},
        },
        tower::attempt::ResponsesAttemptFactory,
    };

    /// Agent-owned context accounting and compaction policy primitives.
    pub mod compaction {
        pub use crate::session::compaction::{
            auto_compact_token_limit, install_history, trigger,
            trim_tool_outputs_to_fit_context_window,
        };
    }

    /// Decomposes a validated client recipe for the higher-level agent driver.
    pub fn into_openai_parts<F>(openai: crate::OpenAi<F>) -> (ModelConfig, F)
    where
        F: ResponsesServiceFactory,
    {
        openai.into_parts()
    }

    /// Installs the server-visible mapping for tools nested under Code Mode.
    pub fn with_code_mode_tool_names(
        profile: crate::responses::RequestProfile,
        names: Vec<(String, String)>,
    ) -> crate::responses::RequestProfile {
        profile.with_code_mode_tool_names(names)
    }
}

/// The single Responses model contract supported by this SDK.
pub const MODEL: &str = "gpt-5.6-sol";

/// Context-window size of the supported Responses model contract.
pub const CONTEXT_WINDOW_TOKENS: u64 = 272_000;

/// User input for one agent turn.
///
/// Session policy such as the filesystem workspace belongs to the agent
/// builder rather than an individual prompt.
///
/// # Examples
///
/// ```
/// use nanocodex_oai_api::{ImageDetail, Prompt, UserInput};
///
/// let prompt = Prompt::content([
///     UserInput::Text {
///         text: "Describe the deployment diagram.".to_owned(),
///     },
///     UserInput::Image {
///         image_url: "https://example.com/deployment.png".to_owned(),
///         detail: Some(ImageDetail::High),
///     },
/// ]);
///
/// assert!(!prompt.instruction.is_empty());
/// ```
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Prompt {
    /// Ordered text and multimodal content for this turn.
    pub instruction: PromptInput,
}

impl Prompt {
    /// Creates a text-only prompt.
    #[must_use]
    pub fn new(instruction: impl Into<String>) -> Self {
        Self {
            instruction: PromptInput::Text(instruction.into()),
        }
    }

    /// Creates a prompt from ordered content items.
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
    /// A text-only instruction.
    Text(String),
    /// Ordered text and multimodal input items.
    Content(Vec<UserInput>),
}

impl PromptInput {
    /// Returns the total UTF-8 byte length of text items.
    #[must_use]
    pub fn text_bytes(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Content(items) => items.iter().map(UserInput::text_bytes).sum(),
        }
    }

    /// Returns the total Unicode scalar-value count of text items.
    #[must_use]
    pub fn text_chars(&self) -> usize {
        match self {
            Self::Text(text) => text.chars().count(),
            Self::Content(items) => items.iter().map(UserInput::text_chars).sum(),
        }
    }

    /// Returns whether this input contains no non-whitespace text or media.
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
    /// Model-visible text.
    Text {
        /// Text supplied by the user.
        text: String,
    },
    /// An image supplied as a URL or data URL.
    Image {
        /// Image URL visible to the model.
        image_url: String,
        /// Optional image-detail policy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
    },
    /// An image loaded from the local filesystem by a native runtime.
    LocalImage {
        /// Path to the local image.
        path: PathBuf,
        /// Optional image-detail policy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
    },
    /// A reserved remote audio input.
    Audio {
        /// Audio URL retained by the input contract.
        audio_url: String,
    },
    /// A reserved local audio input.
    LocalAudio {
        /// Path retained by the input contract.
        path: PathBuf,
    },
}

/// Image fidelity requested from the model.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail {
    /// Lets the provider select the detail level.
    Auto,
    /// Requests lower-resolution image processing.
    Low,
    /// Requests high-resolution image processing.
    High,
    /// Requests original-resolution image processing.
    Original,
}

impl UserInput {
    /// Returns the UTF-8 byte length when this is text, or zero for media.
    #[must_use]
    pub const fn text_bytes(&self) -> usize {
        match self {
            Self::Text { text } => text.len(),
            Self::Image { .. }
            | Self::LocalImage { .. }
            | Self::Audio { .. }
            | Self::LocalAudio { .. } => 0,
        }
    }

    /// Returns the Unicode scalar-value count when this is text, or zero for media.
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

    /// Returns whether this item contains neither non-whitespace text nor media.
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

/// Responses reasoning execution mode for the supported GPT-5.6 model family.
///
/// Standard mode preserves the default request behavior. Pro mode performs
/// additional model work before returning one final answer and can increase
/// latency and token usage independently of [`Thinking`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReasoningMode {
    /// Standard reasoning behavior.
    #[default]
    Standard,
    /// Pro reasoning behavior.
    Pro,
}

impl ReasoningMode {
    /// Returns the request value used by the Responses API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Pro => "pro",
        }
    }

    #[cfg(feature = "client")]
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

/// Requested model reasoning effort.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Thinking {
    /// Disable reasoning when supported.
    None,
    /// Low reasoning effort.
    Low,
    /// Medium reasoning effort.
    Medium,
    /// High reasoning effort.
    #[default]
    High,
    /// Extra-high reasoning effort.
    Xhigh,
    /// Maximum reasoning effort.
    Max,
}

impl Thinking {
    /// Returns the request value used by the Responses API.
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
