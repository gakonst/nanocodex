#[cfg(feature = "openai")]
use super::handle::{request_fork, request_spawn};
use super::*;

/// Input that selects the concrete builder returned by [`Nanocodex::builder`].
///
/// Backend types use the associated builder to expose their own deliberate
/// policy while sharing the same lifecycle handle after build.
pub trait BuilderBackend {
    /// Builder configured by this backend input.
    type Builder;

    /// Converts the backend input into its concrete builder.
    fn into_builder(self) -> Self::Builder;
}

/// Backend operation future used at the lifecycle-erasure boundary.
#[cfg(not(target_family = "wasm"))]
pub type BackendFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Backend operation future used at the lifecycle-erasure boundary.
#[cfg(target_family = "wasm")]
pub type BackendFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

/// Opaque key assigned by the common agent handle before backend admission.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BackendTurnKey(pub u64);

/// Validated prompt input handed to one lifecycle backend.
#[doc(hidden)]
pub struct BackendPrompt {
    /// Common handle identity for control operations.
    pub key: BackendTurnKey,
    /// Validated prompt input.
    pub prompt: Prompt,
    /// Optional caller-owned durable operation identity.
    pub request_id: Option<String>,
    /// Whether the prompt must be durably cancelled as part of admission.
    pub cancel_on_admission: bool,
    /// Canonical publisher routing events to both session and turn streams.
    pub events: nanocodex_oai_api::events::AgentEventPublisher,
}

/// One admitted backend turn and its independently awaitable result.
#[doc(hidden)]
pub struct BackendTurn {
    /// Durable request identity selected during admission, when any.
    pub request_id: Option<String>,
    /// Independently awaitable terminal result.
    pub result: BackendFuture<Result<TurnResult>>,
}

/// Atomic live-routing decision made by a backend driver.
#[doc(hidden)]
pub enum BackendPromptRoute {
    /// The input started a new turn.
    Started(BackendTurn),
    /// The input was steered into the active turn.
    Steered,
}

/// Backend implementor contract behind the common `Nanocodex` lifecycle.
///
/// This trait erases only lifecycle control after a concrete driver has been
/// built. Provider Tower services remain concrete, driver-owned types.
#[doc(hidden)]
pub trait LifecycleBackend: Send + Sync + 'static {
    /// Admits one prompt and returns the complete accepted turn.
    fn submit(&self, prompt: BackendPrompt) -> BackendFuture<Result<BackendTurn>>;

    /// Atomically starts a turn or steers the active one.
    fn route(&self, prompt: BackendPrompt) -> BackendFuture<Result<BackendPromptRoute>>;

    /// Steers one exact active turn.
    fn steer(&self, key: BackendTurnKey, prompt: Prompt) -> BackendFuture<Result<()>>;

    /// Cancels one exact unfinished turn.
    fn cancel(&self, key: BackendTurnKey) -> BackendFuture<Result<()>>;

    /// Changes the model before the first turn is accepted.
    fn set_model(&self, model: Model) -> BackendFuture<Result<()>>;

    /// Changes reasoning policy for later turns.
    fn set_thinking(&self, thinking: Thinking) -> BackendFuture<Result<()>>;

    /// Changes priority policy for later turns.
    fn set_fast_mode(&self, enabled: bool) -> BackendFuture<Result<()>>;

    /// Compacts retained context.
    fn compact(&self) -> BackendFuture<Result<()>>;

    /// Appends adapter-owned developer context.
    fn append_developer_message(&self, text: String) -> BackendFuture<Result<AgentSessionContext>>;

    /// Reads the latest safe model-visible context.
    fn context(&self) -> BackendFuture<Result<AgentSessionContext>>;

    /// Starts a clean sibling lifecycle.
    fn spawn(&self, options: SpawnOptions) -> BackendFuture<Result<(Nanocodex, AgentEvents)>>;

    /// Forks the latest or supplied completed boundary.
    fn fork(
        &self,
        completed: Option<TurnResult>,
    ) -> BackendFuture<Result<(Nanocodex, AgentEvents)>>;

    /// Flushes backend-owned persistence.
    fn flush(&self) -> BackendFuture<Result<()>>;

    /// Disconnects local resources without requesting cancellation of durable work.
    ///
    /// Backends without detached durable execution fall back to ordinary
    /// shutdown.
    fn disconnect(&self) -> BackendFuture<Result<()>> {
        self.shutdown()
    }

    /// Idempotently shuts down local resources.
    fn shutdown(&self) -> BackendFuture<Result<()>>;
}

/// Backend-neutral construction context for the common agent handle.
#[doc(hidden)]
pub struct BackendRuntime {
    agent_id: Arc<str>,
    session_id: Arc<str>,
    #[cfg(feature = "openai")]
    local_session_id: Option<SessionId>,
    events: nanocodex_oai_api::events::AgentEventPublisher,
}

impl BackendRuntime {
    /// Creates one session event channel before the concrete driver starts.
    #[must_use]
    pub fn new(session_id: impl Into<Arc<str>>) -> (Self, AgentEvents) {
        let session_id = session_id.into();
        Self::with_agent_id(Arc::clone(&session_id), session_id)
    }

    /// Creates one session event channel with a distinct durable agent identity.
    #[must_use]
    pub fn with_agent_id(
        agent_id: impl Into<Arc<str>>,
        session_id: impl Into<Arc<str>>,
    ) -> (Self, AgentEvents) {
        let agent_id = agent_id.into();
        let session_id = session_id.into();
        let (events, stream) =
            nanocodex_oai_api::events::AgentEventPublisher::channel(session_id.to_string());
        (
            Self {
                agent_id,
                session_id,
                #[cfg(feature = "openai")]
                local_session_id: None,
                events,
            },
            stream,
        )
    }

    #[cfg(feature = "openai")]
    pub(super) fn new_openai(session_id: SessionId) -> (Self, AgentEvents) {
        let (mut runtime, events) = Self::new(session_id.to_string());
        runtime.local_session_id = Some(session_id);
        (runtime, events)
    }

    /// Returns the canonical event publisher consumed by the concrete driver.
    #[must_use]
    pub fn events(&self) -> nanocodex_oai_api::events::AgentEventPublisher {
        self.events.clone()
    }

    /// Erases one concrete lifecycle implementation into the common handle.
    #[must_use]
    pub fn bind<B>(self, backend: B) -> Nanocodex
    where
        B: LifecycleBackend,
    {
        Nanocodex {
            backend: Arc::new(backend),
            events: self.events,
            next_turn: Arc::new(AtomicU64::new(1)),
            agent_id: self.agent_id,
            session_id: self.session_id,
            #[cfg(feature = "openai")]
            local_session_id: self.local_session_id,
            #[cfg(all(feature = "openai", not(target_family = "wasm")))]
            rollout: None,
        }
    }

    #[cfg(all(feature = "openai", not(target_family = "wasm")))]
    pub(super) fn bind_with_rollout<B>(
        self,
        backend: B,
        rollout: Option<crate::rollout::RolloutInfo>,
    ) -> Nanocodex
    where
        B: LifecycleBackend,
    {
        Nanocodex {
            backend: Arc::new(backend),
            events: self.events,
            next_turn: Arc::new(AtomicU64::new(1)),
            agent_id: self.agent_id,
            session_id: self.session_id,
            local_session_id: self.local_session_id,
            rollout,
        }
    }

    #[cfg(all(feature = "openai", target_family = "wasm"))]
    pub(super) fn bind_with_rollout<B>(self, backend: B, _rollout: Option<()>) -> Nanocodex
    where
        B: LifecycleBackend,
    {
        self.bind(backend)
    }
}

#[cfg(feature = "openai")]
pub(super) struct LocalLifecycle {
    pub(super) commands: mpsc::Sender<Command>,
    pub(super) execution: Execution,
    pub(super) shutdown: DriverShutdown,
    pub(super) lineage_id: Arc<str>,
}

#[cfg(feature = "openai")]
impl LifecycleBackend for LocalLifecycle {
    fn submit(&self, request: BackendPrompt) -> BackendFuture<Result<BackendTurn>> {
        let commands = self.commands.clone();
        let execution = self.execution.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            let execution_operation =
                request
                    .request_id
                    .map(ExecutionOperation::Caller)
                    .or_else(|| {
                        execution
                            .identifies_prompts()
                            .then(|| ExecutionOperation::Automatic(SessionId::new().to_string()))
                    });
            let (result, receiver) = oneshot::channel();
            let (accepted, acceptance) = if execution_operation.is_some() {
                let (accepted, acceptance) = oneshot::channel();
                (Some(accepted), Some(acceptance))
            } else {
                (None, None)
            };
            let parent = tracing::Span::current();
            let parent = (!parent.is_disabled()).then_some(parent);
            if commands
                .send(Command::Prompt {
                    key: TurnKey(request.key.0),
                    prompt: request.prompt,
                    execution_operation,
                    accepted,
                    cancel_on_admission: request.cancel_on_admission,
                    thinking: None,
                    fast_mode: None,
                    parent,
                    events: EventSink::from_publisher(request.events),
                    result,
                })
                .await
                .is_err()
            {
                return Err(shutdown.stopped_error().await);
            }
            let request_id = if let Some(acceptance) = acceptance {
                Some(match acceptance.await {
                    Ok(Ok(request_id)) => request_id,
                    Ok(Err(NanocodexError::AgentStopped)) | Err(_) => {
                        return Err(shutdown.stopped_error().await);
                    }
                    Ok(Err(error)) => return Err(error),
                })
            } else {
                None
            };
            Ok(BackendTurn {
                request_id,
                result: Box::pin(async move {
                    receiver.await.map_err(|_| NanocodexError::TurnStopped)?
                }),
            })
        })
    }

    fn route(&self, request: BackendPrompt) -> BackendFuture<Result<BackendPromptRoute>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            let parent = tracing::Span::current();
            let parent = (!parent.is_disabled()).then_some(parent);
            let (turn_result, turn_receiver) = oneshot::channel();
            let (route_result, route_receiver) = oneshot::channel();
            if commands
                .send(Command::RoutePrompt {
                    key: TurnKey(request.key.0),
                    prompt: request.prompt,
                    parent,
                    events: EventSink::from_publisher(request.events),
                    turn_result,
                    route_result,
                })
                .await
                .is_err()
            {
                return Err(shutdown.stopped_error().await);
            }
            match route_receiver.await {
                Ok(Ok(PromptRouteKind::Started { request_id })) => {
                    Ok(BackendPromptRoute::Started(BackendTurn {
                        request_id,
                        result: Box::pin(async move {
                            turn_receiver
                                .await
                                .map_err(|_| NanocodexError::TurnStopped)?
                        }),
                    }))
                }
                Ok(Ok(PromptRouteKind::Steered)) => Ok(BackendPromptRoute::Steered),
                Ok(Err(NanocodexError::AgentStopped)) | Err(_) => {
                    Err(shutdown.stopped_error().await)
                }
                Ok(Err(error)) => Err(error),
            }
        })
    }

    fn steer(&self, key: BackendTurnKey, prompt: Prompt) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::Steer {
                key: TurnKey(key.0),
                prompt,
                result,
            })
            .await
        })
    }

    fn cancel(&self, key: BackendTurnKey) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::Cancel {
                key: TurnKey(key.0),
                result,
            })
            .await
        })
    }

    fn set_model(&self, model: Model) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::SetModel {
                model,
                result,
            })
            .await
        })
    }

    fn set_thinking(&self, thinking: Thinking) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::SetThinking {
                thinking,
                result,
            })
            .await
        })
    }

    fn set_fast_mode(&self, enabled: bool) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::SetFastMode {
                enabled,
                result,
            })
            .await
        })
    }

    fn compact(&self) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            let parent = tracing::Span::current();
            let parent = (!parent.is_disabled()).then_some(parent);
            request_command(&commands, &shutdown, |result| Command::Compact {
                parent,
                result,
            })
            .await
        })
    }

    fn append_developer_message(&self, text: String) -> BackendFuture<Result<AgentSessionContext>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| {
                Command::AppendDeveloperMessage {
                    text: nanocodex_oai_api::responses::ResponseItem::message(
                        nanocodex_oai_api::responses::MessageRole::Developer,
                        [nanocodex_oai_api::responses::ContentItem::input_text(text)],
                    ),
                    steer_active: false,
                    result,
                }
            })
            .await
        })
    }

    fn context(&self) -> BackendFuture<Result<AgentSessionContext>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::Context { result }).await
        })
    }

    fn spawn(&self, options: SpawnOptions) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move { request_spawn(&commands, &shutdown, options).await })
    }

    fn fork(
        &self,
        completed: Option<TurnResult>,
    ) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        let lineage_id = Arc::clone(&self.lineage_id);
        Box::pin(async move {
            let checkpoint = match completed {
                Some(completed) => {
                    let TurnCheckpoint::Live(checkpoint) = completed.checkpoint else {
                        return Err(NanocodexError::ReplayedCheckpointUnavailable);
                    };
                    if checkpoint.lineage_id() != lineage_id.as_ref() {
                        return Err(NanocodexError::CheckpointLineageMismatch);
                    }
                    Some(checkpoint)
                }
                None => None,
            };
            request_fork(&commands, &shutdown, checkpoint).await
        })
    }

    fn flush(&self) -> BackendFuture<Result<()>> {
        #[cfg(not(target_family = "wasm"))]
        {
            let execution = self.execution.clone();
            Box::pin(async move { execution.flush().await })
        }
        #[cfg(target_family = "wasm")]
        {
            Box::pin(async { Ok(()) })
        }
    }

    fn shutdown(&self) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let execution = self.execution.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            let (initiate, receiver) = shutdown.request();
            if initiate && commands.send(Command::Shutdown).await.is_err() {
                let outcome = match execution.shutdown().await {
                    Ok(()) => Err(NanocodexError::AgentStopped),
                    Err(error) => Err(error),
                };
                shutdown.complete(outcome);
            }
            match receiver.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(NanocodexError::Shutdown(error)),
                Err(_) => Err(NanocodexError::AgentStopped),
            }
        })
    }
}
