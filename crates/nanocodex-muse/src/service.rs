//! Muse-owned HTTP orchestration using Nanocodex's request and SSE machinery.
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use nanocodex_oai_api::{
    __private::ResponsesHttp,
    OpenAiError,
    tower::{
        ResponsesAttempt, ResponsesAttemptKind, ResponsesOutput, ResponsesRetryPolicy,
        ResponsesServiceConfig, ResponsesServiceError, ResponsesServiceFactory,
        ResponsesServiceResponse,
    },
    transport::{ResponsesError, ResponsesTransport},
};
use tower::{Service, retry::Retry};
use web_time::Instant;

/// HTTP-only service factory installed by the Muse recipe.
#[doc(hidden)]
#[derive(Clone)]
pub struct MuseServiceFactory;

impl ResponsesServiceFactory for MuseServiceFactory {
    type Service = Retry<ResponsesRetryPolicy, MuseService>;

    fn validate_config(&self, config: &ResponsesServiceConfig) -> Result<(), OpenAiError> {
        if config.responses_transport != ResponsesTransport::Https {
            return Err(OpenAiError::InvalidConfiguration {
                detail: "Muse requires HTTP Responses",
            });
        }
        #[cfg(target_family = "wasm")]
        if config.host_transport.is_none() {
            return Err(OpenAiError::InvalidConfiguration {
                detail: "Muse on WebAssembly requires an embedding-owned HTTP host transport",
            });
        }
        Ok(())
    }

    fn make(&self, config: Arc<ResponsesServiceConfig>) -> Self::Service {
        #[cfg(not(target_family = "wasm"))]
        let http = {
            nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
            ResponsesHttp::new(reqwest::Client::new())
        };
        #[cfg(target_family = "wasm")]
        // Protocol headers are part of the embedding-owned HTTP host contract.
        let http = ResponsesHttp::new(config.host_transport.clone());
        Retry::new(
            ResponsesRetryPolicy::for_config(ResponsesRetryPolicy::DEFAULT_MAX_ATTEMPTS, &config),
            MuseService {
                config,
                http: http.with_headers(false, &[crate::API_VERSION_HEADER]),
                state: Arc::new(tokio::sync::Mutex::new(HttpState::default())),
            },
        )
    }
}

/// One session's Muse HTTP service, including its retained turn-state header.
#[doc(hidden)]
#[derive(Clone)]
pub struct MuseService {
    config: Arc<ResponsesServiceConfig>,
    http: ResponsesHttp,
    state: Arc<tokio::sync::Mutex<HttpState>>,
}

// Lifted from the reference service's ConnectionState; HTTP needs no socket state.
#[derive(Default)]
struct HttpState {
    logical_turn: Option<String>,
    turn_state: Option<String>,
}

impl HttpState {
    fn enter_logical_turn(&mut self, logical_turn: String) {
        if self.logical_turn.as_ref() == Some(&logical_turn) {
            return;
        }
        self.logical_turn = Some(logical_turn);
        self.turn_state = None;
    }

    fn observe_turn_state(&mut self, turn_state: Option<&str>) {
        if self.turn_state.is_none() {
            self.turn_state = turn_state.map(str::to_owned);
        }
    }
}

#[cfg(not(target_family = "wasm"))]
type ServiceFuture =
    Pin<Box<dyn Future<Output = Result<ResponsesServiceResponse, ResponsesServiceError>> + Send>>;
#[cfg(target_family = "wasm")]
type ServiceFuture =
    Pin<Box<dyn Future<Output = Result<ResponsesServiceResponse, ResponsesServiceError>>>>;

impl Service<ResponsesAttempt> for MuseService {
    type Response = ResponsesServiceResponse;
    type Error = ResponsesServiceError;
    type Future = ServiceFuture;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        let request = request.with_raw_api_events(self.config.raw_api_events);
        let mut service = self.clone();
        Box::pin(async move {
            service
                .run(&request)
                .await
                .map_err(|error| error.with_request_input(&request))
        })
    }
}

impl MuseService {
    async fn run(
        &mut self,
        request: &ResponsesAttempt,
    ) -> Result<ResponsesServiceResponse, ResponsesServiceError> {
        if matches!(request.kind(), ResponsesAttemptKind::Warmup) {
            return Err(ResponsesServiceError::protocol(
                "Muse HTTP does not perform a warmup request",
            ));
        }
        let mut state = self.state.lock().await;
        state.enter_logical_turn(request.profile().turn_id());
        let started_at = Instant::now();
        let encoded = crate::responses::encode(
            request.encode_generation(&self.config, state.turn_state.as_deref())?,
            matches!(request.kind(), ResponsesAttemptKind::Compaction),
        )?;
        request.record_http_request(&encoded)?;
        let profile = request.profile();
        let mut auth = self.config.auth.snapshot().await.map_err(auth_error)?;
        let mut recovered = false;
        let (mut response, metadata) = loop {
            let send = self
                .http
                .send(
                    &self.config.api_base_url,
                    &auth,
                    profile.session_id(),
                    profile.thread_id(),
                    state.turn_state.as_deref(),
                    &encoded,
                )
                .await;
            match send {
                Err(ResponsesError::HttpRejected { status: 401, .. }) if !recovered => {
                    self.config
                        .auth
                        .recover_unauthorized(&auth)
                        .await
                        .map_err(auth_error)?;
                    auth = self.config.auth.snapshot().await.map_err(auth_error)?;
                    recovered = true;
                }
                result => break result?,
            }
        };
        state.observe_turn_state(metadata.turn_state.as_deref());
        let generated = response.receive_generation(request, started_at).await?;
        let output = crate::responses::decode(ResponsesOutput::Generation(generated), request)?;
        Ok(ResponsesServiceResponse::new(output)
            .with_attempt(request.attempt())
            .with_server_reasoning_included(metadata.reasoning_included))
    }
}

fn auth_error(error: nanocodex_oai_api::auth::OpenAiAuthError) -> ResponsesError {
    ResponsesError::Authorization {
        detail: error.to_string(),
    }
}
