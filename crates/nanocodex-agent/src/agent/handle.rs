use super::*;

#[cfg(not(target_family = "wasm"))]
use crate::rollout::RolloutInfo;

/// Cheap, cloneable command handle for an owned agent driver.
pub struct Nanocodex {
    pub(super) commands: mpsc::Sender<Command>,
    pub(super) events: EventSink,
    pub(super) next_turn: Arc<AtomicU64>,
    pub(super) lineage_id: Arc<str>,
    pub(super) session_id: SessionId,
    pub(super) execution: Execution,
    pub(super) shutdown: DriverShutdown,
}

impl Clone for Nanocodex {
    fn clone(&self) -> Self {
        Self {
            commands: self.commands.clone(),
            events: self.events.clone(),
            next_turn: Arc::clone(&self.next_turn),
            lineage_id: Arc::clone(&self.lineage_id),
            session_id: self.session_id,
            execution: self.execution.clone(),
            shutdown: self.shutdown.clone(),
        }
    }
}

/// Weak child-agent capability for the driver that owns one tool runtime.
///
/// A tools factory receives a fresh handle for every agent driver. Holding the
/// handle does not keep its agent alive.
#[derive(Clone)]
pub struct AgentHandle {
    pub(super) commands: mpsc::WeakSender<Command>,
    pub(super) shutdown: DriverShutdown,
    pub(super) session_id: SessionId,
}

impl AgentHandle {
    /// Returns the stable session identity of the containing driver.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
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
        request_spawn(&commands, &self.shutdown, options).await
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

    fn commands(&self) -> Result<mpsc::Sender<Command>> {
        self.commands.upgrade().ok_or(NanocodexError::AgentStopped)
    }
}

impl Nanocodex {
    /// Starts configuring an agent from a reusable [`OpenAi`] client recipe.
    #[must_use]
    pub fn builder<F>(openai: OpenAi<F>) -> NanocodexBuilder<F>
    where
        F: ResponsesServiceFactory,
    {
        let (config, factory) = into_openai_parts(openai);
        NanocodexBuilder {
            config,
            tools: ToolsConfiguration::Shared(Tools::default()),
            workspace: None,
            session_id: None,
            prompt_cache: PromptCacheConfig::default(),
            codex: CodexCompatibility::default(),
            resume: None,
            factory,
        }
    }

    /// Returns the stable identity used by events, transport metadata, and any rollout.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Returns the Codex-compatible rollout identity and path when recording is enabled.
    #[cfg(not(target_family = "wasm"))]
    #[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
    #[must_use]
    pub const fn rollout(&self) -> Option<&RolloutInfo> {
        self.execution.info()
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
    #[cfg(not(target_family = "wasm"))]
    #[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
    pub async fn flush_rollout(&self) -> Result<()> {
        self.execution.flush().await
    }

    /// Gracefully stops this agent and waits for all owned resources to close.
    ///
    /// Shutdown globally invalidates this handle and every clone. It cancels an
    /// active turn, terminalizes all other accepted turns in FIFO order, waits
    /// for model and tool cleanup, and flushes and closes the rollout writer. A
    /// returned `Ok(())` therefore establishes a durable boundary suitable for
    /// an immediate same-process rollout resume.
    ///
    /// Dropping the final handle retains the existing implicit cancellation
    /// behavior, but offers no future that can join resource cleanup. Use this
    /// method at an explicit application or session boundary.
    ///
    /// # Errors
    ///
    /// Returns the shared cleanup result. The first caller initiates shutdown;
    /// concurrent and later callers on any clone await or reuse that same
    /// result.
    pub async fn shutdown(&self) -> Result<()> {
        let (initiate, receiver) = self.shutdown.request();
        if initiate && self.commands.send(Command::Shutdown).await.is_err() {
            let outcome = match self.execution.shutdown().await {
                Ok(()) => Err(NanocodexError::AgentStopped),
                Err(error) => Err(error),
            };
            self.shutdown.complete(outcome);
        }
        match receiver.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(NanocodexError::Shutdown(error)),
            Err(_) => Err(NanocodexError::AgentStopped),
        }
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
        let PromptRequest { prompt, request_id } = request.into();
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
        let execution_operation = request_id.map(ExecutionOperation::Caller).or_else(|| {
            self.execution
                .identifies_prompts()
                .then(|| ExecutionOperation::Automatic(SessionId::new().to_string()))
        });
        let key = TurnKey(self.next_turn.fetch_add(1, Ordering::Relaxed));
        let parent = tracing::Span::current();
        let parent = (!parent.is_disabled()).then_some(parent);
        let (events, event_stream) = self.events.mirrored_channel();
        let (result, receiver) = oneshot::channel();
        let (accepted, acceptance) = if execution_operation.is_some() {
            let (accepted, acceptance) = oneshot::channel();
            (Some(accepted), Some(acceptance))
        } else {
            (None, None)
        };
        if self
            .commands
            .send(Command::Prompt {
                key,
                prompt,
                execution_operation,
                accepted,
                thinking: None,
                fast_mode: None,
                parent,
                events,
                result,
            })
            .await
            .is_err()
        {
            return Err(self.shutdown.stopped_error().await);
        }
        let request_id = if let Some(acceptance) = acceptance {
            Some(match acceptance.await {
                Ok(Ok(request_id)) => request_id,
                Ok(Err(NanocodexError::AgentStopped)) | Err(_) => {
                    return Err(self.shutdown.stopped_error().await);
                }
                Ok(Err(error)) => return Err(error),
            })
        } else {
            None
        };
        Ok(Turn {
            control: TurnControl {
                key,
                commands: self.commands.clone(),
                shutdown: self.shutdown.clone(),
            },
            request_id,
            events: event_stream,
            result: receiver,
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
        let key = TurnKey(self.next_turn.fetch_add(1, Ordering::Relaxed));
        let parent = tracing::Span::current();
        let parent = (!parent.is_disabled()).then_some(parent);
        let (events, event_stream) = self.events.mirrored_channel();
        let (turn_result, turn_receiver) = oneshot::channel();
        let (route_result, route_receiver) = oneshot::channel();
        if self
            .commands
            .send(Command::RoutePrompt {
                key,
                prompt,
                parent,
                events,
                turn_result,
                route_result,
            })
            .await
            .is_err()
        {
            return Err(self.shutdown.stopped_error().await);
        }
        let route = match route_receiver.await {
            Ok(Ok(route)) => route,
            Ok(Err(NanocodexError::AgentStopped)) | Err(_) => {
                return Err(self.shutdown.stopped_error().await);
            }
            Ok(Err(error)) => return Err(error),
        };
        match route {
            PromptRouteKind::Started => Ok(PromptRoute::Started(Turn {
                control: TurnControl {
                    key,
                    commands: self.commands.clone(),
                    shutdown: self.shutdown.clone(),
                },
                request_id: None,
                events: event_stream,
                result: turn_receiver,
            })),
            PromptRouteKind::Steered => Ok(PromptRoute::Steered),
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
        request_command(&self.commands, &self.shutdown, |result| {
            Command::SetThinking { thinking, result }
        })
        .await
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
        request_command(&self.commands, &self.shutdown, |result| {
            Command::SetFastMode { enabled, result }
        })
        .await
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
        let parent = tracing::Span::current();
        let parent = (!parent.is_disabled()).then_some(parent);
        request_command(&self.commands, &self.shutdown, |result| Command::Compact {
            parent,
            result,
        })
        .await
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
        request_command(&self.commands, &self.shutdown, |result| {
            Command::AppendDeveloperMessage { text, result }
        })
        .await
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
        request_command(&self.commands, &self.shutdown, |result| Command::Context {
            result,
        })
        .await
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
        request_spawn(&self.commands, &self.shutdown, options).await
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
        self.request_fork(None).await
    }

    /// Forks from an exact historical completed turn while this agent may keep
    /// advancing on its current branch.
    ///
    /// # Errors
    ///
    /// Returns an error when the result belongs to another conversation or the
    /// driver stopped.
    pub async fn fork_from(&self, completed: &TurnResult) -> Result<(Self, AgentEvents)> {
        let TurnCheckpoint::Live(checkpoint) = &completed.checkpoint else {
            return Err(NanocodexError::ReplayedCheckpointUnavailable);
        };
        if checkpoint.lineage_id() != self.lineage_id.as_ref() {
            return Err(NanocodexError::CheckpointLineageMismatch);
        }
        self.request_fork(Some(Arc::clone(checkpoint))).await
    }

    async fn request_fork(
        &self,
        checkpoint: Option<Arc<CommittedSession>>,
    ) -> Result<(Self, AgentEvents)> {
        request_fork(&self.commands, &self.shutdown, checkpoint).await
    }
}

async fn request_fork(
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

async fn request_spawn(
    commands: &mpsc::Sender<Command>,
    shutdown: &DriverShutdown,
    options: SpawnOptions,
) -> Result<(Nanocodex, AgentEvents)> {
    request_command(commands, shutdown, |result| Command::Spawn {
        options,
        result,
    })
    .await
}

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
