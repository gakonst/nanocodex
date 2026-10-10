use super::backend::{
    BackendPrompt, BackendPromptRoute, BackendTurn, BackendTurnKey, LifecycleBackend,
};
use super::*;

/// Cheap, cloneable command handle for an owned agent driver.
pub struct Nanocodex {
    pub(super) backend: Arc<dyn LifecycleBackend>,
    pub(super) events: nanocodex_oai_api::events::AgentEventPublisher,
    pub(super) next_turn: Arc<AtomicU64>,
    pub(super) agent_id: Arc<str>,
    pub(super) session: Arc<SessionInfo>,
    /// Typed identity of a local OpenAI driver, which owns turn identities.
    #[cfg(feature = "openai")]
    pub(super) local_session_id: Option<SessionId>,
}

impl Clone for Nanocodex {
    fn clone(&self) -> Self {
        Self {
            backend: Arc::clone(&self.backend),
            events: self.events.clone(),
            next_turn: Arc::clone(&self.next_turn),
            agent_id: Arc::clone(&self.agent_id),
            session: Arc::clone(&self.session),
            #[cfg(feature = "openai")]
            local_session_id: self.local_session_id,
        }
    }
}

/// Cheap weak capability for constructing children from one owning runtime.
/// Holding this handle never keeps its parent driver alive.
#[derive(Clone)]
pub struct AgentHandle {
    pub(super) session_id: Arc<str>,
    root_session_id: Arc<str>,
    pub(super) model: crate::HarnessModel,
    native_model_id: Arc<str>,
    pub(super) native: Arc<dyn super::backend::AgentFactory>,
    pub(super) factory: Option<Arc<dyn super::backend::AgentFactory>>,
    child_journal: Option<super::backend::ChildJournal>,
}

impl AgentHandle {
    /// Creates a weak capability backed by a native factory. The factory must
    /// reject operations after its owning driver stops and retain it only weakly.
    pub fn new(
        session_id: impl Into<Arc<str>>,
        model: crate::HarnessModel,
        native: Arc<dyn super::backend::AgentFactory>,
    ) -> Self {
        let session_id: Arc<str> = session_id.into();
        Self {
            root_session_id: Arc::clone(&session_id),
            session_id,
            model,
            native_model_id: Arc::from(model.as_str()),
            native,
            factory: None,
            child_journal: None,
        }
    }

    /// Exposes a durable root's subagent journal on this capability.
    #[must_use]
    pub fn with_child_journal(mut self, journal: Option<super::backend::ChildJournal>) -> Self {
        self.child_journal = journal;
        self
    }

    /// The durable subagent journal of this session, when it is a durable root.
    pub fn child_journal(&self) -> Option<&super::backend::ChildJournal> {
        self.child_journal
            .as_ref()
            .filter(|journal| journal.claim(&self.session_id))
    }

    /// Installs embedding-owned mixed-family construction for this capability.
    #[must_use]
    pub fn with_spawn_factory(mut self, factory: Arc<dyn super::backend::AgentFactory>) -> Self {
        self.factory = Some(factory);
        self
    }

    /// Returns the owning session's stable identity.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Records the root of the owning session's conversation tree. Defaults
    /// to the owning session itself, which is correct only for roots.
    #[must_use]
    pub fn with_root_session_id(mut self, root_session_id: impl Into<Arc<str>>) -> Self {
        self.root_session_id = root_session_id.into();
        self
    }

    /// Returns the root session of the owning session's conversation tree.
    pub fn root_session_id(&self) -> &str {
        &self.root_session_id
    }

    /// Identity exported to every subprocess launched by tools this
    /// capability constructs (`CODEX_THREAD_ID` and
    /// `NANOCODEX_ROOT_SESSION_ID`).
    #[must_use]
    pub fn session_environment(&self) -> nanocodex_home::SessionEnvironment {
        nanocodex_home::SessionEnvironment::new(&self.session_id, &self.root_session_id)
    }

    /// Retains an unrestricted native identifier for concrete backend recipes.
    #[must_use]
    pub fn with_native_model_id(mut self, model: impl Into<Arc<str>>) -> Self {
        self.native_model_id = model.into();
        self
    }
    /// Returns the exact provider-native identifier attached to this capability.
    pub fn native_model_id(&self) -> &str {
        &self.native_model_id
    }
    /// Returns a shared selector only when the native identifier belongs to its catalog.
    pub fn catalog_model(&self) -> Option<crate::HarnessModel> {
        self.native_model_id.parse().ok()
    }
    /// Returns the attached shared selector. For unrestricted native models this
    /// is the family default; use `catalog_model` or `native_model_id` to inspect
    /// the exact recipe, and `settings` for current validated catalog settings.
    pub const fn harness_model(&self) -> crate::HarnessModel {
        self.model
    }
    /// Reads current native model and reasoning defaults without extending parent ownership.
    pub async fn settings(&self) -> Result<(crate::HarnessModel, Thinking)> {
        self.native.settings(self.clone()).await
    }
    /// Returns the owning agent-loop family.
    pub const fn harness_family(&self) -> crate::HarnessFamily {
        self.model.family()
    }

    /// Checks the weak owning runtime independently of its model catalog.
    pub async fn ensure_available(&self) -> Result<()> {
        self.native.ensure_available(self.clone()).await
    }

    /// Starts a clean child using inherited family settings.
    pub async fn spawn(&self) -> Result<(Nanocodex, AgentEvents)> {
        self.spawn_with(SpawnOptions::new()).await
    }
    /// Starts a clean child with family-scoped overrides.
    pub async fn spawn_with(&self, options: SpawnOptions) -> Result<(Nanocodex, AgentEvents)> {
        self.spawn_with_host_context(options, None).await
    }
    /// Starts a child while retaining embedding-private invocation context.
    pub async fn spawn_with_host_context(
        &self,
        options: SpawnOptions,
        host_context: Option<Arc<str>>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        options.validate_harness()?;
        self.native.ensure_available(self.clone()).await?;
        created(
            self.factory
                .as_ref()
                .unwrap_or(&self.native)
                .spawn(self.clone(), options, host_context)
                .await,
        )
        .await
    }
    /// Invokes the parent's native factory directly, bypassing mixed routing.
    pub async fn spawn_native_with_host_context(
        &self,
        options: SpawnOptions,
        host_context: Option<Arc<str>>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        options.validate_harness()?;
        created(self.native.spawn(self.clone(), options, host_context).await).await
    }
    /// Restores an evicted child from its checkpoint without changing its identity.
    ///
    /// A checkpoint of this capability's own family uses the native factory;
    /// any other family requires a configured spawn factory.
    ///
    /// # Errors
    /// Returns [`NanocodexError::UnsupportedCapability`] when no factory can
    /// restore the checkpoint's family, or the factory's restore failure.
    pub async fn restore_runtime(
        &self,
        checkpoint: SessionCheckpoint,
        host_context: Option<Arc<str>>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        self.native.ensure_available(self.clone()).await?;
        let restored = if checkpoint.family() == self.harness_family() {
            self.native
                .restore(self.clone(), checkpoint, host_context)
                .await
        } else {
            self.factory
                .as_ref()
                .ok_or(NanocodexError::UnsupportedCapability {
                    capability: "cross_family_restore",
                })?
                .restore(self.clone(), checkpoint, host_context)
                .await
        };
        created(restored).await
    }
    /// Restores through the native factory, bypassing mixed routing.
    ///
    /// # Errors
    /// Returns the native factory's restore failure, including a family mismatch.
    pub async fn restore_native_runtime(
        &self,
        checkpoint: SessionCheckpoint,
        host_context: Option<Arc<str>>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        created(
            self.native
                .restore(self.clone(), checkpoint, host_context)
                .await,
        )
        .await
    }
    /// Starts an ordered batch, closing created children if construction fails.
    pub async fn spawn_many(&self, count: usize) -> Result<Vec<(Nanocodex, AgentEvents)>> {
        self.spawn_many_observed_with_host_context(count, |_| {}, None)
            .await
    }
    /// Starts an ordered batch and observes materialized session identities.
    pub async fn spawn_many_observed(
        &self,
        count: usize,
        observer: impl Fn(&str) + Send + Sync + 'static,
    ) -> Result<Vec<(Nanocodex, AgentEvents)>> {
        self.spawn_many_observed_with_host_context(count, observer, None)
            .await
    }
    /// Starts an observed ordered batch retaining private host context.
    pub async fn spawn_many_observed_with_host_context(
        &self,
        count: usize,
        observer: impl Fn(&str) + Send + Sync + 'static,
        host_context: Option<Arc<str>>,
    ) -> Result<Vec<(Nanocodex, AgentEvents)>> {
        self.native.ensure_available(self.clone()).await?;
        // A configured factory routes batches with the same family resolution
        // as single spawns; it reaches the native atomic batch through
        // `Self::spawn_many_native_with_host_context`.
        created_batch(
            self.factory
                .as_ref()
                .unwrap_or(&self.native)
                .spawn_many(self.clone(), count, Arc::new(observer), host_context)
                .await,
        )
        .await
    }
    /// Invokes the parent's native batch directly, bypassing mixed routing and
    /// preserving Responses atomic admission and cancellation cleanup.
    pub async fn spawn_many_native_with_host_context(
        &self,
        count: usize,
        observer: impl Fn(&str) + Send + Sync + 'static,
        host_context: Option<Arc<str>>,
    ) -> Result<Vec<(Nanocodex, AgentEvents)>> {
        self.native.ensure_available(self.clone()).await?;
        created_batch(
            self.native
                .spawn_many(self.clone(), count, Arc::new(observer), host_context)
                .await,
        )
        .await
    }
    /// Forks the owning conversation. Forking is deliberately a native
    /// lifecycle operation rather than mixed routing.
    ///
    /// # Errors
    /// Returns an error after the owning runtime stops, before its first safe
    /// boundary, or when the request's boundary belongs to another conversation.
    pub async fn fork(&self, request: ForkRequest) -> Result<(Nanocodex, AgentEvents)> {
        self.native.ensure_available(self.clone()).await?;
        created(self.native.fork(self.clone(), request).await).await
    }
}

/// Completes one child's creation: a durable child's first checkpoint is
/// persisted before the caller receives it, so it is already listed and
/// resumable. A child that cannot persist is retracted and stopped.
async fn created(child: Result<(Nanocodex, AgentEvents)>) -> Result<(Nanocodex, AgentEvents)> {
    let child = child?;
    if let Err(error) = child.0.backend.persist_initial().await {
        child.0.abandon_created().await;
        return Err(error);
    }
    Ok(child)
}

/// Completes an atomic batch: every child persists, or every child is
/// retracted and stopped so none remains listed.
async fn created_batch(
    children: Result<Vec<(Nanocodex, AgentEvents)>>,
) -> Result<Vec<(Nanocodex, AgentEvents)>> {
    let children = children?;
    let mut failure = None;
    for (child, _) in &children {
        if let Err(error) = child.backend.persist_initial().await {
            failure.get_or_insert(error);
        }
    }
    if let Some(error) = failure {
        for (child, _) in &children {
            child.abandon_created().await;
        }
        return Err(error);
    }
    Ok(children)
}

#[cfg(feature = "openai")]
pub(super) struct OpenAiAgentFactory {
    pub(super) commands: mpsc::WeakSender<Command>,
    pub(super) shutdown: DriverShutdown,
    pub(super) conversation_id: Arc<str>,
}

#[cfg(feature = "openai")]
impl super::backend::AgentFactory for OpenAiAgentFactory {
    fn ensure_available(&self, _parent: AgentHandle) -> super::backend::BackendFuture<Result<()>> {
        let available = self.shutdown.is_running()
            && self
                .commands
                .upgrade()
                .is_some_and(|commands| !commands.is_closed());
        Box::pin(async move {
            if available {
                Ok(())
            } else {
                Err(NanocodexError::AgentStopped)
            }
        })
    }
    fn spawn(
        &self,
        _parent: AgentHandle,
        options: SpawnOptions,
        host_context: Option<Arc<str>>,
    ) -> super::backend::BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let commands = self.commands.upgrade();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            options.validate_harness()?;
            if options
                .selected_harness()
                .is_some_and(|family| family != crate::HarnessFamily::Codex)
                || options
                    .selected_harness_model()
                    .is_some_and(|model| model.family() != crate::HarnessFamily::Codex)
            {
                return Err(NanocodexError::InvalidRequest(
                    "Claude harness requires a configured child factory".into(),
                ));
            }
            let commands = commands.ok_or(NanocodexError::AgentStopped)?;
            created(
                request_spawn_with_host_context(&commands, &shutdown, options, host_context).await,
            )
            .await
        })
    }
    fn spawn_many(
        &self,
        _parent: AgentHandle,
        count: usize,
        observer: Arc<dyn Fn(&str) + Send + Sync>,
        host_context: Option<Arc<str>>,
    ) -> super::backend::BackendFuture<Result<Vec<(Nanocodex, AgentEvents)>>> {
        let commands = self.commands.upgrade();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            let commands = commands.ok_or(NanocodexError::AgentStopped)?;
            created_batch(
                request_spawn_many(&commands, &shutdown, count, Some(observer), host_context).await,
            )
            .await
        })
    }
    fn settings(
        &self,
        _parent: AgentHandle,
    ) -> super::backend::BackendFuture<Result<(crate::HarnessModel, Thinking)>> {
        let commands = self.commands.upgrade();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            let commands = commands.ok_or(NanocodexError::AgentStopped)?;
            let snapshot = request_command(&commands, &shutdown, |result| Command::ChildSnapshot {
                result,
            })
            .await?;
            Ok((
                crate::HarnessModel::Codex(snapshot.model),
                snapshot.thinking,
            ))
        })
    }
    fn restore(
        &self,
        _parent: AgentHandle,
        checkpoint: SessionCheckpoint,
        host_context: Option<Arc<str>>,
    ) -> super::backend::BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let commands = self.commands.upgrade();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            let snapshot = ChildState::from_checkpoint(checkpoint)?;
            let commands = commands.ok_or(NanocodexError::AgentStopped)?;
            let restored = request_command(&commands, &shutdown, |result| Command::Spawn {
                options: SpawnOptions::new()
                    .model(snapshot.model)
                    .thinking(snapshot.thinking),
                restore: Some(snapshot),
                host_context,
                result,
            })
            .await;
            created(restored).await
        })
    }
    fn fork(
        &self,
        _parent: AgentHandle,
        request: ForkRequest,
    ) -> super::backend::BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let commands = self.commands.upgrade();
        let shutdown = self.shutdown.clone();
        let conversation_id = Arc::clone(&self.conversation_id);
        Box::pin(async move {
            let commands = commands.ok_or(NanocodexError::AgentStopped)?;
            let (point, origin) = request.into_parts();
            let point = super::backend::resolve_fork_point(point, &conversation_id)?;
            created(request_fork(&commands, &shutdown, point, origin, SessionId::new(), None).await)
                .await
        })
    }
}

impl Nanocodex {
    /// Starts configuring an agent from a concrete backend input.
    #[must_use]
    pub fn builder<B>(backend: B) -> B::Builder
    where
        B: BuilderBackend,
    {
        backend.into_builder()
    }

    /// Returns the immutable native agent-loop family.
    #[must_use]
    pub fn harness_family(&self) -> crate::HarnessFamily {
        self.session.family
    }

    /// Returns this session's identity, family, and lineage.
    #[must_use]
    pub fn session(&self) -> &SessionInfo {
        &self.session
    }

    /// Returns the lifecycle operations this session's backend supports.
    ///
    /// Unsupported operations fail with [`NanocodexError::UnsupportedCapability`].
    #[must_use]
    pub fn capabilities(&self) -> Capabilities {
        self.backend.capabilities()
    }

    /// Returns where this session is persisted, or `None` when it lives only
    /// in memory.
    #[must_use]
    pub fn persistence(&self) -> Option<Persistence> {
        self.backend.persistence()
    }

    /// Changes the selected model within this backend's family.
    ///
    /// [`Capabilities::model`] states when the model may change; Codex accepts
    /// a change only before the first prompt.
    ///
    /// # Errors
    ///
    /// Returns an error for a model of another family, after conversation
    /// activity begins when the backend locks its model, when the model is
    /// incompatible with the current thinking level, or if the backend stopped.
    pub async fn set_harness_model(&self, model: crate::HarnessModel) -> Result<()> {
        if model.family() != self.harness_family() {
            return Err(NanocodexError::InvalidRequest(
                "model belongs to another harness family".into(),
            ));
        }
        self.backend.set_harness_model(model).await
    }

    /// Returns the stable agent identity used to reopen durable backends.
    #[must_use]
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Returns the stable identity used by events, transport metadata, and any rollout.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session.session_id
    }

    /// Retries any pending backend-owned persistence write and waits for it to
    /// become durable.
    ///
    /// This is a no-op when the session is not persisted. CLI consumers call
    /// it at completed turn boundaries so persistence failures are user-visible.
    /// Flushing does not stop the session; call [`Self::shutdown`] at an
    /// explicit application or session boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured persistence cannot be written.
    pub async fn flush(&self) -> Result<()> {
        self.backend.flush().await
    }

    /// Disconnects this client while allowing backend-owned durable work to continue.
    ///
    /// A durable remote backend closes client-local streams and attachments
    /// without cancelling accepted turns. Backends that cannot continue after
    /// disconnection perform an ordinary shutdown instead.
    ///
    /// # Errors
    ///
    /// Returns the backend's local resource cleanup failure.
    pub async fn disconnect(&self) -> Result<()> {
        self.backend.disconnect().await
    }

    /// Gracefully stops this agent and waits for all owned resources to close.
    ///
    /// Shutdown globally invalidates this handle and every clone. It cancels an
    /// active turn, terminalizes all other accepted turns in FIFO order, waits
    /// for model and tool cleanup, and flushes and closes the rollout writer. A
    /// returned `Ok(())` therefore establishes a durable boundary suitable for
    /// an immediate same-process rollout resume.
    ///
    /// Dropping the final handle performs backend-owned implicit cleanup but
    /// offers no future that can join it. Local backends cancel unfinished
    /// work; durable remote backends may instead disconnect and leave accepted
    /// turns running. Use this method when cancellation is the explicit intent.
    ///
    /// # Errors
    ///
    /// Returns the shared cleanup result. The first caller initiates shutdown;
    /// concurrent and later callers on any clone await or reuse that same
    /// result.
    pub async fn shutdown(&self) -> Result<()> {
        self.backend.shutdown().await
    }

    /// Accepts a prompt submission and immediately returns its turn handle.
    ///
    /// When an execution policy is configured, strings and [`Prompt`] values
    /// receive an automatically generated operation identity. Use
    /// [`PromptRequest::request_id`] to supply a stable caller-owned identity.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty prompt or request ID, when identified
    /// work is submitted without a configured policy, or if the driver stopped.
    pub async fn prompt(&self, request: impl Into<PromptRequest>) -> Result<Turn> {
        let PromptRequest {
            prompt,
            request_id,
            cancel_on_admission,
        } = request.into();
        prompt
            .validate()
            .map_err(|error| NanocodexError::InvalidRequest(error.to_string()))?;
        if request_id
            .as_deref()
            .is_some_and(|request_id| request_id.trim().is_empty())
        {
            return Err(NanocodexError::InvalidRequest(
                "request ID must not be empty".to_owned(),
            ));
        }
        let key = BackendTurnKey(self.next_turn.fetch_add(1, Ordering::Relaxed));
        let (events, event_stream) = self.events.mirrored_channel();
        #[cfg(feature = "openai")]
        let turn_id = uuid::Uuid::now_v7().to_string();
        #[cfg(not(feature = "openai"))]
        let turn_id = format!("{}:{}", self.session_id(), key.0);
        let events = events.with_turn_id(turn_id.clone());
        let BackendTurn { request_id, result } = self
            .backend
            .submit(BackendPrompt {
                key,
                prompt,
                request_id,
                cancel_on_admission,
                events,
            })
            .await?;
        let turn_id = self.canonical_turn_id(turn_id, request_id.as_deref());
        Ok(Turn {
            result: Self::turn_result(&turn_id, result),
            turn_id,
            control: TurnControl {
                key,
                backend: Arc::clone(&self.backend),
            },
            request_id,
            events: event_stream,
        })
    }

    fn turn_result(
        turn_id: &str,
        result: super::backend::BackendFuture<Result<TurnResult>>,
    ) -> super::backend::BackendFuture<Result<TurnResult>> {
        let turn_id = turn_id.to_owned();
        Box::pin(async move { result.await.map(|result| result.with_turn_id(turn_id)) })
    }

    fn canonical_turn_id(&self, generated: String, request_id: Option<&str>) -> String {
        #[cfg(feature = "openai")]
        if self.local_session_id.is_some() {
            return generated;
        }
        request_id.map(str::to_owned).unwrap_or(generated)
    }

    /// Routes live input into the active turn or starts a new turn when idle.
    ///
    /// The driver makes the decision atomically. If a regular turn is active,
    /// the prompt is appended to its bounded steering queue and
    /// [`PromptRoute::Steered`] is returned. Otherwise the prompt starts a new
    /// turn and is returned as [`PromptRoute::Started`].
    ///
    /// This is intended for live input adapters. Normal queued request/response
    /// consumers should continue to use [`Self::prompt`].
    ///
    /// # Errors
    ///
    /// Returns an error for an empty prompt, a full steering queue, or if the
    /// agent driver stopped.
    pub async fn route_prompt(&self, prompt: impl Into<Prompt>) -> Result<PromptRoute> {
        let prompt = prompt.into();
        prompt
            .validate()
            .map_err(|error| NanocodexError::InvalidRequest(error.to_string()))?;
        let key = BackendTurnKey(self.next_turn.fetch_add(1, Ordering::Relaxed));
        let (events, event_stream) = self.events.mirrored_channel();
        #[cfg(feature = "openai")]
        let turn_id = uuid::Uuid::now_v7().to_string();
        #[cfg(not(feature = "openai"))]
        let turn_id = format!("{}:{}", self.session_id(), key.0);
        let events = events.with_turn_id(turn_id.clone());
        match self
            .backend
            .route(BackendPrompt {
                key,
                prompt,
                request_id: None,
                cancel_on_admission: false,
                events,
            })
            .await
        {
            Ok(BackendPromptRoute::Started(BackendTurn { request_id, result })) => {
                let turn_id = self.canonical_turn_id(turn_id, request_id.as_deref());
                Ok(PromptRoute::Started(Turn {
                    result: Self::turn_result(&turn_id, result),
                    turn_id,
                    control: TurnControl {
                        key,
                        backend: Arc::clone(&self.backend),
                    },
                    request_id,
                    events: event_stream,
                }))
            }
            Ok(BackendPromptRoute::Steered) => Ok(PromptRoute::Steered),
            Err(error) => Err(error),
        }
    }

    /// Changes the reasoning effort for subsequently accepted turns.
    ///
    /// An active turn and prompts already queued by the driver retain the
    /// effort they captured when accepted.
    ///
    /// # Errors
    ///
    /// Returns an error if the agent driver has stopped.
    pub async fn set_thinking(&self, thinking: Thinking) -> Result<()> {
        self.backend.set_thinking(thinking).await
    }

    /// Enables or disables priority processing for subsequently accepted turns.
    ///
    /// An active turn and prompts already queued by the driver retain the mode
    /// they captured when accepted.
    ///
    /// # Errors
    ///
    /// Returns an error if the agent driver has stopped.
    pub async fn set_fast_mode(&self, enabled: bool) -> Result<()> {
        self.set_service_tier(ServiceTier::from_fast_mode(enabled))
            .await
    }

    /// Selects the processing tier for subsequently accepted turns.
    ///
    /// Active and already accepted queued turns retain their captured tier.
    /// Native OpenAI requests clamp to the fastest tier supported by the model.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend has stopped or does not support the tier.
    pub async fn set_service_tier(&self, service_tier: ServiceTier) -> Result<()> {
        self.backend.set_service_tier(service_tier).await
    }

    /// Immediately compacts this agent's retained conversation.
    ///
    /// Compaction preserves the agent's cache identity, tools, transport, and
    /// cached project instructions. The next prompt receives a full developer,
    /// `AGENTS.md`, and environment-context reinjection before its user input.
    /// If a turn is active, that turn is cancelled and compaction runs before
    /// prompts that were queued behind it.
    ///
    /// ```
    /// # use nanocodex_agent::{Nanocodex, Result};
    /// # async fn compact_after_a_turn(agent: &Nanocodex) -> Result<()> {
    /// agent
    ///     .prompt("Inspect the parser and explain the failing test.")
    ///     .await?
    ///     .result()
    ///     .await?;
    /// agent.compact().await?;
    /// let result = agent
    ///     .prompt("Now implement the smallest correct parser fix.")
    ///     .await?
    ///     .result()
    ///     .await?;
    /// assert!(!result.final_message().is_empty());
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a model or driver-stopped error. Persistence writes follow the
    /// same retry-on-[`Self::flush`] contract as prompt turns.
    pub async fn compact(&self) -> Result<()> {
        self.backend.compact().await
    }

    /// Appends adapter-owned developer context at the next safe model boundary.
    ///
    /// The returned read-only view is captured from the latest safe boundary
    /// and is suitable for building adapter bootstrap context. If a turn is
    /// active, the message is retained immediately and becomes model-visible
    /// before the next turn.
    ///
    /// # Errors
    ///
    /// Returns an error for empty text or after the agent driver has stopped.
    pub async fn append_developer_message(
        &self,
        text: impl Into<String>,
    ) -> Result<AgentSessionContext> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(NanocodexError::InvalidRequest(
                "developer message must not be empty".to_owned(),
            ));
        }
        self.backend.append_developer_message(text).await
    }

    /// Returns complete model-visible context at the latest safe boundary.
    ///
    /// This is a read-only adapter view. The history can contain unredacted
    /// prompts, responses, reasoning, and tool activity and must be protected
    /// like a session snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error after the agent driver has stopped.
    pub async fn context(&self) -> Result<AgentSessionContext> {
        self.backend.context().await
    }

    /// Captures this session's identity, settings, and latest committed
    /// boundary as a portable checkpoint without changing this agent.
    ///
    /// This never waits for an active turn: it copies the latest committed
    /// safe boundary. Before the first boundary the checkpoint carries no
    /// conversation ([`SessionCheckpoint::has_conversation`] is false) but can
    /// still restore the session's identity and settings. The caller owns the
    /// unredacted model-visible history it contains.
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::UnsupportedCapability`] when the backend cannot
    /// checkpoint, or an error after the backend stops.
    pub async fn checkpoint(&self) -> Result<SessionCheckpoint> {
        self.backend.checkpoint().await
    }

    /// Starts a clean sibling agent with the same private configuration,
    /// workspace policy, service factory, and tools factory.
    ///
    /// The sibling receives a new session, cache lineage, conversation,
    /// WebSocket, and tool runtime. It does not inherit conversation history.
    ///
    /// # Errors
    ///
    /// Returns an error after this agent's driver has stopped.
    pub async fn spawn(&self) -> Result<(Self, AgentEvents)> {
        self.spawn_with(SpawnOptions::new()).await
    }

    /// Starts a clean sibling with optional model and reasoning overrides.
    ///
    /// Unspecified values inherit this agent's settings when its driver handles
    /// the spawn command. Overrides affect only the new sibling.
    ///
    /// # Errors
    ///
    /// Returns an error after this agent's driver has stopped.
    pub async fn spawn_with(&self, options: SpawnOptions) -> Result<(Self, AgentEvents)> {
        self.backend.spawn(options).await
    }

    /// Forks this conversation into an independently driven agent.
    ///
    /// [`ForkRequest::latest`] forks from the latest safe boundary without
    /// waiting for an active turn; [`ForkRequest::at_turn`] forks from the
    /// boundary a completed turn retained; [`ForkRequest::at`] forks from a
    /// portable checkpoint of this conversation tree. Mark side questions
    /// with [`ForkRequest::side_conversation`]; they stay durable and listable
    /// with their origin. The child's
    /// [`SessionInfo::lineage`] records this session as its parent.
    ///
    /// The child receives a fresh transport and tool runtime while sharing the
    /// immutable transcript and prompt-cache lineage. Partial model output and
    /// unmatched tool calls are excluded.
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::ForkBeforeCompletedTurn`] before the first
    /// safe boundary, [`NanocodexError::CheckpointLineageMismatch`] for a
    /// boundary of another conversation, [`NanocodexError::CheckpointFamilyMismatch`]
    /// for another family's checkpoint,
    /// [`NanocodexError::ReplayedCheckpointUnavailable`] for a turn without a
    /// live boundary, or an error when the backend has stopped.
    pub async fn fork(&self, request: ForkRequest) -> Result<(Self, AgentEvents)> {
        created(self.backend.fork(request).await).await
    }

    /// Completes a just-created child's creation: a durable child's first
    /// checkpoint is persisted before it is returned, so every factory hands
    /// out children that are already listed and resumable. Idempotent.
    #[doc(hidden)]
    pub async fn persist_created(
        child: Result<(Self, AgentEvents)>,
    ) -> Result<(Self, AgentEvents)> {
        created(child).await
    }

    /// Retracts and stops a just-created child whose creation the caller
    /// abandoned, such as a member of a failed atomic batch, so it is not left
    /// listed. A restored child keeps the history it held before.
    #[doc(hidden)]
    pub async fn abandon_created(&self) {
        // Settle a pending first checkpoint so it cannot land after retraction.
        let _ = self.backend.persist_initial().await;
        let _ = self.backend.discard_initial().await;
        let _ = self.shutdown().await;
    }
}

#[cfg(feature = "openai")]
pub(super) async fn request_fork(
    commands: &mpsc::Sender<Command>,
    shutdown: &DriverShutdown,
    point: ForkFrom,
    origin: crate::Origin,
    session_id: SessionId,
    policy: Option<Arc<dyn execution::ExecutionPolicy>>,
) -> Result<(Nanocodex, AgentEvents)> {
    if !matches!(
        origin,
        crate::Origin::Fork | crate::Origin::SideConversation
    ) {
        return Err(NanocodexError::InvalidRequest(
            "a fork must be a fork or a side conversation".into(),
        ));
    }
    request_command(commands, shutdown, |result| Command::Fork {
        origin,
        point,
        session_id,
        policy,
        result,
    })
    .await
}

#[cfg(feature = "openai")]
async fn request_spawn_with_host_context(
    commands: &mpsc::Sender<Command>,
    shutdown: &DriverShutdown,
    options: SpawnOptions,
    host_context: Option<Arc<str>>,
) -> Result<(Nanocodex, AgentEvents)> {
    request_command(commands, shutdown, |result| Command::Spawn {
        restore: None,
        options,
        host_context,
        result,
    })
    .await
}

#[cfg(feature = "openai")]
async fn request_spawn_many(
    commands: &mpsc::Sender<Command>,
    shutdown: &DriverShutdown,
    count: usize,
    observer: Option<Arc<SpawnObserver>>,
    host_context: Option<Arc<str>>,
) -> Result<Vec<(Nanocodex, AgentEvents)>> {
    request_command(commands, shutdown, |result| Command::SpawnBatch {
        count,
        observer,
        host_context,
        result,
    })
    .await
}

#[cfg(feature = "openai")]
pub(super) async fn request_command<T>(
    commands: &mpsc::Sender<Command>,
    shutdown: &DriverShutdown,
    command: impl FnOnce(oneshot::Sender<Result<T>>) -> Command,
) -> Result<T> {
    let (result, receiver) = oneshot::channel();
    if commands.send(command(result)).await.is_err() {
        return Err(shutdown.stopped_error().await);
    }
    match receiver.await {
        Ok(Err(NanocodexError::AgentStopped)) | Err(_) => Err(shutdown.stopped_error().await),
        Ok(outcome) => outcome,
    }
}
