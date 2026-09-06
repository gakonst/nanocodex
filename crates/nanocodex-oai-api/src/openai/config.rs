use std::{borrow::Cow, sync::Arc};

use crate::{
    CONTEXT_WINDOW_TOKENS, Model, OpenAiAuth, ReasoningMode, ResponsesHistory, ResponsesTransport,
    Thinking,
};

const SYSTEM_PROMPT: &str = include_str!("../../prompts/system.md");
const ASTRA_SYSTEM_PROMPT: &str = include_str!("../../prompts/astra.md");

/// Validated, read-only settings passed to a [`ResponsesServiceFactory`].
///
/// Public policy is configured through [`OpenAiBuilder`]. A custom factory can
/// inspect this snapshot while constructing an independent session service.
///
/// [`OpenAiBuilder`]: super::OpenAiBuilder
/// [`ResponsesServiceFactory`]: super::ResponsesServiceFactory
#[derive(Clone)]
pub struct ModelConfig {
    /// Selected OpenAI coding model.
    pub model: Model,
    /// Optional namespace prepended to the model identifier on the wire.
    ///
    /// This preserves Nanocodex's closed typed model policy while allowing an
    /// OpenAI routing gateway to require IDs such as `openai/gpt-5.6-sol`.
    pub model_id_prefix: Option<Arc<str>>,
    /// Authentication source resolved for each transport connection.
    pub auth: OpenAiAuth,
    /// Reasoning execution mode.
    pub reasoning_mode: ReasoningMode,
    /// Requested reasoning effort.
    pub thinking: Thinking,
    /// Whether an embedding selected an effort instead of model defaults.
    #[doc(hidden)]
    pub thinking_explicit: bool,
    /// Whether requests use priority processing.
    pub fast_mode: bool,
    /// Resolved context window used for accounting and automatic compaction.
    pub context_window_tokens: u64,
    /// Enable model-managed context windows for eligible Codex subscriptions.
    /// Unsupported models, providers, and hosts retain remote summarization.
    pub experimental_context: bool,
    /// Canonical task path for a model-directed child.
    #[doc(hidden)]
    pub agent_name: Option<Arc<str>>,
    /// Preferred initial streaming transport.
    pub responses_transport: ResponsesTransport,
    /// Whether a WebSocket session sends an optional non-generating prewarm
    /// request before its first model call.
    pub websocket_warmup: bool,
    /// Selected healthy-call history strategy.
    pub responses_history: ResponsesHistory,
    /// Whether the provider may retain response checkpoints.
    pub store_responses: bool,
    /// Responses WebSocket endpoint.
    pub websocket_url: String,
    /// Base URL used for HTTPS Responses calls and related endpoints.
    pub api_base_url: String,
    /// Embedding-host transport used by the standard WebAssembly client.
    #[cfg(any(target_family = "wasm", docsrs))]
    pub host_transport: Option<Arc<dyn crate::transport::host::HostTransport>>,
    /// Explicit replacement for the selected model's built-in instructions.
    pub system_prompt: Option<Arc<str>>,
    /// Host instructions appended to the selected or overridden system prompt.
    pub additional_instructions: Option<Arc<str>>,
}

impl ModelConfig {
    pub(crate) fn wire_model_id(&self, model: Model) -> Cow<'static, str> {
        match self.model_id_prefix.as_deref() {
            Some(prefix) => Cow::Owned(format!("{prefix}/{}", model.as_str())),
            None => Cow::Borrowed(model.as_str()),
        }
    }

    /// Returns the fixed orchestration mode sent to the supported model.
    #[must_use]
    pub const fn orchestration() -> &'static str {
        "local_code_mode"
    }

    /// Resolves the selected model's instructions while preserving caller overrides.
    #[must_use]
    pub fn system_prompt(&self) -> Cow<'_, str> {
        let base = self.system_prompt.as_deref().unwrap_or(match self.model {
            Model::Astra => ASTRA_SYSTEM_PROMPT,
            Model::Sol | Model::Terra | Model::Luna => SYSTEM_PROMPT,
        });
        match self.additional_instructions.as_deref() {
            Some(additional) if !additional.is_empty() => {
                Cow::Owned(format!("{base}\n\n{additional}"))
            }
            _ => Cow::Borrowed(base),
        }
    }

    /// Returns the `OpenAI` tool-search endpoint derived from the base URL.
    #[must_use]
    pub fn search_endpoint(&self) -> String {
        format!("{}/alpha/search", self.api_base_url.trim_end_matches('/'))
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            model: Model::default(),
            model_id_prefix: None,
            auth: OpenAiAuth::api_key(String::new()),
            reasoning_mode: ReasoningMode::default(),
            thinking: Thinking::default(),
            thinking_explicit: false,
            fast_mode: false,
            context_window_tokens: CONTEXT_WINDOW_TOKENS,
            experimental_context: true,
            agent_name: None,
            responses_transport: ResponsesTransport::default(),
            websocket_warmup: true,
            responses_history: ResponsesHistory::default(),
            store_responses: false,
            websocket_url: "wss://api.openai.com/v1/responses".to_owned(),
            api_base_url: "https://api.openai.com/v1".to_owned(),
            #[cfg(any(target_family = "wasm", docsrs))]
            host_transport: None,
            system_prompt: None,
            additional_instructions: None,
        }
    }
}
