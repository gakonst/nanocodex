use super::backend::{
    BackendPrompt, BackendPromptRoute, BackendTurn, BackendTurnKey, LifecycleBackend,
};
use super::*;

#[cfg(all(feature = "openai", not(target_family = "wasm")))]
use crate::rollout::RolloutInfo;

/// Cheap, cloneable command handle for an owned agent driver.
pub struct Nanocodex {
    pub(super) backend: Arc<dyn LifecycleBackend>,
    pub(super) events: nanocodex_oai_api::events::AgentEventPublisher,
    pub(super) next_turn: Arc<AtomicU64>,
    pub(super) agent_id: Arc<str>,
    pub(super) session_id: Arc<str>,
    #[cfg(feature = "openai")]
    pub(super) local_session_id: Option<SessionId>,
    #[cfg(all(feature = "openai", not(target_family = "wasm")))]
    pub(super) rollout: Option<RolloutInfo>,
}

impl Clone for Nanocodex {
    fn clone(&self) -> Self {
        Self {
            backend: Arc::clone(&self.backend),
            events: self.events.clone(),
            next_turn: Arc::clone(&self.next_turn),
            agent_id: Arc::clone(&self.agent_id),
            session_id: Arc::clone(&self.session_id),
            #[cfg(feature = "openai")]
            local_session_id: self.local_session_id,
            #[cfg(all(feature = "openai", not(target_family = "wasm")))]
            rollout: self.rollout.clone(),
        }
    }
}

/// Weak child-agent capability for the driver that owns one tool runtime.
///
/// A tools factory receives a fresh handle for every agent driver. Holding the
/// handle does not keep its agent alive.
#[derive(Clone)]
#[cfg(feature = "openai")]
pub struct AgentHandle {
    pub(super) commands: mpsc::WeakSender<Command>,
    pub(super) shutdown: DriverShutdown,
    pub(super) session_id: Arc<str>,
    pub(super) depth: u32,
    pub(super) task_name: Option<Arc<str>>,
    pub(super) user_inputs: Arc<std::sync::Mutex<watch::Receiver<u64>>>,
    pub(super) activity: Arc<std::sync::Mutex<serde_json::Value>>,
}

#[cfg(feature = "openai")]
impl AgentHandle {
    /// Returns the current lifecycle status for model-facing orchestration.
    #[doc(hidden)]
    #[must_use]
    pub fn collaboration_status(&self) -> serde_json::Value {
        if self
            .commands
            .upgrade()
            .is_none_or(|commands| commands.is_closed())
        {
            return serde_json::json!("shutdown");
        }
        self.activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Waits for accepted user steering without retaining the driver itself.
    #[doc(hidden)]
    pub async fn wait_for_user_input(&self) -> bool {
        let mut receiver = self
            .user_inputs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if receiver.changed().await.is_err() {
            return false;
        }
        self.user_inputs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .borrow_and_update();
        true
    }

    /// Returns the nesting depth of this driver (zero for a root).
    #[doc(hidden)]
    #[must_use]
    pub const fn depth(&self) -> u32 {
        self.depth
    }

    /// Returns the canonical task name of a model-directed child.
    #[doc(hidden)]
    #[must_use]
    pub fn task_name(&self) -> Option<&str> {
        self.task_name.as_deref()
    }

    /// Returns the session owned by this weak driver capability.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Starts a clean agent with the containing driver's private configuration,
    /// service factory, workspace policy, and per-agent tools factory.
    ///
    /// The child receives a new session, cache lineage, conversation, driver,
    /// WebSocket, and tool runtime. It does not inherit conversation history.
    ///
    /// # Errors
    ///
    /// Returns an error after the containing driver has stopped.
    pub async fn spawn(&self) -> Result<(Nanocodex, AgentEvents)> {
        self.spawn_with(SpawnOptions::new()).await
    }

    /// Starts a clean agent with optional model and reasoning overrides.
    ///
    /// Unspecified values inherit this agent's settings when the driver handles
    /// the spawn command. Overrides affect only the new child.
    ///
    /// # Errors
    ///
    /// Returns an error after the containing driver has stopped.
    pub async fn spawn_with(&self, options: SpawnOptions) -> Result<(Nanocodex, AgentEvents)> {
        let commands = self.commands()?;
        request_spawn_with_host_context(&commands, &self.shutdown, options, None).await
    }

    /// Starts a clean child with embedding-owned context inherited by its tool invocations.
    #[doc(hidden)]
    pub async fn spawn_with_host_context(
        &self,
        options: SpawnOptions,
        host_context: Option<Arc<str>>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        let commands = self.commands()?;
        request_spawn_with_host_context(&commands, &self.shutdown, options, host_context).await
    }

    /// Starts a named child with the selected inherited conversation boundary.
    #[doc(hidden)]
    pub async fn spawn_task(
        &self,
        options: SpawnOptions,
        fork_turns: ForkTurns,
        agent_name: Arc<str>,
        host_context: Option<Arc<str>>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        let commands = self.commands()?;
        request_command(&commands, &self.shutdown, |result| Command::Spawn {
            options,
            fork_turns,
            agent_name: Some(agent_name),
            host_context,
            result,
        })
        .await
    }

    /// Starts several clean agents in the order requested.
    ///
    /// Every child receives the containing driver's private configuration,
    /// service factory, workspace policy, and per-agent tools factory. The
    /// children do not inherit conversation history.
    ///
    /// # Errors
    ///
    /// Returns an error after the containing driver has stopped.
    pub async fn spawn_many(&self, count: usize) -> Result<Vec<(Nanocodex, AgentEvents)>> {
        let commands = self.commands()?;
        request_spawn_many(&commands, &self.shutdown, count, None, None).await
    }

    /// Starts several clean agents and synchronously observes each child as it
    /// is materialized by the parent driver.
    ///
    /// This low-level seam lets embeddings pair tool-runtime registration with
    /// rollback even when the batch request is cancelled before its result is
    /// delivered.
    #[doc(hidden)]
    pub async fn spawn_many_observed(
        &self,
        count: usize,
        observer: impl Fn(&str) + Send + Sync + 'static,
    ) -> Result<Vec<(Nanocodex, AgentEvents)>> {
        let commands = self.commands()?;
        request_spawn_many(
            &commands,
            &self.shutdown,
            count,
            Some(Arc::new(observer)),
            None,
        )
        .await
    }

    /// Observes a clean batch while privately inheriting embedding-owned context.
    #[doc(hidden)]
    pub async fn spawn_many_observed_with_host_context(
        &self,
        count: usize,
        observer: impl Fn(&str) + Send + Sync + 'static,
        host_context: Option<Arc<str>>,
    ) -> Result<Vec<(Nanocodex, AgentEvents)>> {
        let commands = self.commands()?;
        request_spawn_many(
            &commands,
            &self.shutdown,
            count,
            Some(Arc::new(observer)),
            host_context,
        )
        .await
    }

    /// Forks the containing agent's latest safe model boundary.
    ///
    /// # Errors
    ///
    /// Returns an error before the first prompt reaches a safe boundary, or
    /// after the containing agent driver has stopped.
    pub async fn fork(&self) -> Result<(Nanocodex, AgentEvents)> {
        let commands = self.commands()?;
        request_fork(&commands, &self.shutdown, None).await
    }

    /// Delivers runtime-owned agent communication without starting an idle turn.
    /// Active delivery uses the durable steering queue at the next model boundary.
    #[doc(hidden)]
    pub async fn queue_message(&self, prompt: Prompt) -> Result<()> {
        let text = prompt.agent_message().ok_or_else(|| {
            NanocodexError::InvalidRequest("expected typed agent communication".into())
        })?;
        let commands = self.commands()?;
        request_command(&commands, &self.shutdown, |result| {
            Command::AppendDeveloperMessage {
                text,
                steer_active: true,
                result,
            }
        })
        .await
        .map(|_| ())
    }

    fn commands(&self) -> Result<mpsc::Sender<Command>> {
        self.commands.upgrade().ok_or(NanocodexError::AgentStopped)
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

    /// Returns the stable agent identity used to reopen durable backends.
    #[must_use]
    pub fn agent_id(&self) -> &str {
        &self.agent_id
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
            control: TurnControl {
                key,
                backend: Arc::clone(&self.backend),
            },
            request_id,
            events: event_stream,
            result,
        })
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

    /// Forks from an exact historical completed turn while this agent may keep
    /// advancing on its current branch.
    ///
    /// # Errors
    ///
    /// Returns an error when the result belongs to another conversation or the
    /// driver stopped.
    pub async fn fork_from(&self, completed: &TurnResult) -> Result<(Self, AgentEvents)> {
        self.backend.fork(Some(completed.clone())).await
    }
}

#[cfg(feature = "openai")]
pub(super) async fn request_fork(
    commands: &mpsc::Sender<Command>,
    shutdown: &DriverShutdown,
    checkpoint: Option<Arc<CommittedSession>>,
) -> Result<(Nanocodex, AgentEvents)> {
    request_command(commands, shutdown, |result| Command::Fork {
        checkpoint,
        result,
    })
    .await
}

#[cfg(feature = "openai")]
pub(super) async fn request_spawn(
    commands: &mpsc::Sender<Command>,
    shutdown: &DriverShutdown,
    options: SpawnOptions,
) -> Result<(Nanocodex, AgentEvents)> {
    request_spawn_with_host_context(commands, shutdown, options, None).await
}

#[cfg(feature = "openai")]
async fn request_spawn_with_host_context(
    commands: &mpsc::Sender<Command>,
    shutdown: &DriverShutdown,
    options: SpawnOptions,
    host_context: Option<Arc<str>>,
) -> Result<(Nanocodex, AgentEvents)> {
    request_command(commands, shutdown, |result| Command::Spawn {
        options,
        fork_turns: ForkTurns::None,
        agent_name: None,
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
