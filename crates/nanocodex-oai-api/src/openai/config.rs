use std::sync::Arc;

use crate::{OpenAiAuth, ReasoningMode, ResponsesHistory, ResponsesTransport, Thinking};

const SYSTEM_PROMPT: &str = include_str!("../../prompts/system.md");

/// Validated, read-only settings passed to a [`ResponsesServiceFactory`].
///
/// Public policy is configured through [`OpenAiBuilder`]. A custom factory can
/// inspect this snapshot while constructing an independent session service.
///
/// [`OpenAiBuilder`]: super::OpenAiBuilder
/// [`ResponsesServiceFactory`]: super::ResponsesServiceFactory
#[derive(Clone)]
pub struct ModelConfig {
    /// Authentication source resolved for each transport connection.
    pub auth: OpenAiAuth,
    /// Reasoning execution mode.
    pub reasoning_mode: ReasoningMode,
    /// Requested reasoning effort.
    pub thinking: Thinking,
    /// Whether requests use priority processing.
    pub fast_mode: bool,
    /// Preferred initial streaming transport.
    pub responses_transport: ResponsesTransport,
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
    /// Immutable harness system prompt serialized before session instructions.
    pub system_prompt: Arc<str>,
    /// Whether the built-in OpenAI price table applies to reported usage.
    pub estimate_cost: bool,
}

impl ModelConfig {
    /// Returns the fixed orchestration mode sent to the supported model.
    #[must_use]
    pub const fn orchestration() -> &'static str {
        "local_code_mode"
    }

    /// Returns the immutable harness system prompt.
    #[must_use]
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
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
            auth: OpenAiAuth::api_key(String::new()),
            reasoning_mode: ReasoningMode::default(),
            thinking: Thinking::default(),
            fast_mode: false,
            responses_transport: ResponsesTransport::default(),
            responses_history: ResponsesHistory::default(),
            store_responses: false,
            websocket_url: "wss://api.openai.com/v1/responses".to_owned(),
            api_base_url: "https://api.openai.com/v1".to_owned(),
            #[cfg(any(target_family = "wasm", docsrs))]
            host_transport: None,
            system_prompt: SYSTEM_PROMPT.into(),
            estimate_cost: true,
        }
    }
}
