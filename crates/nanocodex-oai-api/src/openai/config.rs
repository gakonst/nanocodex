use std::{borrow::Cow, sync::Arc};

use crate::{
    CONTEXT_WINDOW_TOKENS, Model, OpenAiAuth, ReasoningMode, ResponsesHistory, ResponsesTransport,
    Thinking,
};

const SYSTEM_PROMPT: &str = include_str!("../../prompts/system.md");
const GLM_SYSTEM_PROMPT: &str = include_str!("../../prompts/glm.md");
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
    /// Preferred initial streaming transport.
    pub responses_transport: ResponsesTransport,
    /// Whether a WebSocket session sends an optional non-generating prewarm
    /// request before its first model call.
    pub websocket_warmup: bool,
    /// Whether the standard transport emits complete raw API request/response events.
    /// Enabled by default; disabling avoids serializing their telemetry payloads.
    pub raw_api_events: bool,
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
            Model::Glm53 | Model::Kimi | Model::Mimo => GLM_SYSTEM_PROMPT,
            Model::Sol | Model::Terra | Model::Luna => SYSTEM_PROMPT,
        });
        let base =
            if self.system_prompt.is_none() && matches!(self.model, Model::Kimi | Model::Mimo) {
                Cow::Owned(base.replacen(
                    "powered by Z.ai GLM-5.3",
                    &format!("powered by {}", self.model.as_str()),
                    1,
                ))
            } else {
                Cow::Borrowed(base)
            };
        match self.additional_instructions.as_deref() {
            Some(additional) if !additional.is_empty() => {
                Cow::Owned(format!("{base}\n\n{additional}"))
            }
            _ => base,
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
            responses_transport: ResponsesTransport::default(),
            websocket_warmup: true,
            raw_api_events: true,
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

#[cfg(test)]
mod prompt_tests {
    use super::*;

    #[test]
    fn glm53_prompt_preserves_its_identity() {
        let config = ModelConfig {
            model: Model::Glm53,
            ..ModelConfig::default()
        };
        assert!(config.system_prompt().starts_with("You are Nanocodex"));
        assert!(!config.system_prompt().contains("GPT-"));
        assert!(!config.system_prompt().contains("You are Codex"));
    }

    #[test]
    fn supported_models_select_exact_pinned_instructions() {
        for (model, expected) in [
            (Model::Astra, ASTRA_SYSTEM_PROMPT),
            (Model::Sol, SYSTEM_PROMPT),
            (Model::Terra, SYSTEM_PROMPT),
            (Model::Luna, SYSTEM_PROMPT),
        ] {
            let mut config = ModelConfig {
                model,
                ..ModelConfig::default()
            };
            assert_eq!(config.system_prompt(), expected);
            assert!(config.system_prompt().starts_with("You are Codex,"));
            config.additional_instructions = Some(Arc::from("Host instructions"));
            assert_eq!(
                config.system_prompt(),
                format!("{expected}\n\nHost instructions")
            );
            config.system_prompt = Some(Arc::from("Explicit override"));
            assert_eq!(
                config.system_prompt(),
                "Explicit override\n\nHost instructions"
            );
        }
        assert!(ASTRA_SYSTEM_PROMPT.starts_with("You are Codex, an agent based on GPT-6."));
        assert!(!ASTRA_SYSTEM_PROMPT.contains("As Nanocodex,"));
    }
}
