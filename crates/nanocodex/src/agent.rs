use std::{path::PathBuf, sync::Arc};

use nanocodex_core::{AgentEvents, EventSink, ModelConfig, Prompt, Thinking};
use nanocodex_service::{
    DefaultResponsesService, ResponsesAttempt, ResponsesClient, ResponsesService,
    ResponsesServiceResponse, TransportStats,
};
use nanocodex_tools::Tools;
use tokio::sync::{mpsc, oneshot};
use tower::Service;
use tracing::{Instrument, info_span};

use crate::{
    AgentError, NanocodexError, Result,
    model::agent::ModelRun,
    responses::{LayeredResponses, Responses, StandardResponses},
};

const COMMAND_CAPACITY: usize = 8;

/// Completion handle for an accepted turn.
pub struct Turn {
    result: oneshot::Receiver<Result<TurnResult>>,
}

impl Turn {
    /// Waits for and returns the final typed turn result.
    ///
    /// # Errors
    ///
    /// Returns the model-run failure or an error if the driver stopped early.
    pub async fn result(self) -> Result<TurnResult> {
        self.result
            .await
            .map_err(|_| NanocodexError::Agent(AgentError::TurnStopped))?
    }
}

/// Final result of a completed turn.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TurnResult {
    pub final_message: String,
}

enum Command {
    Prompt {
        prompt: Prompt,
        result: oneshot::Sender<Result<TurnResult>>,
    },
}

/// Cheap, cloneable command handle for an owned agent driver.
#[derive(Clone)]
pub struct Nanocodex {
    commands: mpsc::Sender<Command>,
    workspace: Option<Arc<str>>,
}

impl Nanocodex {
    /// Builds a running agent with the standard prompt, tools, thinking level,
    /// and Responses WebSocket stack, returning its prompt handle and ordered
    /// event stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the API key is empty or no Tokio runtime is active.
    pub fn new(api_key: impl Into<String>) -> Result<(Self, AgentEvents)> {
        Self::builder(api_key).build()
    }

    /// Starts configuring an agent with sensible defaults.
    #[must_use]
    pub fn builder(api_key: impl Into<String>) -> NanocodexBuilder {
        let config = ModelConfig {
            api_key: api_key.into(),
            ..ModelConfig::default()
        };
        NanocodexBuilder {
            config,
            tools: Tools::default(),
            workspace: None,
            session_id: None,
            responses: Responses::default(),
        }
    }

    /// Accepts the agent's prompt and immediately returns its turn handle.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty prompt or if the driver stopped.
    pub async fn prompt(&self, prompt: impl Into<Prompt>) -> Result<Turn> {
        let mut prompt = prompt.into();
        if prompt.instruction.is_empty() {
            return Err(NanocodexError::InvalidRequest(
                "prompt instruction must not be empty".to_owned(),
            ));
        }
        if prompt.workspace.is_none() {
            prompt.workspace = self.workspace.as_deref().map(str::to_owned);
        }
        let (result, receiver) = oneshot::channel();
        if self
            .commands
            .send(Command::Prompt { prompt, result })
            .await
            .is_err()
        {
            return Err(NanocodexError::Agent(AgentError::DriverStopped));
        }
        Ok(Turn { result: receiver })
    }
}

/// Builder for a running agent with deferred Responses service composition.
pub struct NanocodexBuilder<S = StandardResponses> {
    config: ModelConfig,
    tools: Tools,
    workspace: Option<PathBuf>,
    session_id: Option<String>,
    responses: Responses<S>,
}

impl<S> NanocodexBuilder<S> {
    /// Replaces the stable system/developer prompt.
    #[must_use]
    pub fn prompt(mut self, prompt: impl Into<Arc<str>>) -> Self {
        self.config.system_prompt = prompt.into();
        self
    }

    /// Sets the model thinking level. The default is [`Thinking::Medium`].
    #[must_use]
    pub const fn thinking(mut self, thinking: Thinking) -> Self {
        self.config.thinking = thinking;
        self
    }

    /// Replaces the standard built-in tool selection.
    #[must_use]
    pub fn tools(mut self, tools: Tools) -> Self {
        self.tools = tools;
        self
    }

    /// Fixes the workspace used by every prompt in this agent session.
    #[must_use]
    pub fn workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    /// Sets the stable session/request ID used for headers and prompt caching.
    #[must_use]
    pub fn session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Replaces the default Responses configuration or service stack.
    #[must_use]
    pub fn responses<T>(self, responses: Responses<T>) -> NanocodexBuilder<T> {
        NanocodexBuilder {
            config: self.config,
            tools: self.tools,
            workspace: self.workspace,
            session_id: self.session_id,
            responses,
        }
    }
}

impl NanocodexBuilder<StandardResponses> {
    /// Builds and spawns the agent with the standard persistent-WebSocket and
    /// retry stack, returning its prompt handle and ordered event stream.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration or when no Tokio runtime is
    /// active.
    pub fn build(mut self) -> Result<(Nanocodex, AgentEvents)> {
        configure(&mut self.config, &self.responses);
        validate(&self.config, self.session_id.as_deref())?;
        let config = Arc::new(self.config);
        let service = ResponsesService::standard(Arc::clone(&config));
        build_agent(config, self.tools, self.workspace, self.session_id, service)
    }
}

impl<L> NanocodexBuilder<LayeredResponses<L>>
where
    L: tower::Layer<DefaultResponsesService>,
    L::Service: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    <L::Service as Service<ResponsesAttempt>>::Error: Into<NanocodexError> + Send + 'static,
    <L::Service as Service<ResponsesAttempt>>::Future: Send,
{
    /// Builds and spawns the agent after applying the caller's deferred Tower
    /// layers around the standard persistent-WebSocket and retry stack,
    /// returning its prompt handle and ordered event stream.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration or when no Tokio runtime is
    /// active.
    pub fn build(mut self) -> Result<(Nanocodex, AgentEvents)> {
        configure(&mut self.config, &self.responses);
        validate(&self.config, self.session_id.as_deref())?;
        let config = Arc::new(self.config);
        let service = self
            .responses
            .service
            .0
            .service(ResponsesService::standard(Arc::clone(&config)));
        build_agent(config, self.tools, self.workspace, self.session_id, service)
    }
}

impl<S> NanocodexBuilder<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    S::Error: Into<NanocodexError> + Send + 'static,
    S::Future: Send,
{
    /// Builds and spawns the agent with the caller's Responses service stack,
    /// returning its prompt handle and ordered event stream.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration or when no Tokio runtime is
    /// active.
    pub fn build(mut self) -> Result<(Nanocodex, AgentEvents)> {
        configure(&mut self.config, &self.responses);
        validate(&self.config, self.session_id.as_deref())?;
        build_agent(
            Arc::new(self.config),
            self.tools,
            self.workspace,
            self.session_id,
            self.responses.service,
        )
    }
}

/// Sole owner of mutable run state and the Responses service stack.
struct AgentDriver<S> {
    commands: mpsc::Receiver<Command>,
    config: Arc<ModelConfig>,
    events: EventSink,
    client: ResponsesClient<S>,
    transport_stats: Arc<TransportStats>,
    tools: Tools,
}

impl<S> AgentDriver<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    S::Error: Into<NanocodexError> + Send + 'static,
    S::Future: Send,
{
    /// Drives queued turns until every command handle is dropped.
    ///
    /// # Errors
    ///
    /// Returns an infrastructure error while receiving or starting a command.
    async fn run(mut self) -> Result<()> {
        self.tools.start_providers();
        let session_id = self.events.request_id().to_owned();
        let thinking = self.config.thinking;
        let mut model = ModelRun::new(
            self.events,
            self.config,
            self.client,
            self.transport_stats,
            self.tools,
        );
        let mut turn_index = 0_u64;
        while let Some(Command::Prompt { prompt, result }) = self.commands.recv().await {
            turn_index += 1;
            let turn_span = info_span!(
                target: "nanocodex",
                parent: None,
                "agent.turn",
                otel.kind = "internal",
                otel.status_code = tracing::field::Empty,
                session.id = session_id.as_str(),
                model = nanocodex_core::MODEL,
                thinking = thinking.as_str(),
                turn.index = turn_index,
                prompt.bytes = prompt.instruction.text_bytes(),
                status = tracing::field::Empty,
            );
            let outcome = model
                .execute(prompt)
                .instrument(turn_span.clone())
                .await
                .map(|final_message| TurnResult { final_message });
            turn_span.record(
                "status",
                if outcome.is_ok() {
                    "completed"
                } else {
                    "failed"
                },
            );
            turn_span.record(
                "otel.status_code",
                if outcome.is_ok() { "OK" } else { "ERROR" },
            );
            drop(result.send(outcome));
        }
        Ok(())
    }
}

fn build_agent<S>(
    config: Arc<ModelConfig>,
    tools: Tools,
    workspace: Option<PathBuf>,
    session_id: Option<String>,
    service: S,
) -> Result<(Nanocodex, AgentEvents)>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    S::Error: Into<NanocodexError> + Send + 'static,
    S::Future: Send,
{
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|_| NanocodexError::Agent(AgentError::TokioRuntimeUnavailable))?;
    let session_id = session_id.unwrap_or_else(new_session_id);
    let (events, event_stream) = EventSink::channel(session_id);
    let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
    let transport_stats = Arc::new(TransportStats::default());
    let workspace = workspace
        .map(|path| {
            path.into_os_string()
                .into_string()
                .map(Arc::<str>::from)
                .map_err(|path| AgentError::WorkspaceNotUtf8 {
                    path: PathBuf::from(path),
                })
        })
        .transpose()?;
    let agent = Nanocodex {
        commands,
        workspace,
    };
    drop(
        runtime.spawn(
            AgentDriver {
                commands: receiver,
                config,
                events,
                client: ResponsesClient::new(service),
                transport_stats,
                tools,
            }
            .run(),
        ),
    );
    Ok((agent, event_stream))
}

fn configure<S>(config: &mut ModelConfig, responses: &Responses<S>) {
    config.websocket_url.clone_from(&responses.websocket_url);
    config.api_base_url.clone_from(&responses.api_base_url);
}

fn validate(config: &ModelConfig, session_id: Option<&str>) -> Result<()> {
    if config.api_key.trim().is_empty() {
        return Err(NanocodexError::InvalidRequest(
            "OpenAI API key must not be empty".to_owned(),
        ));
    }
    if config.websocket_url.trim().is_empty() {
        return Err(NanocodexError::InvalidRequest(
            "Responses WebSocket URL must not be empty".to_owned(),
        ));
    }
    if config.api_base_url.trim().is_empty() {
        return Err(NanocodexError::InvalidRequest(
            "OpenAI API base URL must not be empty".to_owned(),
        ));
    }
    if session_id.is_some_and(|session_id| session_id.trim().is_empty()) {
        return Err(NanocodexError::InvalidRequest(
            "session_id must not be empty".to_owned(),
        ));
    }
    Ok(())
}

fn new_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("nanocodex-{timestamp:x}")
}

#[cfg(test)]
mod tests {
    use std::{future::Ready, time::Duration};

    use super::*;
    use tower::{ServiceBuilder, limit::ConcurrencyLimitLayer, timeout::TimeoutLayer};

    #[derive(Clone)]
    struct NeverCalled;

    impl Service<ResponsesAttempt> for NeverCalled {
        type Response = ResponsesServiceResponse;
        type Error = NanocodexError;
        type Future = Ready<std::result::Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, _request: ResponsesAttempt) -> Self::Future {
            panic!("the service is not called by this test")
        }
    }

    #[tokio::test]
    async fn accepts_a_caller_composed_tower_stack() {
        let stack = ServiceBuilder::new()
            .layer(TimeoutLayer::new(Duration::from_secs(30)))
            .layer(ConcurrencyLimitLayer::new(1))
            .service(NeverCalled);
        let responses = Responses::builder().service(stack).build();

        let (_agent, events) = Nanocodex::builder("test")
            .responses(responses)
            .build()
            .unwrap();
        drop(events);
    }

    #[tokio::test]
    async fn defers_layers_until_the_standard_service_is_built() {
        let responses = Responses::builder()
            .layer(TimeoutLayer::new(Duration::from_secs(30)))
            .layer(ConcurrencyLimitLayer::new(1))
            .build();

        let (_agent, events) = Nanocodex::builder("test")
            .responses(responses)
            .build()
            .unwrap();
        drop(events);
    }

    #[test]
    fn building_requires_a_tokio_runtime() {
        assert!(matches!(
            Nanocodex::new("test"),
            Err(NanocodexError::Agent(AgentError::TokioRuntimeUnavailable))
        ));
    }
}
