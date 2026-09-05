use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use nanocodex_agent::{
    AgentEvents, BuilderBackend, Model, Nanocodex, NanocodexError, ReasoningMode, Thinking,
    backend::BackendRuntime,
};
use tokio::sync::mpsc;
use tower::{Layer, Service, ServiceExt};

#[cfg(feature = "tools")]
use nanocodex_tools::{
    Tools,
    attachment::{AttachmentMetadata, AttachmentTarget},
};

#[cfg(feature = "tools")]
use crate::attachment::AttachmentSupervisor;
use crate::{
    AgentReceipt, AgentSettings, AgentState, EventCursor, ManagedClient, ManagedError,
    ManagedEvent, ManagedEvents, PromptInput, TurnAction, TurnView,
    driver::{ManagedAgent, ManagedDriver},
    websocket::ManagedSocket,
};

/// One owned operation accepted by a managed Tower service.
#[derive(Debug)]
#[non_exhaustive]
pub enum ManagedRequest {
    /// Creates a new account-owned agent.
    Create {
        /// Initial model and reasoning policy.
        settings: AgentSettings,
    },
    /// Reads current durable agent state.
    State {
        /// Stable managed agent identifier.
        agent_id: String,
    },
    /// Opens the durable event stream at an exact cursor.
    Events {
        /// Stable managed agent identifier.
        agent_id: String,
        /// Last completely observed durable cursor.
        cursor: EventCursor,
    },
    /// Submits a new server-identified turn.
    Submit {
        /// Stable managed agent identifier.
        agent_id: String,
        /// Optional caller-selected turn identifier.
        turn_id: Option<String>,
        /// Stable idempotency key for this logical request.
        idempotency_key: String,
        /// Complete managed prompt input.
        input: PromptInput,
    },
    /// Adds input to an active turn.
    Steer {
        /// Stable managed agent identifier.
        agent_id: String,
        /// Server-owned turn identifier.
        turn_id: String,
        /// Additional managed prompt input.
        input: PromptInput,
    },
    /// Requests cancellation of an active turn.
    Cancel {
        /// Stable managed agent identifier.
        agent_id: String,
        /// Server-owned turn identifier.
        turn_id: String,
    },
    /// Selects the model before the first accepted turn.
    SetModel {
        /// Stable managed agent identifier.
        agent_id: String,
        /// New hosted model.
        model: Model,
    },
    /// Selects the reasoning mode before the first accepted turn.
    SetReasoningMode {
        /// Stable managed agent identifier.
        agent_id: String,
        /// New reasoning execution mode.
        reasoning_mode: ReasoningMode,
    },
    /// Changes reasoning effort for subsequently accepted turns.
    SetThinking {
        /// Stable managed agent identifier.
        agent_id: String,
        /// New reasoning effort.
        thinking: Thinking,
    },
    /// Changes priority processing for subsequently accepted turns.
    SetFastMode {
        /// Stable managed agent identifier.
        agent_id: String,
        /// Whether priority processing is enabled.
        enabled: bool,
    },
    /// Resolves the authenticated reverse-tool attachment target.
    #[cfg(feature = "tools")]
    AttachmentTarget {
        /// Stable managed agent identifier.
        agent_id: String,
    },
}

/// Typed response produced by a managed Tower service.
#[derive(Debug)]
#[non_exhaustive]
pub enum ManagedResponse {
    /// Receipt for a newly created managed agent.
    Created(AgentReceipt),
    /// Current durable agent state.
    State(AgentState),
    /// Opened durable event stream.
    Events(ManagedEvents),
    /// Current view of a submitted turn.
    Submitted(TurnView),
    /// Receipt for a steer operation.
    Steered(TurnAction),
    /// Receipt for a cancel operation.
    Cancelled(TurnAction),
    /// Complete settings after a successful mutation.
    Settings(AgentSettings),
    /// Authenticated reverse-tool attachment target.
    #[cfg(feature = "tools")]
    AttachmentTarget(AttachmentTarget),
}

/// Default concrete managed Tower service backed by [`ManagedClient`].
#[derive(Clone, Debug)]
pub struct ManagedService {
    client: ManagedClient,
    socket: Arc<tokio::sync::Mutex<Option<ManagedLiveSocket>>>,
    transport: ManagedTransport,
}

#[derive(Debug)]
struct ManagedLiveSocket {
    agent_id: String,
    socket: ManagedSocket,
    events: Option<crate::websocket::ManagedSocketEvents>,
}

#[derive(Clone, Copy, Debug)]
enum ManagedTransport {
    Http,
    WebSocket,
}

impl ManagedService {
    fn new(client: ManagedClient, transport: ManagedTransport) -> Self {
        Self {
            client,
            socket: Arc::new(tokio::sync::Mutex::new(None)),
            transport,
        }
    }
}

impl Service<ManagedRequest> for ManagedService {
    type Response = ManagedResponse;
    type Error = ManagedError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ManagedRequest) -> Self::Future {
        let client = self.client.clone();
        let socket = Arc::clone(&self.socket);
        let transport = self.transport;
        Box::pin(async move {
            match request {
                ManagedRequest::Create { settings } => match transport {
                    ManagedTransport::Http => client
                        .create_with_settings(settings)
                        .await
                        .map(ManagedResponse::Created),
                    ManagedTransport::WebSocket => {
                        let (receipt, live, events) =
                            ManagedSocket::create(client.clone(), settings).await?;
                        *socket.lock().await = Some(ManagedLiveSocket {
                            agent_id: receipt.agent_id.clone(),
                            socket: live,
                            events: Some(events),
                        });
                        Ok(ManagedResponse::Created(receipt))
                    }
                },
                ManagedRequest::State { agent_id } => {
                    client.state(&agent_id).await.map(ManagedResponse::State)
                }
                ManagedRequest::Events { agent_id, cursor } => match transport {
                    ManagedTransport::Http => {
                        // Reading retained state must not wait for the live
                        // stream to become available. The driver reconnects
                        // from this cursor while the caller renders history.
                        let events = client.events(&agent_id, cursor)?;
                        Ok(ManagedResponse::Events(ManagedEvents::new(events)))
                    }
                    ManagedTransport::WebSocket => {
                        {
                            let mut socket = socket.lock().await;
                            if let Some(live) =
                                socket.as_mut().filter(|live| live.agent_id == agent_id)
                            {
                                let events = live.events.as_ref().ok_or_else(|| {
                                    ManagedError::Configuration(
                                        "managed create WebSocket events were already consumed"
                                            .to_owned(),
                                    )
                                })?;
                                if events.cursor().as_str() != cursor.as_str() {
                                    return Err(ManagedError::Configuration(
                                        "managed create WebSocket cursor does not match ready state"
                                            .to_owned(),
                                    ));
                                }
                                let events = live
                                    .events
                                    .take()
                                    .expect("managed create WebSocket events were just observed");
                                return Ok(ManagedResponse::Events(ManagedEvents::new(events)));
                            }
                        }
                        let (live, events) =
                            ManagedSocket::open(client.clone(), agent_id.clone(), cursor).await?;
                        *socket.lock().await = Some(ManagedLiveSocket {
                            agent_id,
                            socket: live,
                            events: None,
                        });
                        Ok(ManagedResponse::Events(ManagedEvents::new(events)))
                    }
                },
                ManagedRequest::Submit {
                    agent_id,
                    turn_id,
                    idempotency_key,
                    input,
                } => {
                    if matches!(transport, ManagedTransport::Http) {
                        return client
                            .submit(&agent_id, turn_id.as_deref(), &idempotency_key, &input)
                            .await
                            .map(ManagedResponse::Submitted);
                    }
                    if turn_id
                        .as_deref()
                        .is_some_and(|turn_id| turn_id != idempotency_key)
                    {
                        return Err(ManagedError::Configuration(
                            "managed WebSocket turn ID must match its idempotency key".to_owned(),
                        ));
                    }
                    let live = socket
                        .lock()
                        .await
                        .as_ref()
                        .filter(|live| live.agent_id == agent_id)
                        .map(|live| live.socket.clone());
                    let live = live.ok_or_else(|| {
                        ManagedError::Configuration(
                            "managed WebSocket is not open for this agent".to_owned(),
                        )
                    })?;
                    live.submit(idempotency_key, input)
                        .await
                        .map(ManagedResponse::Submitted)
                }
                ManagedRequest::Steer {
                    agent_id,
                    turn_id,
                    input,
                } => client
                    .steer(&agent_id, &turn_id, &input)
                    .await
                    .map(ManagedResponse::Steered),
                ManagedRequest::Cancel { agent_id, turn_id } => client
                    .cancel(&agent_id, &turn_id)
                    .await
                    .map(ManagedResponse::Cancelled),
                ManagedRequest::SetModel { agent_id, model } => client
                    .set_model(&agent_id, model)
                    .await
                    .map(ManagedResponse::Settings),
                ManagedRequest::SetReasoningMode {
                    agent_id,
                    reasoning_mode,
                } => client
                    .set_reasoning_mode(&agent_id, reasoning_mode)
                    .await
                    .map(ManagedResponse::Settings),
                ManagedRequest::SetThinking { agent_id, thinking } => client
                    .set_thinking(&agent_id, thinking)
                    .await
                    .map(ManagedResponse::Settings),
                ManagedRequest::SetFastMode { agent_id, enabled } => client
                    .set_fast_mode(&agent_id, enabled)
                    .await
                    .map(ManagedResponse::Settings),
                #[cfg(feature = "tools")]
                ManagedRequest::AttachmentTarget { agent_id } => client
                    .attachment_target(&agent_id)
                    .map(ManagedResponse::AttachmentTarget),
            }
        })
    }
}

/// Account-managed lifecycle recipe accepted by [`Nanocodex::builder`].
#[derive(Clone, Debug)]
pub struct Managed<S = ManagedService> {
    service: S,
    operation: ManagedOperation,
}

#[derive(Clone, Debug)]
enum ManagedOperation {
    Create(AgentSettings),
    Open(String),
    OpenFromState(String, AgentState),
}

impl Managed<ManagedService> {
    /// Selects creation of a new account-owned managed agent.
    #[must_use]
    pub fn create(client: ManagedClient) -> Self {
        Self {
            service: ManagedService::new(client, ManagedTransport::Http),
            operation: ManagedOperation::Create(AgentSettings::default()),
        }
    }

    /// Selects creation over the resumable managed WebSocket transport.
    #[must_use]
    pub fn create_live(client: ManagedClient) -> Self {
        Self {
            service: ManagedService::new(client, ManagedTransport::WebSocket),
            operation: ManagedOperation::Create(AgentSettings::default()),
        }
    }

    /// Selects an existing account-owned managed agent by stable identifier.
    #[must_use]
    pub fn open(client: ManagedClient, agent_id: impl Into<String>) -> Self {
        Self {
            service: ManagedService::new(client, ManagedTransport::Http),
            operation: ManagedOperation::Open(agent_id.into()),
        }
    }

    /// Opens an existing agent over the resumable managed WebSocket transport.
    #[must_use]
    pub fn open_live(client: ManagedClient, agent_id: impl Into<String>) -> Self {
        Self {
            service: ManagedService::new(client, ManagedTransport::WebSocket),
            operation: ManagedOperation::Open(agent_id.into()),
        }
    }

    /// Opens an existing account-owned agent from one already validated state response.
    ///
    /// This preserves the state/event cursor fence without repeating the
    /// authenticated state request when a caller also needs the state for
    /// presentation hydration.
    #[must_use]
    pub fn open_from_state(
        client: ManagedClient,
        agent_id: impl Into<String>,
        state: AgentState,
    ) -> Self {
        Self {
            service: ManagedService::new(client, ManagedTransport::Http),
            operation: ManagedOperation::OpenFromState(agent_id.into(), state),
        }
    }

    /// Opens an existing agent from validated state over the resumable WebSocket transport.
    #[must_use]
    pub fn open_live_from_state(
        client: ManagedClient,
        agent_id: impl Into<String>,
        state: AgentState,
    ) -> Self {
        Self {
            service: ManagedService::new(client, ManagedTransport::WebSocket),
            operation: ManagedOperation::OpenFromState(agent_id.into(), state),
        }
    }
}

impl<S> Managed<S> {
    /// Replaces the initial settings on a managed create recipe.
    ///
    /// This has no effect on open recipes; existing agents hydrate their
    /// settings from durable state and use explicit setting mutations.
    #[must_use]
    pub fn with_settings(mut self, settings: AgentSettings) -> Self {
        if matches!(self.operation, ManagedOperation::Create(_)) {
            self.operation = ManagedOperation::Create(settings);
        }
        self
    }
}

impl<S> BuilderBackend for Managed<S> {
    type Builder = ManagedBuilder<S>;

    fn into_builder(self) -> Self::Builder {
        ManagedBuilder {
            managed: self,
            event_observer: None,
            #[cfg(feature = "tools")]
            tools: None,
            #[cfg(feature = "tools")]
            attachment_metadata: None,
        }
    }
}

/// Builder for one account-managed agent lifecycle.
#[derive(Debug)]
pub struct ManagedBuilder<S = ManagedService> {
    managed: Managed<S>,
    event_observer: Option<mpsc::UnboundedSender<ManagedEvent>>,
    #[cfg(feature = "tools")]
    tools: Option<Tools>,
    #[cfg(feature = "tools")]
    attachment_metadata: Option<AttachmentMetadata>,
}

impl<S> ManagedBuilder<S> {
    /// Replaces the managed Tower service while preserving create/open intent.
    #[must_use]
    pub fn service<T>(self, service: T) -> ManagedBuilder<T> {
        ManagedBuilder {
            managed: Managed {
                service,
                operation: self.managed.operation,
            },
            event_observer: self.event_observer,
            #[cfg(feature = "tools")]
            tools: self.tools,
            #[cfg(feature = "tools")]
            attachment_metadata: self.attachment_metadata,
        }
    }

    /// Wraps the concrete managed Tower service in caller middleware.
    #[must_use]
    pub fn layer<L>(self, layer: L) -> ManagedBuilder<L::Service>
    where
        L: Layer<S>,
    {
        let Self {
            managed,
            event_observer,
            #[cfg(feature = "tools")]
            tools,
            #[cfg(feature = "tools")]
            attachment_metadata,
        } = self;
        ManagedBuilder {
            managed: Managed {
                service: layer.layer(managed.service),
                operation: managed.operation,
            },
            event_observer,
            #[cfg(feature = "tools")]
            tools,
            #[cfg(feature = "tools")]
            attachment_metadata,
        }
    }

    /// Copies the driver's ordered durable envelopes to a presentation observer.
    ///
    /// A closed observer is ignored and never affects the managed lifecycle.
    #[must_use]
    pub fn event_observer(mut self, observer: mpsc::UnboundedSender<ManagedEvent>) -> Self {
        self.event_observer = Some(observer);
        self
    }

    /// Attaches one caller-owned immutable tool recipe to the managed agent.
    #[cfg(feature = "tools")]
    #[cfg_attr(docsrs, doc(cfg(feature = "tools")))]
    #[must_use]
    pub fn tools(mut self, tools: Tools) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Publishes stable, non-secret routing metadata for the attached tools.
    #[cfg(feature = "tools")]
    #[cfg_attr(docsrs, doc(cfg(feature = "tools")))]
    #[must_use]
    pub fn attachment_metadata(mut self, metadata: AttachmentMetadata) -> Self {
        self.attachment_metadata = Some(metadata);
        self
    }

    /// Creates or opens the managed agent and starts its owned lifecycle driver.
    ///
    /// # Errors
    ///
    /// Returns a managed service, response, event-cursor, or runtime
    /// construction failure.
    pub async fn build(mut self) -> nanocodex_agent::Result<(Nanocodex, AgentEvents)>
    where
        S: Service<ManagedRequest, Response = ManagedResponse> + Send + 'static,
        S::Future: Send + 'static,
        S::Error: std::error::Error + Send + Sync + 'static,
    {
        let (agent_id, expected_session_id, supplied_state) = match self.managed.operation {
            ManagedOperation::Create(settings) => {
                let settings = settings.validate().map_err(backend_error)?;
                match call(
                    &mut self.managed.service,
                    ManagedRequest::Create { settings },
                )
                .await?
                {
                    ManagedResponse::Created(receipt) => (
                        receipt.agent_id,
                        Some(receipt.session_id),
                        receipt.initial_state,
                    ),
                    _ => return Err(unexpected_response()),
                }
            }
            ManagedOperation::Open(agent_id) => (agent_id, None, None),
            ManagedOperation::OpenFromState(agent_id, state) => (agent_id, None, Some(state)),
        };

        let state_and_cursor = async {
            let state = match supplied_state {
                Some(state) => state,
                None => match call(
                    &mut self.managed.service,
                    ManagedRequest::State {
                        agent_id: agent_id.clone(),
                    },
                )
                .await?
                {
                    ManagedResponse::State(state) => state,
                    _ => return Err(unexpected_response()),
                },
            };
            if state.agent_id != agent_id {
                return Err(backend_error(ManagedError::InvalidResponse(
                    "agent state identity does not match the requested agent",
                )));
            }
            if expected_session_id
                .as_deref()
                .is_some_and(|expected| expected != state.session_id)
            {
                return Err(backend_error(ManagedError::InvalidResponse(
                    "created agent receipt and state session identities differ",
                )));
            }
            if state.stream_error.is_some() {
                return Err(backend_error(ManagedError::InvalidResponse(
                    "agent state reports a durable stream failure",
                )));
            }
            if !state.settings.is_valid() {
                return Err(backend_error(ManagedError::InvalidResponse(
                    "agent state contains incompatible model and reasoning settings",
                )));
            }
            crate::sse::validate_numeric_cursor(&state.latest_event_cursor).map_err(|_| {
                backend_error(ManagedError::InvalidResponse(
                    "agent state latest event cursor is invalid",
                ))
            })?;
            let cursor =
                EventCursor::parse(state.latest_event_cursor.clone()).map_err(backend_error)?;
            Ok((state, cursor))
        }
        .await;
        let (state, cursor) = match state_and_cursor {
            Ok(result) => result,
            Err(error) => return Err(error),
        };

        #[cfg(feature = "tools")]
        let attachment = match self.tools {
            Some(tools) => {
                let target = match call(
                    &mut self.managed.service,
                    ManagedRequest::AttachmentTarget {
                        agent_id: agent_id.clone(),
                    },
                )
                .await
                {
                    Ok(ManagedResponse::AttachmentTarget(target)) => target,
                    Ok(_) => return Err(unexpected_response()),
                    Err(error) => return Err(error),
                };
                Some(
                    AttachmentSupervisor::start(tools, target, self.attachment_metadata)
                        .map_err(backend_error)?,
                )
            }
            None => None,
        };

        let stream_response = match call(
            &mut self.managed.service,
            ManagedRequest::Events {
                agent_id: agent_id.clone(),
                cursor,
            },
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                #[cfg(feature = "tools")]
                if let Some(attachment) = attachment.as_ref() {
                    let _ = attachment.shutdown().await;
                }
                return Err(error);
            }
        };

        let stream = match stream_response {
            ManagedResponse::Events(stream) => stream,
            _ => {
                #[cfg(feature = "tools")]
                if let Some(attachment) = attachment {
                    let _ = attachment.shutdown().await;
                }
                return Err(unexpected_response());
            }
        };

        let (runtime, events) =
            BackendRuntime::with_agent_id(agent_id.clone(), state.session_id.clone());
        let (backend, commands, shutdown) = ManagedAgent::new();
        let driver = ManagedDriver::new(
            self.managed.service,
            agent_id,
            stream,
            commands,
            runtime.events(),
            shutdown,
            self.event_observer,
            #[cfg(feature = "tools")]
            attachment,
        );
        tokio::spawn(driver.run());
        Ok((runtime.bind(backend), events))
    }
}

pub(crate) async fn call<S>(
    service: &mut S,
    request: ManagedRequest,
) -> nanocodex_agent::Result<ManagedResponse>
where
    S: Service<ManagedRequest, Response = ManagedResponse> + Send,
    S::Future: Send,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    service
        .ready()
        .await
        .map_err(backend_error)?
        .call(request)
        .await
        .map_err(backend_error)
}

pub(crate) const fn unexpected_response() -> NanocodexError {
    NanocodexError::BackendContract {
        detail: "managed Tower service returned the wrong response variant",
    }
}

pub(crate) fn backend_error(
    error: impl std::error::Error + Send + Sync + 'static,
) -> NanocodexError {
    NanocodexError::Backend {
        backend: "managed",
        source: std::sync::Arc::new(error),
    }
}
