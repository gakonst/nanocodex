use super::backend::{
    BackendPrompt, BackendPromptRoute, BackendTurn, BackendTurnKey, LifecycleBackend,
};
use super::*;

#[cfg(all(feature = "openai", not(target_family = "wasm")))]
use crate::rollout::RolloutInfo;

/// Cheap, cloneable command handle for an owned agent driver.
pub struct Nanocodex {
    // Only caller-facing handles own this lease. Keeping it out of the backend
    // and factory capabilities prevents a registry -> harness -> driver cycle.
    pub(super) caller_ownership: Option<Arc<dyn Send + Sync>>,
    pub(super) backend: Arc<dyn LifecycleBackend>,
    pub(super) events: nanocodex_oai_api::events::AgentEventPublisher,
    pub(super) next_turn: Arc<AtomicU64>,
    pub(super) agent_id: Arc<str>,
    pub(super) session_id: Arc<str>,
    #[cfg(not(target_family = "wasm"))]
    pub(super) startup: Option<Arc<OwnedStartup>>,
    #[cfg(feature = "openai")]
    pub(super) local_session_id: Option<SessionId>,
    #[cfg(all(feature = "openai", not(target_family = "wasm")))]
    pub(super) rollout: Option<RolloutInfo>,
}

impl Clone for Nanocodex {
    fn clone(&self) -> Self {
        Self {
            caller_ownership: self.caller_ownership.clone(),
            backend: Arc::clone(&self.backend),
            events: self.events.clone(),
            next_turn: Arc::clone(&self.next_turn),
            agent_id: Arc::clone(&self.agent_id),
            session_id: Arc::clone(&self.session_id),
            #[cfg(not(target_family = "wasm"))]
            startup: self.startup.clone(),
            #[cfg(feature = "openai")]
            local_session_id: self.local_session_id,
            #[cfg(all(feature = "openai", not(target_family = "wasm")))]
            rollout: self.rollout.clone(),
        }
    }
}

#[cfg(not(target_family = "wasm"))]
pub(super) struct OwnedStartup {
    result: tokio::sync::watch::Receiver<Option<std::result::Result<(), String>>>,
    task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[cfg(not(target_family = "wasm"))]
impl Drop for OwnedStartup {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().expect("startup task lock").take() {
            task.abort();
        }
    }
}

/// Cheap weak capability for constructing children from one owning runtime.
/// Holding this handle never keeps its parent driver alive.
#[derive(Clone)]
pub struct AgentHandle {
    pub(super) session_id: Arc<str>,
    pub(super) model: crate::HarnessModel,
    native_model_id: Arc<str>,
    pub(super) native: Arc<dyn super::backend::AgentFactory>,
    pub(super) factory: Option<Arc<dyn super::backend::AgentFactory>>,
}

impl AgentHandle {
    /// Creates a weak capability backed by a native factory. The factory must
    /// reject operations after its owning driver stops and retain it only weakly.
    pub fn new(
        session_id: impl Into<Arc<str>>,
        model: crate::HarnessModel,
        native: Arc<dyn super::backend::AgentFactory>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            model,
            native_model_id: Arc::from(model.as_str()),
            native,
            factory: None,
        }
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
        self.factory
            .as_ref()
            .unwrap_or(&self.native)
            .spawn(self.clone(), options, host_context)
            .await
    }
    /// Invokes the parent's native factory directly, bypassing mixed routing.
    pub async fn spawn_native_with_host_context(
        &self,
        options: SpawnOptions,
        host_context: Option<Arc<str>>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        options.validate_harness()?;
        self.native.spawn(self.clone(), options, host_context).await
    }
    /// Restores an existing Responses child without changing its identity.
    pub async fn restore_child(
        &self,
        snapshot: ChildRuntimeSnapshot,
        host_context: Option<Arc<str>>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        self.restore_runtime(ChildSnapshot::Codex(snapshot), host_context)
            .await
    }
    /// Restores an evicted child through its selected native factory.
    pub async fn restore_runtime(
        &self,
        snapshot: ChildSnapshot,
        host_context: Option<Arc<str>>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        self.native.ensure_available(self.clone()).await?;
        if self.factory.is_some() {
            self.restore_runtime_with_factory(snapshot, host_context)
                .await
        } else if snapshot.model().family() == self.harness_family() {
            self.native
                .restore(self.clone(), snapshot, host_context)
                .await
        } else {
            Err(NanocodexError::InvalidRequest(
                "checkpoint family requires a configured child factory".into(),
            ))
        }
    }
    /// Restores a retained child through the embedding's configured factory.
    /// Durable hosts use this even within the same family to reopen each child's
    /// independent execution journal before rebuilding the native driver.
    pub async fn restore_runtime_with_factory(
        &self,
        snapshot: ChildSnapshot,
        host_context: Option<Arc<str>>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        self.native.ensure_available(self.clone()).await?;
        self.factory
            .as_ref()
            .unwrap_or(&self.native)
            .restore(self.clone(), snapshot, host_context)
            .await
    }
    /// Restores through the native factory, bypassing mixed routing.
    pub async fn restore_native_runtime(
        &self,
        snapshot: ChildSnapshot,
        host_context: Option<Arc<str>>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        self.native
            .restore(self.clone(), snapshot, host_context)
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
        // No family override exists on this batch API: use the native batch
        // boundary, preserving Responses atomic admission and cancellation cleanup.
        self.native
            .spawn_many(self.clone(), count, Arc::new(observer), host_context)
            .await
    }
    /// Forking is deliberately a native lifecycle operation rather than mixed routing.
    pub async fn fork(&self) -> Result<(Nanocodex, AgentEvents)> {
        self.native.ensure_available(self.clone()).await?;
        self.native.fork(self.clone()).await
    }
}

#[cfg(feature = "openai")]
pub(super) struct OpenAiAgentFactory {
    pub(super) commands: mpsc::WeakSender<Command>,
    pub(super) shutdown: DriverShutdown,
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
            request_spawn_with_host_context(&commands, &shutdown, options, host_context).await
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
            request_spawn_many(&commands, &shutdown, count, Some(observer), host_context).await
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
        snapshot: ChildSnapshot,
        host_context: Option<Arc<str>>,
    ) -> super::backend::BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let commands = self.commands.upgrade();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            let ChildSnapshot::Codex(snapshot) = snapshot else {
                return Err(NanocodexError::InvalidRequest(
                    "native Codex factory cannot restore Claude checkpoint".into(),
                ));
            };
            let commands = commands.ok_or(NanocodexError::AgentStopped)?;
            request_command(&commands, &shutdown, |result| Command::Spawn {
                options: SpawnOptions::new()
                    .model(snapshot.model)
                    .thinking(snapshot.thinking),
                restore: Some(snapshot),
                host_context,
                result,
            })
            .await
        })
    }
    fn fork(
        &self,
        _parent: AgentHandle,
    ) -> super::backend::BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let commands = self.commands.upgrade();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            request_fork(
                &commands.ok_or(NanocodexError::AgentStopped)?,
                &shutdown,
                None,
                false,
            )
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
    pub fn harness_family(&self) -> crate::HarnessFamily {
        self.backend.harness_family()
    }

    /// Changes the selected model within this backend's family before first use.
    pub async fn set_harness_model(&self, model: crate::HarnessModel) -> Result<()> {
        if model.family() != self.harness_family() {
            return Err(NanocodexError::InvalidRequest(
                "model belongs to another harness family".into(),
            ));
        }
        self.backend.set_harness_model(model).await
    }

    /// Whether this native driver atomically retains identified steering receipts.
    pub fn durable_steering(&self) -> bool {
        self.backend.durable_steering()
    }

    /// Checks a retained steering admission without restarting its operation.
    pub async fn has_steer_receipt(&self, operation_id: String, id: String) -> Result<bool> {
        self.backend.has_steer_receipt(operation_id, id).await
    }

    /// Captures a provider-native in-memory residency checkpoint.
    pub async fn runtime_snapshot(&self) -> Result<ChildSnapshot> {
        self.backend.runtime_snapshot().await
    }

    /// Returns the stable agent identity used to reopen durable backends.
    #[must_use]
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Waits for owner-bound startup recovery without submitting a model turn.
    /// Recovery failures remain observable to every clone and block new prompts.
    pub async fn ready(&self) -> Result<()> {
        #[cfg(not(target_family = "wasm"))]
        if let Some(startup) = &self.startup {
            let mut result = startup.result.clone();
            loop {
                if let Some(outcome) = result.borrow().clone() {
                    return outcome.map_err(NanocodexError::InvalidExecutionPolicy);
                }
                result.changed().await.map_err(|_| {
                    NanocodexError::InvalidExecutionPolicy("owned startup recovery stopped".into())
                })?;
            }
        }
        Ok(())
    }

    /// Starts the embedding's reconstruction only after its native owner is bound.
    #[doc(hidden)]
    #[cfg(not(target_family = "wasm"))]
    pub fn with_owned_startup(mut self, hook: Option<Arc<dyn execution::TurnOwnership>>) -> Self {
        if let Some(hook) = hook {
            self.caller_ownership = hook.caller_ownership();
            let session = self.session_id.clone();
            let (send, result) = tokio::sync::watch::channel(None);
            let task = tokio::spawn(async move {
                let outcome = hook
                    .prepare(&session)
                    .await
                    .map_err(|error| error.to_string());
                send.send_replace(Some(outcome));
            });
            self.startup = Some(Arc::new(OwnedStartup {
                result,
                task: std::sync::Mutex::new(Some(task)),
            }));
        }
        self
    }

    /// Transfers a caller-owned handle to a harness that supplies execution and
    /// mailbox ownership. This does not stop the driver or change stored state.
    #[doc(hidden)]
    #[must_use]
    pub fn without_caller_ownership(mut self) -> Self {
        self.caller_ownership = None;
        self
    }

    /// Returns the stable identity used by events, transport metadata, and any rollout.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Returns the typed local OpenAI identity when this handle owns that backend.
    #[cfg(feature = "openai")]
    #[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
    #[must_use]
    pub const fn local_session_id(&self) -> Option<SessionId> {
        self.local_session_id
    }

    /// Returns the Codex-compatible rollout identity and path when recording is enabled.
    #[cfg(all(feature = "openai", not(target_family = "wasm")))]
    #[cfg_attr(docsrs, doc(cfg(all(feature = "openai", not(target_family = "wasm")))))]
    #[must_use]
    pub const fn rollout(&self) -> Option<&RolloutInfo> {
        self.rollout.as_ref()
    }

    /// Retries any pending rollout write and waits for a durable file flush.
    ///
    /// This is a no-op when rollout recording is disabled. CLI consumers call
    /// it at completed turn boundaries so persistence failures are user-visible.
    /// Flushing does not stop the live writer; call [`Self::shutdown`] at an
    /// explicit application or session boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured rollout cannot be written.
    #[cfg(all(feature = "openai", not(target_family = "wasm")))]
    #[cfg_attr(docsrs, doc(cfg(all(feature = "openai", not(target_family = "wasm")))))]
    pub async fn flush_rollout(&self) -> Result<()> {
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
        #[cfg(not(target_family = "wasm"))]
        if let Some(startup) = &self.startup {
            let task = startup.task.lock().expect("startup task lock").take();
            if let Some(task) = task {
                task.abort();
                let _ = task.await;
            }
        }
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
        self.ready().await?;
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
        let turn_id = format!("{}:{}", self.session_id, key.0);
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
        Ok(Turn {
            turn_id: self.canonical_turn_id(turn_id, request_id.as_deref()),
            control: TurnControl {
                key,
                backend: Arc::clone(&self.backend),
            },
            request_id,
            events: event_stream,
            result,
        })
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
        let turn_id = format!("{}:{}", self.session_id, key.0);
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
                Ok(PromptRoute::Started(Turn {
                    turn_id: self.canonical_turn_id(turn_id, request_id.as_deref()),
                    control: TurnControl {
                        key,
                        backend: Arc::clone(&self.backend),
                    },
                    request_id,
                    events: event_stream,
                    result,
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

    /// Changes the model before the first turn is accepted.
    ///
    /// # Errors
    ///
    /// Returns an error after conversation activity begins, when the selected
    /// model is incompatible with the current thinking level, or if the
    /// backend has stopped.
    pub async fn set_model(&self, model: Model) -> Result<()> {
        self.backend.set_model(model).await
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
        self.backend.set_fast_mode(enabled).await
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
    /// Returns a model or driver-stopped error. Rollout writes follow the same
    /// retry-on-[`Self::flush_rollout`] contract as prompt turns.
    pub async fn compact(&self) -> Result<()> {
        self.ready().await?;
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

    /// Copies the latest committed model boundary without changing this agent.
    ///
    /// The caller owns its unredacted model-visible history. This fails before
    /// the first safe boundary or after the driver stops, even during a turn.
    pub async fn snapshot(&self) -> Result<SessionSnapshot> {
        self.backend.snapshot().await
    }

    /// Rehydrates a child driver from this runtime's in-memory identity and history.
    #[doc(hidden)]
    pub async fn restore_child(
        &self,
        snapshot: ChildRuntimeSnapshot,
        host_context: Option<Arc<str>>,
    ) -> Result<(Self, AgentEvents)> {
        self.backend.restore_child(snapshot, host_context).await
    }

    /// Captures an in-memory idle child boundary without exposing host credentials.
    #[doc(hidden)]
    pub async fn child_snapshot(&self) -> Result<ChildRuntimeSnapshot> {
        self.backend.child_snapshot().await
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

    /// Forks from the latest safe model boundary into an independently driven
    /// agent.
    ///
    /// The child receives a fresh WebSocket and tool runtime while sharing the
    /// immutable transcript, inherited incremental delta, and prompt-cache
    /// lineage. Partial model output and unmatched tool calls are excluded.
    ///
    /// # Errors
    ///
    /// Returns an error before the first prompt reaches a safe boundary, or
    /// when the driver has stopped.
    pub async fn fork(&self) -> Result<(Self, AgentEvents)> {
        self.backend.fork(None).await
    }

    /// Forks a separately identified side conversation from the latest safe boundary.
    ///
    /// # Errors
    /// Returns an error when the backend cannot fork a side conversation.
    pub async fn fork_side_conversation(&self) -> Result<(Self, AgentEvents)> {
        self.backend.fork_side_conversation().await
    }

    /// Forks from an exact historical completed turn.
    ///
    /// # Errors
    /// Returns an error if the checkpoint belongs to another conversation.
    pub async fn fork_from(&self, completed: &TurnResult) -> Result<(Self, AgentEvents)> {
        self.backend.fork(Some(completed.clone())).await
    }
}

#[cfg(feature = "openai")]
pub(super) async fn request_fork(
    commands: &mpsc::Sender<Command>,
    shutdown: &DriverShutdown,
    checkpoint: Option<Arc<CommittedSession>>,
    side_conversation: bool,
) -> Result<(Nanocodex, AgentEvents)> {
    request_command(commands, shutdown, |result| Command::Fork {
        side_conversation,
        checkpoint,
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
