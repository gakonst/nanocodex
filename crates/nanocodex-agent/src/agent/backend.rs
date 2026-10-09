#[cfg(feature = "openai")]
use super::handle::request_fork;
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

/// Host persistence for one durable root's subagent task-tree journal.
///
/// Durability adapters supply this beside the root's execution state. Values
/// are opaque, Rust-owned JSON; `save` atomically replaces the previous value.
pub trait ChildJournalStore: Send + Sync + 'static {
    /// Loads the latest journal value.
    fn load(&self) -> BackendFuture<std::io::Result<Option<String>>>;
    /// Atomically replaces the journal value.
    fn save(&self, payload: String) -> BackendFuture<std::io::Result<()>>;
    /// Records a child's latest committed checkpoint as that child's own
    /// durable session (catalog entry plus resumable state), keyed by its
    /// distinct session ID and carrying its `Origin::Subagent` lineage.
    ///
    /// Called after the journal containing the same boundary was saved.
    /// Hosts without a session catalog keep only the journal.
    fn record_child(&self, _checkpoint: SessionCheckpoint) -> BackendFuture<std::io::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

/// A durable root's task-tree journal, exposed on its [`AgentHandle`].
///
/// Any harness whose builder attaches durability exposes this on its root
/// handle, so a subagent registry makes that root's children durable without
/// host wiring. The first session to claim it owns it.
#[derive(Clone)]
pub struct ChildJournal {
    store: Arc<dyn ChildJournalStore>,
    owner: Arc<std::sync::OnceLock<Arc<str>>>,
}

impl ChildJournal {
    /// Wraps a host journal store.
    pub fn new(store: Arc<dyn ChildJournalStore>) -> Self {
        Self {
            store,
            owner: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Binds the journal to its root session; false for any other session.
    pub fn claim(&self, session_id: &str) -> bool {
        &**self.owner.get_or_init(|| Arc::from(session_id)) == session_id
    }

    /// The underlying host store.
    pub fn store(&self) -> Arc<dyn ChildJournalStore> {
        Arc::clone(&self.store)
    }
}

impl std::fmt::Debug for ChildJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildJournal").finish_non_exhaustive()
    }
}

/// Embedding-owned construction of clean native children.
///
/// Implementations retain provider recipes and approved host capabilities. The
/// supplied parent is weak and never extends the owning driver's lifetime.
pub trait AgentFactory: Send + Sync + 'static {
    /// Builds the selected native family without starting its first turn.
    fn spawn(
        &self,
        parent: AgentHandle,
        options: SpawnOptions,
        host_context: Option<Arc<str>>,
    ) -> BackendFuture<Result<(Nanocodex, AgentEvents)>>;

    /// Rejects construction after the native owning runtime begins shutdown.
    fn ensure_available(&self, parent: AgentHandle) -> BackendFuture<Result<()>> {
        let settings = self.settings(parent);
        Box::pin(async move { settings.await.map(|_| ()) })
    }

    /// Constructs an ordered clean batch and observes each materialized child.
    /// Native runtimes may override this to retain atomic admission and rollback.
    fn spawn_many(
        &self,
        parent: AgentHandle,
        count: usize,
        observer: Arc<dyn Fn(&str) + Send + Sync>,
        host_context: Option<Arc<str>>,
    ) -> BackendFuture<Result<Vec<(Nanocodex, AgentEvents)>>> {
        let pending = (0..count)
            .map(|_| self.spawn(parent.clone(), SpawnOptions::new(), host_context.clone()))
            .collect::<Vec<_>>();
        Box::pin(async move {
            let mut children: Vec<(Nanocodex, AgentEvents)> = Vec::with_capacity(count);
            for child in pending {
                match child.await {
                    Ok(child) => children.push(child),
                    Err(error) => {
                        for (child, _) in &children {
                            let _ = child.shutdown().await;
                        }
                        return Err(error);
                    }
                }
            }
            // All or nothing: observers see only a batch that started completely.
            for (child, _) in &children {
                observer(child.session_id());
            }
            Ok(children)
        })
    }

    /// Reads the owning runtime's current settings for model-boundary inheritance.
    fn settings(
        &self,
        parent: AgentHandle,
    ) -> BackendFuture<Result<(crate::HarnessModel, Thinking)>> {
        Box::pin(async move {
            let model = parent.harness_model();
            Ok((model, model.default_thinking()))
        })
    }

    /// Forks the parent's native conversation when this factory supports it.
    fn fork(
        &self,
        _parent: AgentHandle,
        _request: ForkRequest,
    ) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        unsupported("fork")
    }

    /// Rebinds approved host capabilities to a checkpoint of this factory's family,
    /// keeping the checkpoint's session identity.
    fn restore(
        &self,
        _parent: AgentHandle,
        _checkpoint: SessionCheckpoint,
        _host_context: Option<Arc<str>>,
    ) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        unsupported("restore")
    }
}

fn unsupported<T: 'static>(capability: &'static str) -> BackendFuture<Result<T>> {
    Box::pin(async move { Err(NanocodexError::UnsupportedCapability { capability }) })
}

/// Backend implementor contract behind the common `Nanocodex` lifecycle.
///
/// This trait erases only lifecycle control after a concrete driver has been
/// built. Provider Tower services remain concrete, driver-owned types.
#[doc(hidden)]
pub trait LifecycleBackend: Send + Sync + 'static {
    /// Immutable native agent-loop family.
    fn harness_family(&self) -> crate::HarnessFamily;

    /// Lifecycle operations this backend supports.
    fn capabilities(&self) -> Capabilities;

    /// Where this session is persisted, when it is.
    fn persistence(&self) -> Option<Persistence> {
        None
    }

    /// Admits one prompt and returns the complete accepted turn.
    fn submit(&self, prompt: BackendPrompt) -> BackendFuture<Result<BackendTurn>>;

    /// Atomically starts a turn or steers the active one.
    fn route(&self, prompt: BackendPrompt) -> BackendFuture<Result<BackendPromptRoute>>;

    /// Steers one exact active turn.
    fn steer(&self, key: BackendTurnKey, prompt: Prompt) -> BackendFuture<Result<()>>;

    /// Admits a steer with a caller-owned identity unique within this turn.
    fn steer_with_id(
        &self,
        _key: BackendTurnKey,
        _id: String,
        _prompt: Prompt,
    ) -> BackendFuture<Result<()>> {
        unsupported("identified_steering")
    }

    /// Withdraws the latest steer if it has not reached a model boundary.
    fn withdraw_steer(&self, _key: BackendTurnKey, _id: String) -> BackendFuture<Result<bool>> {
        unsupported("identified_steering")
    }

    /// Cancels one exact unfinished turn.
    fn cancel(&self, key: BackendTurnKey) -> BackendFuture<Result<()>>;

    /// Changes the model within this backend's family, subject to
    /// [`Capabilities::model`].
    fn set_harness_model(&self, model: crate::HarnessModel) -> BackendFuture<Result<()>>;

    /// Changes reasoning policy for later turns.
    fn set_thinking(&self, thinking: Thinking) -> BackendFuture<Result<()>>;

    /// Changes priority policy for later turns.
    fn set_fast_mode(&self, enabled: bool) -> BackendFuture<Result<()>>;

    /// Changes processing policy for later turns.
    ///
    /// Backends with only a priority switch reject Ultrafast unless they override this method.
    fn set_service_tier(&self, service_tier: ServiceTier) -> BackendFuture<Result<()>> {
        match service_tier {
            ServiceTier::Standard => self.set_fast_mode(false),
            ServiceTier::Priority | ServiceTier::Fast => self.set_fast_mode(true),
            ServiceTier::Ultrafast => unsupported("ultrafast_service_tier"),
        }
    }

    /// Compacts retained context.
    fn compact(&self) -> BackendFuture<Result<()>>;

    /// Appends adapter-owned developer context.
    fn append_developer_message(&self, text: String) -> BackendFuture<Result<AgentSessionContext>>;

    /// Reads the latest safe model-visible context.
    fn context(&self) -> BackendFuture<Result<AgentSessionContext>>;

    /// Captures the latest committed boundary without waiting for an active turn.
    fn checkpoint(&self) -> BackendFuture<Result<SessionCheckpoint>>;

    /// Starts a clean sibling lifecycle.
    fn spawn(&self, options: SpawnOptions) -> BackendFuture<Result<(Nanocodex, AgentEvents)>>;

    /// Forks from the requested boundary with the requested provenance.
    fn fork(&self, request: ForkRequest) -> BackendFuture<Result<(Nanocodex, AgentEvents)>>;

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
    lineage: Lineage,
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
                lineage: Lineage::root(session_id.as_ref()),
                agent_id,
                session_id,
                #[cfg(feature = "openai")]
                local_session_id: None,
                events,
            },
            stream,
        )
    }

    /// Records where this session came from; defaults to a fresh root.
    #[must_use]
    pub fn with_lineage(mut self, lineage: Lineage) -> Self {
        self.lineage = lineage;
        self
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
        let session = SessionInfo {
            session_id: self.session_id.to_string(),
            family: backend.harness_family(),
            lineage: self.lineage,
        };
        Nanocodex {
            backend: Arc::new(backend),
            events: self.events,
            next_turn: Arc::new(AtomicU64::new(1)),
            agent_id: self.agent_id,
            session: Arc::new(session),
            #[cfg(feature = "openai")]
            local_session_id: self.local_session_id,
        }
    }
}

/// Resolves a public fork point against this Codex conversation tree.
#[cfg(feature = "openai")]
pub(super) fn resolve_fork_point(
    point: crate::session::ForkPoint,
    conversation_id: &str,
) -> Result<ForkFrom> {
    match point {
        crate::session::ForkPoint::Latest => Ok(ForkFrom::Latest),
        crate::session::ForkPoint::Turn(completed) => {
            let boundary = completed
                .boundary()
                .ok_or(NanocodexError::ReplayedCheckpointUnavailable)?;
            let Some(committed) = boundary.downcast::<CommittedSession>() else {
                // Only another family's boundary needs materializing to be identified.
                if !boundary.is::<super::checkpoint::ReplayedBoundary>()
                    && let Ok(checkpoint) = boundary.checkpoint()
                {
                    checkpoint.require_family(crate::HarnessFamily::Codex)?;
                }
                return Err(NanocodexError::ReplayedCheckpointUnavailable);
            };
            if committed.lineage_id() != conversation_id {
                return Err(NanocodexError::CheckpointLineageMismatch);
            }
            Ok(ForkFrom::Live(committed))
        }
        crate::session::ForkPoint::Checkpoint(checkpoint) => {
            checkpoint.require_family(crate::HarnessFamily::Codex)?;
            if checkpoint.conversation_id() != conversation_id {
                return Err(NanocodexError::CheckpointLineageMismatch);
            }
            let conversation = ChildState::from_checkpoint(checkpoint)?
                .conversation
                .ok_or(NanocodexError::ForkBeforeCompletedTurn)?;
            Ok(ForkFrom::Snapshot(Box::new(conversation)))
        }
    }
}

#[cfg(feature = "openai")]
pub(super) struct LocalLifecycle {
    pub(super) child_handle: AgentHandle,
    pub(super) commands: mpsc::Sender<Command>,
    pub(super) execution: Execution,
    pub(super) shutdown: DriverShutdown,
    pub(super) checkpoints: Arc<CheckpointSource>,
}

/// Lifecycle operations supported by the local Codex driver.
#[cfg(feature = "openai")]
const CODEX_CAPABILITIES: Capabilities = {
    let mut capabilities = Capabilities::NONE;
    capabilities.checkpoint = true;
    capabilities.resume = true;
    capabilities.fork = true;
    capabilities.fork_at = true;
    capabilities.side_conversation = true;
    capabilities.spawn = true;
    capabilities.steering = true;
    capabilities.identified_steering = true;
    capabilities.compaction = true;
    capabilities.developer_messages = true;
    capabilities.context = true;
    capabilities.model = crate::session::Mutability::BeforeFirstPrompt;
    capabilities.thinking = crate::session::Mutability::Anytime;
    capabilities.service_tier = crate::session::Mutability::Anytime;
    capabilities.ultrafast_service_tier = true;
    capabilities
};

#[cfg(feature = "openai")]
impl LifecycleBackend for LocalLifecycle {
    fn harness_family(&self) -> crate::HarnessFamily {
        crate::HarnessFamily::Codex
    }

    fn capabilities(&self) -> Capabilities {
        CODEX_CAPABILITIES
    }

    fn persistence(&self) -> Option<Persistence> {
        let durable = self.execution.durable_state_id().map(Persistence::durable);
        #[cfg(not(target_family = "wasm"))]
        if let Some(rollout) = self.execution.info().cloned() {
            return Some(durable.unwrap_or_default().with_rollout(rollout));
        }
        durable
    }

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
                    service_tier: None,
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

    fn steer_with_id(
        &self,
        key: BackendTurnKey,
        id: String,
        prompt: Prompt,
    ) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::SteerWithId {
                key: TurnKey(key.0),
                id,
                prompt,
                result,
            })
            .await
        })
    }

    fn withdraw_steer(&self, key: BackendTurnKey, id: String) -> BackendFuture<Result<bool>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::WithdrawSteer {
                key: TurnKey(key.0),
                id,
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

    fn set_harness_model(&self, model: crate::HarnessModel) -> BackendFuture<Result<()>> {
        let crate::HarnessModel::Codex(model) = model else {
            return Box::pin(async {
                Err(NanocodexError::InvalidRequest(
                    "model belongs to another harness family".into(),
                ))
            });
        };
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
        self.set_service_tier(ServiceTier::from_fast_mode(enabled))
    }

    fn set_service_tier(&self, service_tier: ServiceTier) -> BackendFuture<Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::SetServiceTier {
                service_tier,
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
                Command::AppendDeveloperMessage { text, result }
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

    fn checkpoint(&self) -> BackendFuture<Result<SessionCheckpoint>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_command(&commands, &shutdown, |result| Command::ChildSnapshot {
                result,
            })
            .await?
            .into_checkpoint()
        })
    }

    fn spawn(&self, options: SpawnOptions) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let parent = self.child_handle.clone();
        Box::pin(async move { parent.spawn_with(options).await })
    }

    fn fork(&self, request: ForkRequest) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        let checkpoints = Arc::clone(&self.checkpoints);
        let execution = self.execution.clone();
        Box::pin(async move {
            let (point, origin) = request.into_parts();
            let point = resolve_fork_point(point, checkpoints.conversation_id())?;
            let session_id = SessionId::new();
            let policy = execution.branch_policy(&checkpoints.child_info(&session_id, origin))?;
            request_fork(&commands, &shutdown, point, origin, session_id, policy).await
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
