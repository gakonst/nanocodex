use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use harness_core::{AgentEvents, EventSink, ModelConfig, Prompt};
use harness_service::{
    DefaultResponsesService, ResponsesAttempt, ResponsesClient, ResponsesService,
    ResponsesServiceResponse, TransportStats,
};
use tokio::sync::{mpsc, oneshot};
use tower::Service;

use crate::{AgentError, HarnessError, Result, model::agent::ModelRun};

const COMMAND_CAPACITY: usize = 8;

/// How a submitted prompt was accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PromptDisposition {
    Started,
    Queued,
}

/// Receipt returned as soon as a prompt is accepted by the driver.
pub struct PromptReceipt {
    pub disposition: PromptDisposition,
    pub turn: Turn,
}

/// Completion handle for an accepted turn.
pub struct Turn {
    result: oneshot::Receiver<Result<TurnOutcome>>,
}

impl Turn {
    /// Waits for the final typed turn result.
    ///
    /// # Errors
    ///
    /// Returns the model-run failure or an error if the driver stopped early.
    pub async fn completed(self) -> Result<TurnOutcome> {
        self.result
            .await
            .map_err(|_| HarnessError::Agent(AgentError::TurnStopped))?
    }
}

/// Final outcome of a completed turn.
#[derive(Clone, Debug)]
pub struct TurnOutcome {
    pub final_message: String,
}

enum Command {
    Prompt {
        prompt: Prompt,
        result: oneshot::Sender<Result<TurnOutcome>>,
    },
}

/// Cheap, cloneable command handle for an owned agent driver.
#[derive(Clone)]
pub struct Agent {
    commands: mpsc::Sender<Command>,
    pending: Arc<AtomicUsize>,
}

impl Agent {
    /// Starts configuring an agent with the standard `OpenAI` WebSocket stack.
    #[must_use]
    pub fn builder(config: ModelConfig) -> AgentBuilder {
        AgentBuilder {
            config,
            request_id: None,
        }
    }

    /// Accepts the agent's prompt and immediately returns its turn handle.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty prompt or if the driver stopped.
    pub async fn prompt(&self, prompt: Prompt) -> Result<PromptReceipt> {
        if prompt.instruction.is_empty() {
            return Err(HarnessError::InvalidRequest(
                "prompt instruction must not be empty".to_owned(),
            ));
        }
        let queued_before = self.pending.fetch_add(1, Ordering::AcqRel);
        let (result, receiver) = oneshot::channel();
        if self
            .commands
            .send(Command::Prompt { prompt, result })
            .await
            .is_err()
        {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            return Err(HarnessError::Agent(AgentError::DriverStopped));
        }
        Ok(PromptReceipt {
            disposition: if queued_before == 0 {
                PromptDisposition::Started
            } else {
                PromptDisposition::Queued
            },
            turn: Turn { result: receiver },
        })
    }
}

/// Builder for a handle, driver, and event stream with deferred service composition.
pub struct AgentBuilder {
    config: ModelConfig,
    request_id: Option<String>,
}

impl AgentBuilder {
    /// Sets the stable session/request ID used for headers and prompt caching.
    #[must_use]
    pub fn request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Replaces the configured transport with a fully caller-composed Tower stack.
    #[must_use]
    pub fn responses_service<S>(self, service: S) -> CustomAgentBuilder<S> {
        CustomAgentBuilder {
            config: self.config,
            request_id: self.request_id,
            service,
        }
    }

    /// Builds the standard persistent-WebSocket service and retry policy.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured request ID is empty.
    pub fn build_parts(self) -> Result<AgentParts<DefaultResponsesService>> {
        let config = Arc::new(self.config);
        let service = ResponsesService::standard(Arc::clone(&config));
        build_parts(config, self.request_id, service)
    }
}

/// Builder state containing a caller-composed Tower service.
pub struct CustomAgentBuilder<S> {
    config: ModelConfig,
    request_id: Option<String>,
    service: S,
}

impl<S> CustomAgentBuilder<S> {
    /// Sets the stable session/request ID used for headers and prompt caching.
    #[must_use]
    pub fn request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }
}

impl<S> CustomAgentBuilder<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    S::Error: Into<HarnessError>,
    S::Future: Send,
{
    /// Builds with the caller's already-composed Tower service stack.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured request ID is empty.
    pub fn build_parts(self) -> Result<AgentParts<S>> {
        build_parts(Arc::new(self.config), self.request_id, self.service)
    }
}

/// The three independently owned pieces of a configured agent.
pub struct AgentParts<S> {
    pub agent: Agent,
    pub driver: AgentDriver<S>,
    pub events: AgentEvents,
}

/// Sole owner of mutable run state and the Responses service stack.
pub struct AgentDriver<S> {
    commands: mpsc::Receiver<Command>,
    config: Arc<ModelConfig>,
    events: EventSink,
    client: ResponsesClient<S>,
    transport_stats: Arc<TransportStats>,
    pending: Arc<AtomicUsize>,
}

impl<S> AgentDriver<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    S::Error: Into<HarnessError>,
    S::Future: Send,
{
    /// Drives queued turns until every command handle is dropped.
    ///
    /// # Errors
    ///
    /// Returns an infrastructure error while receiving or starting a command.
    pub async fn run(mut self) -> Result<()> {
        let mut model = ModelRun::new(self.events, self.config, self.client, self.transport_stats);
        while let Some(Command::Prompt { prompt, result }) = self.commands.recv().await {
            let outcome = model
                .execute(prompt)
                .await
                .map(|final_message| TurnOutcome { final_message });
            self.pending.fetch_sub(1, Ordering::AcqRel);
            drop(result.send(outcome));
        }
        Ok(())
    }
}

fn build_parts<S>(
    config: Arc<ModelConfig>,
    request_id: Option<String>,
    service: S,
) -> Result<AgentParts<S>> {
    let request_id = request_id.unwrap_or_else(new_request_id);
    if request_id.trim().is_empty() {
        return Err(HarnessError::InvalidRequest(
            "request_id must not be empty".to_owned(),
        ));
    }
    let (events, event_stream) = EventSink::channel(request_id);
    let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
    let transport_stats = Arc::new(TransportStats::default());
    let pending = Arc::new(AtomicUsize::new(0));
    Ok(AgentParts {
        agent: Agent {
            commands,
            pending: Arc::clone(&pending),
        },
        driver: AgentDriver {
            commands: receiver,
            config,
            events,
            client: ResponsesClient::new(service),
            transport_stats,
            pending,
        },
        events: event_stream,
    })
}

fn new_request_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("harness-{timestamp:x}")
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
        type Error = HarnessError;
        type Future = Ready<std::result::Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, _request: ResponsesAttempt) -> Self::Future {
            panic!("the driver is not run by this test")
        }
    }

    #[tokio::test]
    async fn queues_a_second_prompt_before_it_reaches_the_driver() {
        let config = ModelConfig {
            model: "test".to_owned(),
            api_key: "test".to_owned(),
            effort: crate::ReasoningEffort::Low,
            web_search: true,
            websocket_url: "ws://localhost".to_owned(),
            api_base_url: "http://localhost".to_owned(),
        };
        let parts = Agent::builder(config)
            .responses_service(NeverCalled)
            .build_parts()
            .unwrap();
        let first = parts.agent.prompt(Prompt::new("first")).await;
        assert!(first.is_ok());
        let second = parts.agent.prompt(Prompt::new("second")).await;
        assert!(matches!(
            second,
            Ok(PromptReceipt {
                disposition: PromptDisposition::Queued,
                ..
            })
        ));
    }

    #[test]
    fn accepts_a_caller_composed_tower_stack() {
        let config = ModelConfig {
            model: "test".to_owned(),
            api_key: "test".to_owned(),
            effort: crate::ReasoningEffort::Low,
            web_search: false,
            websocket_url: "ws://localhost".to_owned(),
            api_base_url: "http://localhost".to_owned(),
        };
        let stack = ServiceBuilder::new()
            .layer(TimeoutLayer::new(Duration::from_secs(30)))
            .layer(ConcurrencyLimitLayer::new(1))
            .service(NeverCalled);

        assert!(
            Agent::builder(config)
                .responses_service(stack)
                .build_parts()
                .is_ok()
        );
    }
}
