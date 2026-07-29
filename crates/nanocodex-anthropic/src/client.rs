use std::{num::NonZeroU32, sync::Arc};

use super::{
    AnthropicAuth,
    service::{AnthropicResponsesService, AnthropicService, retrying},
};
use nanocodex_oai_api::{
    OpenAi, OpenAiError,
    auth::OpenAiAuth,
    tower::{CallerServiceFactory, ResponsesRetryPolicy},
    transport::{ResponsesHistory, ResponsesTransport},
};

/// Default Claude model used by the Messages client.
pub const ANTHROPIC_MODEL: &str = "claude-opus-5";
const ANTHROPIC_API_BASE_URL: &str = "https://api.anthropic.com/v1";

/// Namespace for constructing an Anthropic-backed Nanocodex client recipe.
pub struct Anthropic;

impl Anthropic {
    /// Starts configuring an Anthropic Messages client.
    #[must_use]
    pub fn builder(auth: AnthropicAuth) -> AnthropicBuilder {
        AnthropicBuilder {
            auth,
            model: ANTHROPIC_MODEL.into(),
            api_base_url: ANTHROPIC_API_BASE_URL.to_owned(),
            max_tokens: super::translate::DEFAULT_MAX_TOKENS,
            max_attempts: ResponsesRetryPolicy::DEFAULT_MAX_ATTEMPTS,
            http_client: reqwest::Client::new(),
        }
    }

    /// Creates an Anthropic Messages client with Claude Code defaults.
    ///
    /// # Errors
    ///
    /// Returns an error when the supplied credential is unavailable.
    pub fn client(
        auth: AnthropicAuth,
    ) -> Result<
        OpenAi<CallerServiceFactory<impl Fn() -> AnthropicResponsesService + Clone>>,
        AnthropicError,
    > {
        Self::builder(auth).build()
    }
}

/// Builder for an Anthropic-backed agent client.
pub struct AnthropicBuilder {
    auth: AnthropicAuth,
    model: Arc<str>,
    api_base_url: String,
    max_tokens: u32,
    max_attempts: NonZeroU32,
    http_client: reqwest::Client,
}

impl AnthropicBuilder {
    /// Overrides the Messages model name.
    #[must_use]
    pub fn model(mut self, model: impl Into<Arc<str>>) -> Self {
        self.model = model.into();
        self
    }

    /// Overrides the Anthropic API base URL.
    #[must_use]
    pub fn api_base_url(mut self, api_base_url: impl Into<String>) -> Self {
        self.api_base_url = api_base_url.into();
        self
    }

    /// Overrides the maximum output token count sent to Messages.
    #[must_use]
    pub const fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Sets the total attempt limit, including the initial request.
    #[must_use]
    pub const fn max_attempts(mut self, max_attempts: NonZeroU32) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Replaces the HTTP client used by the Messages transport.
    #[must_use]
    pub fn http_client(mut self, http_client: reqwest::Client) -> Self {
        self.http_client = http_client;
        self
    }

    /// Validates the recipe and creates a cloneable client.
    ///
    /// # Errors
    ///
    /// Returns an error for unavailable credentials or empty endpoint/model policy.
    pub fn build(
        self,
    ) -> Result<
        OpenAi<CallerServiceFactory<impl Fn() -> AnthropicResponsesService + Clone>>,
        AnthropicError,
    > {
        self.auth.validate()?;
        if self.api_base_url.trim().is_empty() {
            return Err(AnthropicError::InvalidConfiguration(
                "the Anthropic API base URL must not be empty",
            ));
        }
        if self.model.trim().is_empty() {
            return Err(AnthropicError::InvalidConfiguration(
                "the Anthropic model must not be empty",
            ));
        }
        if self.max_tokens == 0 {
            return Err(AnthropicError::InvalidConfiguration(
                "Anthropic max_tokens must be greater than zero",
            ));
        }

        let max_attempts = self.max_attempts;
        let service = AnthropicService::new(
            self.auth,
            self.model,
            self.api_base_url,
            self.max_tokens,
            self.http_client,
        );
        OpenAi::builder(OpenAiAuth::api_key("anthropic-oapi-adapter"))
            .transport(ResponsesTransport::Https)
            .history(ResponsesHistory::FullReplay)
            .store(false)
            .estimate_cost(false)
            .service(move || retrying(service.clone(), max_attempts))
            .build()
            .map_err(Into::into)
    }
}

/// Invalid Anthropic client configuration.
#[derive(Debug, thiserror::Error)]
pub enum AnthropicError {
    /// Credentials cannot currently provide authorization.
    #[error(transparent)]
    Authorization(#[from] super::AnthropicAuthError),
    /// The embedded OpenAI Responses client rejected its wrapper configuration.
    #[error(transparent)]
    OpenAi(#[from] OpenAiError),
    /// Two client policies cannot be satisfied together.
    #[error("invalid Anthropic client configuration: {0}")]
    InvalidConfiguration(&'static str),
}
