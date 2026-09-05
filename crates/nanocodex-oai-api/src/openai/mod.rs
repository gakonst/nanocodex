use std::{num::NonZeroU32, sync::Arc};

use ::tower::Layer;

mod config;
mod platform;

use crate::{
    DefaultResponsesService, Model, OpenAiAuth, OpenAiAuthError, OpenAiAuthMode, ReasoningMode,
    ResponsesHistory, ResponsesRetryPolicy, ResponsesTransport, Thinking, session::SessionBuilder,
};

#[doc(hidden)]
pub use config::ModelConfig;
pub use config::ModelConfig as ResponsesServiceConfig;

/// Configured, cloneable `OpenAI` client recipe.
///
/// `OpenAi` owns authentication, endpoint policy, and the concrete Tower
/// service factory. Each session built from it receives independent mutable
/// service and conversation state.
#[derive(Clone)]
pub struct OpenAi<F = StandardServiceFactory> {
    config: ModelConfig,
    factory: F,
}

impl OpenAi<StandardServiceFactory> {
    /// Creates a client with the standard persistent WebSocket and retry
    /// stack, including session-scoped HTTPS fallback on native targets.
    ///
    /// # Errors
    ///
    /// Returns an error when the supplied credentials are unavailable.
    pub fn new(auth: impl Into<OpenAiAuth>) -> Result<Self, OpenAiError> {
        Self::builder(auth).build()
    }

    /// Starts configuring an `OpenAI` client.
    #[must_use]
    pub fn builder(auth: impl Into<OpenAiAuth>) -> OpenAiBuilder<StandardServiceFactory> {
        let auth = auth.into();
        let mode = auth.mode();
        let mut config = ModelConfig {
            auth,
            ..ModelConfig::default()
        };
        apply_mode_defaults(&mut config, mode);
        OpenAiBuilder {
            config,
            factory: StandardServiceFactory::default(),
        }
    }
}

impl<F> OpenAi<F>
where
    F: ResponsesServiceFactory,
{
    /// Returns the authentication mode used by this client recipe.
    #[must_use]
    pub const fn auth_mode(&self) -> OpenAiAuthMode {
        self.config.auth.mode()
    }

    /// Starts configuring a native GPT Realtime voice session.
    ///
    /// Realtime uses this client's authentication and API base while owning an
    /// independent conversation lifecycle. Platform API keys connect directly
    /// over WebSocket. Managed ChatGPT credentials create a WebRTC media call
    /// and attach its sideband control socket. Audio remains exposed as 24 kHz
    /// mono PCM16 so embeddings retain device policy.
    #[cfg(all(feature = "realtime", not(target_family = "wasm")))]
    #[cfg_attr(
        docsrs,
        doc(cfg(all(feature = "realtime", not(target_family = "wasm"))))
    )]
    #[must_use]
    pub fn realtime(
        &self,
        instructions: impl Into<Arc<str>>,
    ) -> crate::realtime::RealtimeSessionBuilder {
        crate::realtime::RealtimeSessionBuilder::new(
            self.config.auth.clone(),
            self.config.api_base_url.clone(),
            instructions.into(),
        )
    }

    /// Attaches an authenticated control sideband to an existing realtime call.
    ///
    /// The embedding retains ownership of call creation, WebRTC negotiation,
    /// and media. Nanocodex does not reconfigure or close the remote call.
    #[cfg(all(feature = "realtime", not(target_family = "wasm")))]
    #[cfg_attr(
        docsrs,
        doc(cfg(all(feature = "realtime", not(target_family = "wasm"))))
    )]
    #[must_use]
    pub fn attach_realtime_call(
        &self,
        call_id: impl Into<String>,
    ) -> crate::realtime::RealtimeCallAttachmentBuilder {
        crate::realtime::RealtimeCallAttachmentBuilder::new(
            self.config.auth.clone(),
            call_id.into(),
        )
    }

    /// Starts a client-side managed session with stable developer
    /// instructions.
    ///
    /// The returned builder does not make a network request. Its `build`
    /// method creates fresh transport and context state.
    #[must_use]
    pub fn instructions(&self, instructions: impl Into<Arc<str>>) -> SessionBuilder<F> {
        SessionBuilder::new(self.clone(), instructions.into())
    }

    pub(crate) const fn config(&self) -> &ModelConfig {
        &self.config
    }

    pub(crate) fn make_service(&self) -> F::Service {
        self.factory.make(Arc::new(self.config.clone()))
    }

    pub(crate) fn into_parts(self) -> (ModelConfig, F) {
        (self.config, self.factory)
    }
}

/// Builder for a configured `OpenAI` client and concrete Tower service factory.
#[derive(Clone)]
pub struct OpenAiBuilder<F = StandardServiceFactory> {
    config: ModelConfig,
    factory: F,
}

impl<F> OpenAiBuilder<F> {
    /// Selects the default GPT-5.6 coding model for new sessions and agents.
    ///
    /// A higher-level session or agent builder may override this reusable
    /// client default without mutating the `OpenAi` recipe.
    #[must_use]
    pub const fn model(mut self, model: Model) -> Self {
        self.config.model = model;
        if self.config.context_window_tokens > model.max_context_window_tokens() {
            self.config.context_window_tokens = model.max_context_window_tokens();
        }
        self
    }

    /// Prepends a namespace to supported model identifiers on the wire.
    ///
    /// For example, an OpenAI routing gateway may expose Sol as
    /// `openai/gpt-5.6-sol` while Nanocodex continues to retain `Model::Sol`
    /// for model-specific behavior, pricing, compaction, and snapshots. This
    /// changes only the wire identifier for the closed [`Model`] enum; it is
    /// not an alternate provider or arbitrary-model surface. [`Self::build`]
    /// rejects malformed prefixes and use outside API-key HTTPS transport.
    #[must_use]
    pub fn model_id_prefix(mut self, prefix: impl Into<String>) -> Self {
        let prefix = prefix.into();
        self.config.model_id_prefix = Some(Arc::from(prefix.trim()));
        self
    }

    /// Selects the initial Responses transport policy for new sessions.
    ///
    /// [`ResponsesTransport::WebSocket`] prefers a persistent socket. The
    /// native standard stack degrades one-way to HTTPS after exhausting its
    /// retry budget.
    /// [`ResponsesTransport::Https`] never probes the WebSocket endpoint.
    #[must_use]
    pub const fn transport(mut self, transport: ResponsesTransport) -> Self {
        self.config.responses_transport = transport;
        if matches!(transport, ResponsesTransport::Https) && !self.config.store_responses {
            self.config.responses_history = ResponsesHistory::FullReplay;
        }
        self
    }

    /// Controls the optional non-generating request used to prewarm a
    /// persistent Responses WebSocket before its first model call.
    ///
    /// This is enabled by default to match Codex: session startup primes the
    /// immutable tools and instructions, and the first model call reuses the
    /// resulting response chain on the same connection.
    #[must_use]
    pub const fn websocket_warmup(mut self, enabled: bool) -> Self {
        self.config.websocket_warmup = enabled;
        self
    }

    /// Selects incremental continuation or complete replay for healthy calls.
    #[must_use]
    pub const fn history(mut self, history: ResponsesHistory) -> Self {
        self.config.responses_history = history;
        self
    }

    /// Controls whether the provider retains Responses checkpoints.
    ///
    /// Storage is disabled by default for both API-key and ChatGPT
    /// authentication. API-key callers can opt into durable checkpoints with
    /// `store(true)`; ChatGPT subscription authentication does not support
    /// stored responses.
    ///
    /// On HTTPS, this also selects the compatible default history policy:
    /// incremental checkpoints when enabled and full client-history replay
    /// when disabled. Call [`Self::history`] afterwards to override that
    /// default.
    #[must_use]
    pub const fn store(mut self, store: bool) -> Self {
        self.config.store_responses = store;
        if matches!(self.config.responses_transport, ResponsesTransport::Https) {
            self.config.responses_history = if store {
                ResponsesHistory::Incremental
            } else {
                ResponsesHistory::FullReplay
            };
        }
        self
    }

    /// Sets the default reasoning effort for new sessions and agents.
    ///
    /// A higher-level session or agent builder may override this reusable
    /// client default without mutating the `OpenAi` recipe.
    #[must_use]
    pub const fn thinking(mut self, thinking: Thinking) -> Self {
        self.config.thinking = thinking;
        self
    }

    /// Sets the default reasoning execution mode for new sessions and agents.
    ///
    /// A higher-level agent builder may override this reusable client default.
    #[must_use]
    pub const fn reasoning_mode(mut self, reasoning_mode: ReasoningMode) -> Self {
        self.config.reasoning_mode = reasoning_mode;
        self
    }

    /// Selects the default priority-processing policy for new sessions and
    /// agents.
    ///
    /// A higher-level session or agent builder may override this reusable
    /// client default without mutating the `OpenAi` recipe.
    #[must_use]
    pub const fn fast_mode(mut self, enabled: bool) -> Self {
        self.config.fast_mode = enabled;
        self
    }

    /// Sets the selected model's context window used for accounting and compaction.
    ///
    /// Nanocodex defaults to 272,000 tokens to stay below long-context pricing.
    /// Values above the selected model's advertised maximum are clamped.
    #[must_use]
    pub const fn context_window_tokens(mut self, tokens: u64) -> Self {
        let maximum = self.config.model.max_context_window_tokens();
        self.config.context_window_tokens = if tokens > maximum { maximum } else { tokens };
        self
    }

    /// Replaces the standard Responses WebSocket endpoint.
    #[must_use]
    pub fn websocket_url(mut self, url: impl Into<String>) -> Self {
        self.config.websocket_url = url.into();
        self
    }

    /// Replaces the standard `OpenAI` API base URL.
    #[must_use]
    pub fn api_base_url(mut self, url: impl Into<String>) -> Self {
        self.config.api_base_url = url.into();
        self
    }

    /// Installs the environment-owned socket and timer implementation.
    ///
    /// The host only moves text frames and waits for retry deadlines. Request
    /// encoding, response decoding, continuation state, and retry decisions
    /// remain owned by this crate.
    #[cfg(any(target_family = "wasm", docsrs))]
    #[cfg_attr(docsrs, doc(cfg(target_family = "wasm")))]
    #[must_use]
    pub fn host_transport(mut self, transport: impl crate::transport::host::HostTransport) -> Self {
        self.config.host_transport = Some(Arc::new(transport));
        self
    }

    /// Applies a Tower layer without boxing the resulting service.
    ///
    /// Common Tower middleware that returns [`tower::BoxError`] is converted
    /// into [`crate::ResponseError`] without discarding its source.
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use nanocodex_oai_api::OpenAi;
    /// use tower::timeout::TimeoutLayer;
    ///
    /// let openai = OpenAi::builder("test-api-key")
    ///     .layer(TimeoutLayer::new(Duration::from_secs(45)))
    ///     .build()?;
    ///
    /// let session = openai
    ///     .instructions("Preserve exact identifiers and answer concisely.")
    ///     .build()?;
    /// assert_eq!(session.history_len(), 0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn layer<L>(self, layer: L) -> OpenAiBuilder<LayeredServiceFactory<F, L>> {
        OpenAiBuilder {
            config: self.config,
            factory: LayeredServiceFactory {
                inner: self.factory,
                layer,
            },
        }
    }

    /// Replaces the standard stack with a factory for independent services.
    ///
    /// The factory runs once per managed session. Its service receives a
    /// replayable [`crate::tower::ResponsesAttempt`], may emit normalized
    /// streaming events through [`crate::tower::ResponsesAttempt::emit`], and
    /// returns one complete [`crate::tower::ResponsesServiceResponse`].
    ///
    /// ```no_run
    /// use nanocodex_oai_api::{
    ///     OpenAi, ResponseError, ResponseEvent,
    ///     responses::{ContentItem, MessageRole, ResponseItem},
    ///     tower::{
    ///         GenerationOutput, ResponsePipelineStats, ResponsesAttempt,
    ///         ResponsesOutput, ResponsesServiceResponse,
    ///     },
    /// };
    /// use tower::service_fn;
    ///
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let openai = OpenAi::builder("test-api-key")
    ///     .service(|| {
    ///         service_fn(|request: ResponsesAttempt| async move {
    ///             request
    ///                 .emit(ResponseEvent::OutputTextDelta(
    ///                     "served by the adapter".to_owned(),
    ///                 ))
    ///                 .await;
    ///             let item = ResponseItem::message(
    ///                 MessageRole::Assistant,
    ///                 [ContentItem::output_text("served by the adapter")],
    ///             );
    ///             Ok::<_, ResponseError>(ResponsesServiceResponse::new(
    ///                 ResponsesOutput::Generation(GenerationOutput {
    ///                     id: "resp_adapter_01".to_owned(),
    ///                     status: "completed".to_owned(),
    ///                     end_turn: Some(true),
    ///                     final_message: Some("served by the adapter".to_owned()),
    ///                     output_items: vec![item],
    ///                     code_calls: Vec::new(),
    ///                     usage: None,
    ///                     time_to_first_event_ns: 0,
    ///                     time_to_first_output_ns: Some(0),
    ///                     pipeline_stats: ResponsePipelineStats::default(),
    ///                 }),
    ///             ))
    ///         })
    ///     })
    ///     .build()?;
    /// let mut session = openai
    ///     .instructions("Return the adapter's exact retained result.")
    ///     .build()?;
    ///
    /// let completed = session
    ///     .turn()
    ///     .create("Return the retained result.")
    ///     .await?;
    /// assert_eq!(completed.output_text(), "served by the adapter");
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn service<M>(self, make: M) -> OpenAiBuilder<CallerServiceFactory<M>> {
        OpenAiBuilder {
            config: self.config,
            factory: CallerServiceFactory {
                make: Arc::new(make),
            },
        }
    }
}

impl OpenAiBuilder<StandardServiceFactory> {
    /// Sets the total attempt limit for the standard typed retry policy.
    #[must_use]
    pub const fn max_attempts(mut self, max_attempts: NonZeroU32) -> Self {
        self.factory.max_attempts = max_attempts;
        self
    }

    /// Replaces the HTTP client used by the standard HTTPS/SSE transport.
    #[cfg(not(target_family = "wasm"))]
    #[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
    #[must_use]
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.factory.platform.set_http_client(client);
        self
    }
}

impl<F> OpenAiBuilder<F>
where
    F: ResponsesServiceFactory,
{
    /// Validates the configuration and returns a cloneable client recipe.
    ///
    /// # Errors
    ///
    /// Returns an error for unavailable credentials or an incompatible
    /// transport, storage, and replay configuration.
    pub fn build(self) -> Result<OpenAi<F>, OpenAiError> {
        #[cfg(not(target_family = "wasm"))]
        crate::transport::install_default_rustls_crypto_provider();
        validate(&self.config)?;
        self.factory.validate_config(&self.config)?;
        Ok(OpenAi {
            config: self.config,
            factory: self.factory,
        })
    }
}

/// Standard service factory for the persistent transport and typed retry
/// stack.
#[derive(Clone)]
pub struct StandardServiceFactory {
    max_attempts: NonZeroU32,
    platform: platform::FactoryPlatform,
}

impl Default for StandardServiceFactory {
    fn default() -> Self {
        #[cfg(not(target_family = "wasm"))]
        crate::transport::install_default_rustls_crypto_provider();
        Self {
            max_attempts: ResponsesRetryPolicy::DEFAULT_MAX_ATTEMPTS,
            platform: platform::FactoryPlatform::new(),
        }
    }
}

/// Factory produced by [`OpenAiBuilder::service`].
///
/// The callable is invoked once for each managed session, so every session
/// owns independent mutable Tower service state.
pub struct CallerServiceFactory<M> {
    make: Arc<M>,
}

impl<M> Clone for CallerServiceFactory<M> {
    fn clone(&self) -> Self {
        Self {
            make: Arc::clone(&self.make),
        }
    }
}

/// A concrete Tower layer applied to another service factory.
#[derive(Clone)]
pub struct LayeredServiceFactory<F, L> {
    inner: F,
    layer: L,
}

/// Factory for the concrete Tower service owned by each managed session.
///
/// This trait makes the generic result of [`OpenAiBuilder::layer`] and
/// [`OpenAiBuilder::service`] usable in named structs and function bounds.
/// Most callers should obtain one of the provided implementations from those
/// builder methods instead of implementing the construction boundary directly.
pub trait ResponsesServiceFactory: Clone {
    /// Concrete service owned by each managed session.
    type Service;

    /// Validates service-specific client configuration.
    fn validate_config(&self, _config: &ResponsesServiceConfig) -> Result<(), OpenAiError> {
        Ok(())
    }

    /// Creates one independent service stack.
    fn make(&self, config: Arc<ResponsesServiceConfig>) -> Self::Service;
}

impl ResponsesServiceFactory for StandardServiceFactory {
    type Service = DefaultResponsesService;

    fn validate_config(&self, config: &ModelConfig) -> Result<(), OpenAiError> {
        self.platform.validate_config(config)
    }

    fn make(&self, config: Arc<ModelConfig>) -> Self::Service {
        self.platform.make(config, self.max_attempts)
    }
}

impl<M, S> ResponsesServiceFactory for CallerServiceFactory<M>
where
    M: Fn() -> S,
{
    type Service = S;

    fn make(&self, _config: Arc<ModelConfig>) -> Self::Service {
        (self.make)()
    }
}

impl<F, L> ResponsesServiceFactory for LayeredServiceFactory<F, L>
where
    F: ResponsesServiceFactory,
    L: Layer<F::Service> + Clone,
{
    type Service = L::Service;

    fn validate_config(&self, config: &ModelConfig) -> Result<(), OpenAiError> {
        self.inner.validate_config(config)
    }

    fn make(&self, config: Arc<ModelConfig>) -> Self::Service {
        self.layer.layer(self.inner.make(config))
    }
}

/// Invalid `OpenAI` client configuration.
#[derive(Debug, thiserror::Error)]
pub enum OpenAiError {
    /// Credentials cannot currently provide an authorization value.
    #[error(transparent)]
    Authorization(#[from] OpenAiAuthError),
    /// Two client policies cannot be satisfied together.
    #[error("invalid OpenAI client configuration: {detail}")]
    InvalidConfiguration {
        /// Human-readable explanation without credentials.
        detail: &'static str,
    },
}

fn apply_mode_defaults(config: &mut ModelConfig, mode: OpenAiAuthMode) {
    config.store_responses = false;
    config.responses_history = ResponsesHistory::Incremental;
    mode.default_websocket_url()
        .clone_into(&mut config.websocket_url);
    mode.default_api_base_url()
        .clone_into(&mut config.api_base_url);
}

fn validate(config: &ModelConfig) -> Result<(), OpenAiError> {
    config.auth.validate()?;
    if !config.model.supports_thinking(config.thinking) {
        return Err(OpenAiError::InvalidConfiguration {
            detail: "GPT-6 Astra requires low, medium, high, xhigh, or max reasoning effort",
        });
    }
    if !config.model.supports_reasoning_mode(config.reasoning_mode) {
        return Err(OpenAiError::InvalidConfiguration {
            detail: "GPT-6 Astra does not support pro reasoning mode",
        });
    }
    if config.context_window_tokens == 0 {
        return Err(OpenAiError::InvalidConfiguration {
            detail: "the model context window must be greater than zero",
        });
    }
    if config.websocket_url.trim().is_empty() {
        return Err(OpenAiError::InvalidConfiguration {
            detail: "the Responses WebSocket URL must not be empty",
        });
    }
    if config.api_base_url.trim().is_empty() {
        return Err(OpenAiError::InvalidConfiguration {
            detail: "the OpenAI API base URL must not be empty",
        });
    }
    if let Some(prefix) = config.model_id_prefix.as_deref() {
        if prefix.is_empty()
            || prefix.starts_with('/')
            || prefix.ends_with('/')
            || prefix.split('/').any(str::is_empty)
            || prefix
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(OpenAiError::InvalidConfiguration {
                detail: "the model ID prefix must contain non-empty path segments without whitespace",
            });
        }
        if config.auth.mode() != OpenAiAuthMode::ApiKey {
            return Err(OpenAiError::InvalidConfiguration {
                detail: "model ID prefixes require API-key authentication",
            });
        }
        if !matches!(config.responses_transport, ResponsesTransport::Https) {
            return Err(OpenAiError::InvalidConfiguration {
                detail: "model ID prefixes require the HTTPS Responses transport",
            });
        }
    }
    if config.auth.mode() == OpenAiAuthMode::ChatGpt && config.store_responses {
        return Err(OpenAiError::InvalidConfiguration {
            detail: "ChatGPT subscription authentication does not support store: true",
        });
    }
    if matches!(config.responses_transport, ResponsesTransport::Https)
        && !config.store_responses
        && matches!(config.responses_history, ResponsesHistory::Incremental)
    {
        return Err(OpenAiError::InvalidConfiguration {
            detail: "HTTPS with store: false requires full client-history replay",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        error::Error as _,
        future::{Ready, pending},
        time::Duration,
    };

    use ::tower::{Service, service_fn, timeout::TimeoutLayer};

    use crate::{
        Model, ModelConfig, OpenAiAuthMode, ResponseError, ResponsesAttempt, ResponsesHistory,
        ResponsesServiceResponse, ResponsesTransport,
    };

    use super::{OpenAi, apply_mode_defaults};

    #[derive(Clone)]
    struct NeverCalled;

    impl Service<ResponsesAttempt> for NeverCalled {
        type Response = ResponsesServiceResponse;
        type Error = Infallible;
        type Future = Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, _request: ResponsesAttempt) -> Self::Future {
            panic!("test service should not be called")
        }
    }

    #[test]
    fn one_client_recipe_builds_independent_sessions() {
        let client = OpenAi::builder("test-key")
            .service(|| NeverCalled)
            .build()
            .unwrap();

        let session = client.instructions("Answer only from supplied facts.");
        let first = session.clone().build().unwrap();
        let second = session.build().unwrap();

        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn response_storage_is_opt_in_for_both_auth_modes() {
        for mode in [OpenAiAuthMode::ApiKey, OpenAiAuthMode::ChatGpt] {
            let mut config = ModelConfig {
                store_responses: true,
                ..ModelConfig::default()
            };
            apply_mode_defaults(&mut config, mode);
            assert!(!config.store_responses);
        }
    }

    #[test]
    fn context_window_defaults_and_clamps_to_supported_model_limit() {
        let default = OpenAi::builder("test-key")
            .service(|| NeverCalled)
            .build()
            .unwrap();
        assert_eq!(
            default.config.context_window_tokens,
            crate::CONTEXT_WINDOW_TOKENS
        );

        let maximum = OpenAi::builder("test-key")
            .context_window_tokens(u64::MAX)
            .service(|| NeverCalled)
            .build()
            .unwrap();
        assert_eq!(
            maximum.config.context_window_tokens,
            Model::default().max_context_window_tokens()
        );

        let astra_maximum = OpenAi::builder("test-key")
            .model(Model::Astra)
            .context_window_tokens(u64::MAX)
            .service(|| NeverCalled)
            .build()
            .unwrap();
        assert_eq!(
            astra_maximum.config.context_window_tokens,
            crate::MAX_CONTEXT_WINDOW_TOKENS
        );

        let error = OpenAi::builder("test-key")
            .context_window_tokens(0)
            .service(|| NeverCalled)
            .build()
            .err()
            .expect("zero context window should fail validation");
        assert!(error.to_string().contains("greater than zero"));
    }

    #[test]
    fn astra_rejects_unsupported_none_reasoning() {
        let error = OpenAi::builder("test-key")
            .model(crate::Model::Astra)
            .thinking(crate::Thinking::None)
            .service(|| NeverCalled)
            .build()
            .err()
            .expect("Astra none reasoning should fail validation");

        assert!(error.to_string().contains("GPT-6 Astra requires low"));
    }

    #[test]
    fn astra_rejects_unsupported_pro_reasoning_mode() {
        let error = OpenAi::builder("test-key")
            .model(crate::Model::Astra)
            .reasoning_mode(crate::ReasoningMode::Pro)
            .service(|| NeverCalled)
            .build()
            .err()
            .expect("Astra pro reasoning mode should fail validation");

        assert!(error.to_string().contains("does not support pro"));
    }

    #[test]
    fn api_key_can_opt_into_https_checkpoints() {
        let client = OpenAi::builder("test-key")
            .transport(ResponsesTransport::Https)
            .store(true)
            .build()
            .unwrap();

        assert!(client.config.store_responses);
        assert_eq!(
            client.config.responses_history,
            ResponsesHistory::Incremental
        );
    }

    #[test]
    fn model_id_prefix_is_normalized_and_scoped_to_api_key_https() {
        let client = OpenAi::builder("test-key")
            .transport(ResponsesTransport::Https)
            .model_id_prefix(" openai ")
            .build()
            .unwrap();

        assert_eq!(client.config.model_id_prefix.as_deref(), Some("openai"));

        let websocket_error = OpenAi::builder("test-key")
            .model_id_prefix("openai")
            .build()
            .err()
            .expect("WebSocket prefix should be rejected");
        assert!(
            websocket_error
                .to_string()
                .contains("require the HTTPS Responses transport")
        );

        for prefix in [" ", "/openai", "openai/", "openai//routing", "open ai"] {
            let malformed_error = OpenAi::builder("test-key")
                .transport(ResponsesTransport::Https)
                .model_id_prefix(prefix)
                .build()
                .err()
                .expect("malformed prefix should be rejected");
            assert!(
                malformed_error
                    .to_string()
                    .contains("non-empty path segments")
            );
        }
    }

    #[tokio::test]
    async fn tower_box_errors_remain_usable_through_the_managed_response_api() {
        let client = OpenAi::builder("test-key")
            .service(|| {
                service_fn(|_request: ResponsesAttempt| {
                    pending::<Result<ResponsesServiceResponse, ResponseError>>()
                })
            })
            .layer(TimeoutLayer::new(Duration::from_millis(1)))
            .build()
            .unwrap();
        let mut session = client
            .instructions("Return exactly one short answer.")
            .build()
            .unwrap();

        let error = session
            .turn()
            .create("This request should reach the test deadline.")
            .await
            .unwrap_err();

        assert!(error.source().is_some());
        assert!(error.to_string().contains("request timed out"));
    }
}
