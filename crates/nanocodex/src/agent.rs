use std::{
    collections::VecDeque,
    fmt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use nanocodex_core::{AgentEvents, EventSink, ModelConfig, Prompt, Thinking};
use nanocodex_service::{
    DefaultResponsesService, ResponsesAttempt, ResponsesClient, ResponsesService,
    ResponsesServiceResponse, TransportStats,
};
use nanocodex_tools::{Tools, ToolsBuildError};
use tokio::sync::{mpsc, oneshot};
use tower::Service;
use tracing::{Instrument, info_span};

use crate::{
    NanocodexError, Result,
    model::agent::{CompletedModelTurn, ModelCheckpoint, ModelRun},
    responses::{FactoryResponses, LayeredResponses, Responses, StandardResponses},
};

const COMMAND_CAPACITY: usize = 8;
const STEER_CAPACITY: usize = 8;

type ServiceFactory<S> = Arc<dyn Fn() -> S + Send + Sync>;
type ToolsFactory =
    Arc<dyn Fn(AgentHandle) -> std::result::Result<Tools, ToolsBuildError> + Send + Sync>;

#[derive(Clone)]
enum ToolsConfiguration {
    Shared(Tools),
    PerAgent(ToolsFactory),
}

impl ToolsConfiguration {
    fn materialize(&self, agent_handle: AgentHandle) -> Result<Tools> {
        match self {
            Self::Shared(tools) => Ok(tools.clone()),
            Self::PerAgent(factory) => factory(agent_handle).map_err(Into::into),
        }
    }
}

/// Completion handle for an accepted turn.
///
/// Dropping this handle does not cancel the accepted turn. Use [`Self::cancel`]
/// before dropping it when the work should stop.
#[must_use = "a turn continues running when dropped; await result(), control it, or explicitly drop it"]
pub struct Turn {
    control: TurnControl,
    result: oneshot::Receiver<Result<TurnResult>>,
}

impl Turn {
    /// Returns a cheap cloneable capability targeting this exact turn.
    #[must_use]
    pub fn control(&self) -> TurnControl {
        self.control.clone()
    }

    /// Injects additional input into this turn at its next safe model boundary.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty prompt, when this turn is queued or no
    /// longer active, when its steering queue is full, or if the driver stops.
    pub async fn steer(&self, prompt: impl Into<Prompt>) -> Result<()> {
        self.control.steer(prompt).await
    }

    /// Cancels this exact unfinished turn.
    ///
    /// A queued turn is removed before execution and acknowledged immediately;
    /// its result and terminal event retain their FIFO position behind earlier
    /// turns. An active turn waits for its model and tool resources to stop
    /// before cancellation is acknowledged.
    ///
    /// # Errors
    ///
    /// Returns an error when this turn has already finished or if the driver
    /// stops.
    pub async fn cancel(&self) -> Result<()> {
        self.control.cancel().await
    }

    /// Waits for and returns the final typed turn result.
    ///
    /// # Errors
    ///
    /// Returns the model-run failure or an error if the driver stopped early.
    pub async fn result(self) -> Result<TurnResult> {
        self.result.await.map_err(|_| NanocodexError::TurnStopped)?
    }
}

/// Cheap cloneable control capability for one accepted turn.
#[derive(Clone)]
pub struct TurnControl {
    key: TurnKey,
    commands: mpsc::Sender<Command>,
}

impl TurnControl {
    /// Injects additional input into the targeted turn.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty prompt, when the turn is not active, when
    /// its steering queue is full, or if the driver stops.
    pub async fn steer(&self, prompt: impl Into<Prompt>) -> Result<()> {
        let prompt = prompt.into();
        if prompt.instruction.is_empty() {
            return Err(NanocodexError::InvalidRequest(
                "steer instruction must not be empty".to_owned(),
            ));
        }
        request_command(&self.commands, |result| Command::Steer {
            key: self.key,
            prompt,
            result,
        })
        .await
    }

    /// Cancels the targeted unfinished turn.
    ///
    /// # Errors
    ///
    /// Returns an error when the turn has already finished or if the driver
    /// stops.
    pub async fn cancel(&self) -> Result<()> {
        request_command(&self.commands, |result| Command::Cancel {
            key: self.key,
            result,
        })
        .await
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct TurnKey(u64);

/// Final result of a completed turn.
#[derive(Clone)]
#[non_exhaustive]
pub struct TurnResult {
    pub final_message: String,
    checkpoint: Arc<CommittedCheckpoint>,
}

impl fmt::Debug for TurnResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TurnResult")
            .field("final_message", &self.final_message)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct CommittedCheckpoint {
    lineage_id: Arc<str>,
    model: ModelCheckpoint,
}

enum Command {
    Prompt {
        key: TurnKey,
        prompt: Prompt,
        result: oneshot::Sender<Result<TurnResult>>,
    },
    Steer {
        key: TurnKey,
        prompt: Prompt,
        result: oneshot::Sender<Result<()>>,
    },
    Cancel {
        key: TurnKey,
        result: oneshot::Sender<Result<()>>,
    },
    Fork {
        checkpoint: Option<Arc<CommittedCheckpoint>>,
        result: oneshot::Sender<Result<(Nanocodex, AgentEvents)>>,
    },
    Spawn {
        result: oneshot::Sender<Result<(Nanocodex, AgentEvents)>>,
    },
}

enum QueuedTurn {
    Pending {
        key: TurnKey,
        prompt: Prompt,
        result: oneshot::Sender<Result<TurnResult>>,
    },
    Cancelled {
        prompt: Prompt,
        result: oneshot::Sender<Result<TurnResult>>,
    },
}

/// Cheap, cloneable command handle for an owned agent driver.
#[derive(Clone)]
pub struct Nanocodex {
    commands: mpsc::Sender<Command>,
    next_turn: Arc<AtomicU64>,
    lineage_id: Arc<str>,
}

/// Weak child-agent capability for the driver that owns one tool runtime.
///
/// A tools factory receives a fresh handle for every agent driver. Holding the
/// handle does not keep its agent alive.
#[derive(Clone)]
pub struct AgentHandle {
    commands: mpsc::WeakSender<Command>,
}

impl AgentHandle {
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
        let commands = self.commands()?;
        request_spawn(&commands).await
    }

    /// Forks the containing agent's latest completed turn.
    ///
    /// # Errors
    ///
    /// Returns an error before the first completed turn or after the containing
    /// agent driver has stopped.
    pub async fn fork(&self) -> Result<(Nanocodex, AgentEvents)> {
        let commands = self.commands()?;
        request_fork(&commands, None).await
    }

    fn commands(&self) -> Result<mpsc::Sender<Command>> {
        self.commands.upgrade().ok_or(NanocodexError::AgentStopped)
    }
}

impl Nanocodex {
    /// Builds a running agent with the standard instructions, tools, thinking level,
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
            tools: ToolsConfiguration::Shared(Tools::default()),
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
        let prompt = prompt.into();
        if prompt.instruction.is_empty() {
            return Err(NanocodexError::InvalidRequest(
                "prompt instruction must not be empty".to_owned(),
            ));
        }
        let key = TurnKey(self.next_turn.fetch_add(1, Ordering::Relaxed));
        let (result, receiver) = oneshot::channel();
        if self
            .commands
            .send(Command::Prompt {
                key,
                prompt,
                result,
            })
            .await
            .is_err()
        {
            return Err(NanocodexError::AgentStopped);
        }
        Ok(Turn {
            control: TurnControl {
                key,
                commands: self.commands.clone(),
            },
            result: receiver,
        })
    }

    /// Forks from the latest completed turn into an independently driven agent.
    ///
    /// The child receives a fresh WebSocket and tool runtime while sharing the
    /// immutable committed transcript and prompt-cache lineage.
    ///
    /// # Errors
    ///
    /// Returns an error before the first completed turn or when the driver has
    /// stopped.
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
        if completed.checkpoint.lineage_id != self.lineage_id {
            return Err(NanocodexError::CheckpointLineageMismatch);
        }
        self.request_fork(Some(Arc::clone(&completed.checkpoint)))
            .await
    }

    async fn request_fork(
        &self,
        checkpoint: Option<Arc<CommittedCheckpoint>>,
    ) -> Result<(Self, AgentEvents)> {
        request_fork(&self.commands, checkpoint).await
    }
}

async fn request_fork(
    commands: &mpsc::Sender<Command>,
    checkpoint: Option<Arc<CommittedCheckpoint>>,
) -> Result<(Nanocodex, AgentEvents)> {
    request_command(commands, |result| Command::Fork { checkpoint, result }).await
}

async fn request_spawn(commands: &mpsc::Sender<Command>) -> Result<(Nanocodex, AgentEvents)> {
    request_command(commands, |result| Command::Spawn { result }).await
}

async fn request_command<T>(
    commands: &mpsc::Sender<Command>,
    command: impl FnOnce(oneshot::Sender<Result<T>>) -> Command,
) -> Result<T> {
    let (result, receiver) = oneshot::channel();
    commands
        .send(command(result))
        .await
        .map_err(|_| NanocodexError::AgentStopped)?;
    receiver.await.map_err(|_| NanocodexError::AgentStopped)?
}

/// Builder for a running agent with deferred Responses service composition.
pub struct NanocodexBuilder<S = StandardResponses> {
    config: ModelConfig,
    tools: ToolsConfiguration,
    workspace: Option<PathBuf>,
    session_id: Option<String>,
    responses: Responses<S>,
}

impl<S> NanocodexBuilder<S> {
    /// Replaces the stable system/developer instructions.
    #[must_use]
    pub fn instructions(mut self, instructions: impl Into<Arc<str>>) -> Self {
        self.config.system_prompt = instructions.into();
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
        self.tools = ToolsConfiguration::Shared(tools);
        self
    }

    /// Builds a fresh tool collection for every agent driver.
    ///
    /// The factory receives a weak capability targeting the driver whose tool
    /// runtime is being built. Use this for agent-relative tools such as Code
    /// Mode child-agent tools; stateless tools may continue using
    /// [`Self::tools`].
    #[must_use]
    pub fn tools_factory<F>(mut self, factory: F) -> Self
    where
        F: Fn(AgentHandle) -> std::result::Result<Tools, ToolsBuildError> + Send + Sync + 'static,
    {
        self.tools = ToolsConfiguration::PerAgent(Arc::new(factory));
        self
    }

    /// Fixes the workspace used by every prompt in this agent session.
    #[must_use]
    pub fn workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    /// Sets the root session ID and stable prompt-cache lineage inherited by forks.
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
        let service_factory: ServiceFactory<DefaultResponsesService> = Arc::new({
            let config = Arc::clone(&config);
            move || ResponsesService::standard(Arc::clone(&config))
        });
        build_agent(
            config,
            self.tools,
            self.workspace,
            self.session_id,
            service_factory,
        )
    }
}

impl<L> NanocodexBuilder<LayeredResponses<L>>
where
    L: tower::Layer<DefaultResponsesService> + Clone + Send + Sync + 'static,
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
        let layers = self.responses.service.0;
        let service_factory: ServiceFactory<L::Service> = Arc::new({
            let config = Arc::clone(&config);
            move || {
                layers
                    .clone()
                    .service(ResponsesService::standard(Arc::clone(&config)))
            }
        });
        build_agent(
            config,
            self.tools,
            self.workspace,
            self.session_id,
            service_factory,
        )
    }
}

impl<F, S> NanocodexBuilder<FactoryResponses<F>>
where
    F: Fn() -> S + Send + Sync + 'static,
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    S::Error: Into<NanocodexError> + Send + 'static,
    S::Future: Send,
{
    /// Builds and spawns the root agent from a caller-provided fresh-service
    /// factory. The same factory is invoked independently for every spawned or
    /// forked child.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration or when no Tokio runtime is
    /// active.
    pub fn build(mut self) -> Result<(Nanocodex, AgentEvents)> {
        configure(&mut self.config, &self.responses);
        validate(&self.config, self.session_id.as_deref())?;
        let config = Arc::new(self.config);
        let service_factory: ServiceFactory<S> = Arc::new(self.responses.service.0);
        build_agent(
            config,
            self.tools,
            self.workspace,
            self.session_id,
            service_factory,
        )
    }
}

/// Sole owner of mutable run state and the Responses service stack.
struct AgentDriver<S> {
    commands: mpsc::Receiver<Command>,
    events: EventSink,
    client: ResponsesClient<S>,
    transport_stats: Arc<TransportStats>,
    tools: Tools,
    workspace: Option<Arc<str>>,
    spawner: BranchSpawner<S>,
    initial_checkpoint: Option<ModelCheckpoint>,
}

struct BranchSpawner<S> {
    config: Arc<ModelConfig>,
    tools: ToolsConfiguration,
    lineage_id: Arc<str>,
    service_factory: ServiceFactory<S>,
}

impl<S> Clone for BranchSpawner<S> {
    fn clone(&self) -> Self {
        Self {
            config: Arc::clone(&self.config),
            tools: self.tools.clone(),
            lineage_id: Arc::clone(&self.lineage_id),
            service_factory: Arc::clone(&self.service_factory),
        }
    }
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
    #[allow(clippy::too_many_lines)]
    async fn run(mut self) -> Result<()> {
        self.tools.start_providers();
        let session_id = self.events.request_id().to_owned();
        let thinking = self.spawner.config.thinking;
        let inherited_checkpoint = self.initial_checkpoint.as_ref().map(|checkpoint| {
            Arc::new(CommittedCheckpoint {
                lineage_id: Arc::clone(&self.spawner.lineage_id),
                model: checkpoint.clone(),
            })
        });
        let mut model = if let Some(checkpoint) = self.initial_checkpoint.take() {
            ModelRun::from_checkpoint(
                self.events.clone(),
                Arc::clone(&self.spawner.config),
                self.client,
                Arc::clone(&self.transport_stats),
                self.tools.clone(),
                Arc::clone(&self.spawner.lineage_id),
                checkpoint,
            )
        } else {
            ModelRun::new(
                self.events.clone(),
                Arc::clone(&self.spawner.config),
                self.client,
                Arc::clone(&self.transport_stats),
                self.tools.clone(),
                Arc::clone(&self.spawner.lineage_id),
            )
        };
        let mut turn_index = 0_u64;
        let mut latest_checkpoint = inherited_checkpoint;
        let mut queued_turns = VecDeque::new();
        let mut commands_open = true;
        loop {
            let command = loop {
                if let Some(queued) = queued_turns.pop_front() {
                    match queued {
                        QueuedTurn::Pending {
                            key,
                            prompt,
                            result,
                        } => {
                            break Command::Prompt {
                                key,
                                prompt,
                                result,
                            };
                        }
                        QueuedTurn::Cancelled { prompt, result } => {
                            turn_index += 1;
                            let turn_span = info_span!(
                                target: "nanocodex",
                                parent: None,
                                "agent.turn",
                                otel.kind = "internal",
                                otel.status_code = "ERROR",
                                session.id = session_id.as_str(),
                                model = nanocodex_core::MODEL,
                                thinking = thinking.as_str(),
                                turn.index = turn_index,
                                prompt.bytes = prompt.instruction.text_bytes(),
                                status = "cancelled",
                            );
                            let _guard = turn_span.enter();
                            model
                                .emit_cancelled_before_start(&prompt, self.workspace.as_deref())?;
                            drop(result.send(Err(NanocodexError::TurnCancelled)));
                            continue;
                        }
                    }
                }
                if commands_open {
                    let Some(command) = self.commands.recv().await else {
                        commands_open = false;
                        continue;
                    };
                    break command;
                }
                return Ok(());
            };
            let Command::Prompt {
                key,
                prompt,
                result,
            } = command
            else {
                handle_idle_command(
                    command,
                    latest_checkpoint.as_ref(),
                    &self.spawner,
                    self.workspace.clone(),
                );
                continue;
            };
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
            let (steers, steer_rx) = mpsc::channel(STEER_CAPACITY);
            let (cancel, cancel_rx) = oneshot::channel();
            let mut cancel = Some(cancel);
            let mut cancel_result = None;
            let mut execution = Box::pin(
                model
                    .execute(prompt, self.workspace.clone(), steer_rx, cancel_rx)
                    .instrument(turn_span.clone()),
            );
            let completed = loop {
                if !commands_open {
                    break execution.as_mut().await;
                }
                tokio::select! {
                    biased;
                    command = self.commands.recv() => {
                        match command {
                            Some(Command::Prompt { key, prompt, result }) => {
                                queued_turns.push_back(QueuedTurn::Pending {
                                    key,
                                    prompt,
                                    result,
                                });
                            }
                            Some(Command::Steer { key: target, prompt, result }) => {
                                if target != key {
                                    drop(result.send(Err(NanocodexError::TurnNotSteerable)));
                                    continue;
                                }
                                let outcome = steers.try_send(prompt).map_err(|error| match error {
                                    mpsc::error::TrySendError::Full(_) => {
                                        NanocodexError::SteerQueueFull
                                    }
                                    mpsc::error::TrySendError::Closed(_) => {
                                        NanocodexError::TurnNotSteerable
                                    }
                                });
                                drop(result.send(outcome));
                            }
                            Some(Command::Cancel { key: target, result: cancellation }) => {
                                if target != key {
                                    if cancel_queued_turn(&mut queued_turns, target) {
                                        drop(cancellation.send(Ok(())));
                                    } else {
                                        drop(cancellation.send(Err(
                                            NanocodexError::TurnNotCancellable,
                                        )));
                                    }
                                    continue;
                                }
                                let Some(cancel) = cancel.take() else {
                                    drop(cancellation.send(Err(
                                        NanocodexError::TurnNotCancellable,
                                    )));
                                    continue;
                                };
                                let _ = cancel.send(());
                                cancel_result = Some(cancellation);
                                break execution.as_mut().await;
                            }
                            Some(command @ (Command::Fork { .. } | Command::Spawn { .. })) => {
                                handle_idle_command(
                                    command,
                                    latest_checkpoint.as_ref(),
                                    &self.spawner,
                                    self.workspace.clone(),
                                );
                            }
                            None => commands_open = false,
                        }
                    }
                    outcome = &mut execution => break outcome,
                }
            };
            drop(execution);
            let was_cancelled = matches!(&completed, Err(NanocodexError::TurnCancelled));
            if was_cancelled {
                let service = (self.spawner.service_factory)();
                let replacement = if let Some(checkpoint) = latest_checkpoint.as_ref() {
                    ModelRun::from_checkpoint(
                        self.events.clone(),
                        Arc::clone(&self.spawner.config),
                        ResponsesClient::new(service),
                        Arc::clone(&self.transport_stats),
                        self.tools.clone(),
                        Arc::clone(&self.spawner.lineage_id),
                        checkpoint.model.clone(),
                    )
                } else {
                    ModelRun::new(
                        self.events.clone(),
                        Arc::clone(&self.spawner.config),
                        ResponsesClient::new(service),
                        Arc::clone(&self.transport_stats),
                        self.tools.clone(),
                        Arc::clone(&self.spawner.lineage_id),
                    )
                };
                model = replacement;
            }
            let outcome = completed.map(|completed| {
                let CompletedModelTurn {
                    final_message,
                    checkpoint,
                } = completed;
                let checkpoint = Arc::new(CommittedCheckpoint {
                    lineage_id: Arc::clone(&self.spawner.lineage_id),
                    model: checkpoint,
                });
                latest_checkpoint = Some(Arc::clone(&checkpoint));
                TurnResult {
                    final_message,
                    checkpoint,
                }
            });
            turn_span.record(
                "status",
                if was_cancelled {
                    "cancelled"
                } else if outcome.is_ok() {
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
            if let Some(cancel_result) = cancel_result {
                let outcome = if was_cancelled {
                    Ok(())
                } else {
                    Err(NanocodexError::TurnNotCancellable)
                };
                drop(cancel_result.send(outcome));
            }
        }
    }
}

fn cancel_queued_turn(queued_turns: &mut VecDeque<QueuedTurn>, target: TurnKey) -> bool {
    let Some(position) = queued_turns
        .iter()
        .position(|queued| matches!(queued, QueuedTurn::Pending { key, .. } if *key == target))
    else {
        return false;
    };
    let Some(queued) = queued_turns.remove(position) else {
        return false;
    };
    let QueuedTurn::Pending { prompt, result, .. } = queued else {
        return false;
    };
    queued_turns.insert(position, QueuedTurn::Cancelled { prompt, result });
    true
}

fn handle_idle_command<S>(
    command: Command,
    latest: Option<&Arc<CommittedCheckpoint>>,
    spawner: &BranchSpawner<S>,
    workspace: Option<Arc<str>>,
) where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    S::Error: Into<NanocodexError> + Send + 'static,
    S::Future: Send,
{
    match command {
        Command::Fork { checkpoint, result } => {
            let checkpoint = checkpoint.or_else(|| latest.cloned());
            let outcome = checkpoint
                .ok_or(NanocodexError::ForkBeforeCompletedTurn)
                .and_then(|checkpoint| spawner.spawn_fork(&checkpoint));
            drop(result.send(outcome));
        }
        Command::Spawn { result } => {
            drop(result.send(spawner.spawn_clean(workspace)));
        }
        Command::Steer { result, .. } => {
            drop(result.send(Err(NanocodexError::TurnNotSteerable)));
        }
        Command::Cancel { result, .. } => {
            drop(result.send(Err(NanocodexError::TurnNotCancellable)));
        }
        Command::Prompt { .. } => {}
    }
}

impl<S> BranchSpawner<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    S::Error: Into<NanocodexError> + Send + 'static,
    S::Future: Send,
{
    fn spawn_fork(&self, checkpoint: &CommittedCheckpoint) -> Result<(Nanocodex, AgentEvents)> {
        let session_id = new_session_id();
        let workspace = Some(Arc::<str>::from(checkpoint.model.workspace()));
        spawn_agent_driver(
            self.clone(),
            session_id,
            workspace,
            (self.service_factory)(),
            Some(checkpoint.model.clone()),
        )
    }

    fn spawn_clean(&self, workspace: Option<Arc<str>>) -> Result<(Nanocodex, AgentEvents)> {
        let session_id = new_session_id();
        let spawner = Self {
            config: Arc::clone(&self.config),
            tools: self.tools.clone(),
            lineage_id: Arc::from(session_id.as_str()),
            service_factory: Arc::clone(&self.service_factory),
        };
        let service = (self.service_factory)();
        spawn_agent_driver(spawner, session_id, workspace, service, None)
    }
}

fn build_agent<S>(
    config: Arc<ModelConfig>,
    tools: ToolsConfiguration,
    workspace: Option<PathBuf>,
    session_id: Option<String>,
    service_factory: ServiceFactory<S>,
) -> Result<(Nanocodex, AgentEvents)>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    S::Error: Into<NanocodexError> + Send + 'static,
    S::Future: Send,
{
    let session_id = session_id.unwrap_or_else(new_session_id);
    let lineage_id = Arc::<str>::from(session_id.as_str());
    let workspace = workspace
        .map(|path| {
            path.into_os_string()
                .into_string()
                .map(Arc::<str>::from)
                .map_err(|path| NanocodexError::WorkspaceNotUtf8 {
                    path: PathBuf::from(path),
                })
        })
        .transpose()?;
    let service = service_factory();
    spawn_agent_driver(
        BranchSpawner {
            config,
            tools,
            lineage_id,
            service_factory,
        },
        session_id,
        workspace,
        service,
        None,
    )
}

fn spawn_agent_driver<S>(
    spawner: BranchSpawner<S>,
    session_id: String,
    workspace: Option<Arc<str>>,
    service: S,
    initial_checkpoint: Option<ModelCheckpoint>,
) -> Result<(Nanocodex, AgentEvents)>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    S::Error: Into<NanocodexError> + Send + 'static,
    S::Future: Send,
{
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|_| NanocodexError::TokioRuntimeUnavailable)?;
    let (events, event_stream) = EventSink::channel(session_id);
    let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
    let tools = spawner.tools.materialize(AgentHandle {
        commands: commands.downgrade(),
    })?;
    let transport_stats = Arc::new(TransportStats::default());
    let agent = Nanocodex {
        commands,
        next_turn: Arc::new(AtomicU64::new(1)),
        lineage_id: Arc::clone(&spawner.lineage_id),
    };
    drop(
        runtime.spawn(
            AgentDriver {
                commands: receiver,
                events,
                client: ResponsesClient::new(service),
                transport_stats,
                tools,
                workspace,
                spawner,
                initial_checkpoint,
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let sequence = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    format!("nanocodex-{timestamp:x}-{sequence:x}")
}

#[cfg(test)]
mod tests {
    use std::{
        future::{Pending, Ready, pending},
        time::Duration,
    };

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

    #[derive(Clone)]
    struct PendingService;

    impl Service<ResponsesAttempt> for PendingService {
        type Response = ResponsesServiceResponse;
        type Error = NanocodexError;
        type Future = Pending<std::result::Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, _request: ResponsesAttempt) -> Self::Future {
            pending()
        }
    }

    #[tokio::test]
    async fn accepts_a_caller_composed_tower_service_factory() {
        let responses = Responses::builder()
            .service(|| {
                ServiceBuilder::new()
                    .layer(TimeoutLayer::new(Duration::from_secs(30)))
                    .layer(ConcurrencyLimitLayer::new(1))
                    .service(NeverCalled)
            })
            .build();

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

    #[tokio::test]
    async fn forking_before_a_completed_turn_is_typed() {
        let (agent, events) = Nanocodex::new("test").unwrap();
        let Err(error) = agent.fork().await else {
            panic!("fork unexpectedly succeeded");
        };
        assert!(matches!(error, NanocodexError::ForkBeforeCompletedTurn));
        drop((agent, events));
    }

    #[tokio::test]
    async fn steering_without_an_active_turn_is_typed() {
        let (agent, events) = Nanocodex::new("test").unwrap();
        let control = TurnControl {
            key: TurnKey(1),
            commands: agent.commands.clone(),
        };
        let Err(error) = control.steer("additional direction").await else {
            panic!("steer unexpectedly succeeded");
        };
        assert!(matches!(error, NanocodexError::TurnNotSteerable));
        drop((agent, events));
    }

    #[tokio::test]
    async fn caller_service_factory_supports_cancellation() {
        let builds = Arc::new(AtomicU64::new(0));
        let factory_builds = Arc::clone(&builds);
        let responses = Responses::builder()
            .service(move || {
                factory_builds.fetch_add(1, Ordering::Relaxed);
                PendingService
            })
            .build();
        let (agent, events) = Nanocodex::builder("test")
            .responses(responses)
            .build()
            .unwrap();
        let turn = agent.prompt("keep running").await.unwrap();

        turn.cancel().await.unwrap();
        assert!(matches!(
            turn.result().await,
            Err(NanocodexError::TurnCancelled)
        ));
        assert_eq!(builds.load(Ordering::Relaxed), 2);
        drop((agent, events));
    }

    #[tokio::test]
    async fn accepts_a_caller_service_factory_for_future_children() {
        let responses = Responses::builder().service(|| NeverCalled).build();
        let (agent, events) = Nanocodex::builder("test")
            .responses(responses)
            .build()
            .unwrap();
        drop((agent, events));
    }

    #[tokio::test]
    async fn caller_service_factory_supports_clean_spawn() {
        let (handles, mut received_handles) = mpsc::unbounded_channel();
        let responses = Responses::builder().service(|| NeverCalled).build();
        let (agent, events) = Nanocodex::builder("test")
            .responses(responses)
            .tools_factory(move |handle| {
                drop(handles.send(handle));
                Tools::builder().without_defaults().build()
            })
            .build()
            .unwrap();
        let handle = received_handles.recv().await.unwrap();

        let (child, child_events) = handle.spawn().await.unwrap();
        drop((child, child_events, agent, events));
    }

    #[tokio::test]
    async fn an_agent_handle_does_not_keep_its_driver_alive() {
        let (handles, mut received_handles) = mpsc::unbounded_channel();
        let (agent, events) = Nanocodex::builder("test")
            .tools_factory(move |handle| {
                drop(handles.send(handle));
                Tools::builder().without_defaults().build()
            })
            .build()
            .unwrap();
        let handle = received_handles.recv().await.unwrap();

        drop(agent);
        let Err(error) = handle.spawn().await else {
            panic!("agent handle unexpectedly kept its driver alive");
        };
        assert!(matches!(error, NanocodexError::AgentStopped));
        let Err(error) = handle.fork().await else {
            panic!("agent handle unexpectedly kept its driver alive");
        };
        assert!(matches!(error, NanocodexError::AgentStopped));
        drop(events);
    }

    #[test]
    fn building_requires_a_tokio_runtime() {
        assert!(matches!(
            Nanocodex::new("test"),
            Err(NanocodexError::TokioRuntimeUnavailable)
        ));
    }
}
