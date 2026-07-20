use std::{collections::VecDeque, fmt, path::PathBuf, sync::Arc};

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
    AgentError, NanocodexError, Result,
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
        prompt: Prompt,
        result: oneshot::Sender<Result<TurnResult>>,
    },
    Steer {
        prompt: Prompt,
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

/// Cheap, cloneable command handle for an owned agent driver.
#[derive(Clone)]
pub struct Nanocodex {
    commands: mpsc::Sender<Command>,
    workspace: Option<Arc<str>>,
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
    /// Returns an error after the containing driver has stopped or when its
    /// Responses configuration cannot create a fresh service.
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
        self.commands
            .upgrade()
            .ok_or(NanocodexError::Agent(AgentError::DriverStopped))
    }
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

    /// Injects additional input into the active turn.
    ///
    /// Accepted input is retained in FIFO order and sampled at the next safe
    /// model boundary. It remains part of the active turn rather than starting
    /// a separately awaitable turn. Use [`Self::prompt`] to queue a follow-up.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty prompt, when no turn is active, or if the
    /// driver stopped.
    pub async fn steer(&self, prompt: impl Into<Prompt>) -> Result<()> {
        let mut prompt = prompt.into();
        if prompt.instruction.is_empty() {
            return Err(NanocodexError::InvalidRequest(
                "steer instruction must not be empty".to_owned(),
            ));
        }
        // A steer always belongs to the active turn and therefore cannot move
        // that turn to another workspace.
        prompt.workspace = self.workspace.as_deref().map(str::to_owned);
        let (result, receiver) = oneshot::channel();
        self.commands
            .send(Command::Steer { prompt, result })
            .await
            .map_err(|_| NanocodexError::Agent(AgentError::DriverStopped))?;
        receiver
            .await
            .map_err(|_| NanocodexError::Agent(AgentError::DriverStopped))?
    }

    /// Forks from the latest completed turn into an independently driven agent.
    ///
    /// The child receives a fresh WebSocket and tool runtime while sharing the
    /// immutable committed transcript and prompt-cache lineage.
    ///
    /// # Errors
    ///
    /// Returns an error before the first completed turn, when the driver has
    /// stopped, or when the configured Responses stack cannot be recreated.
    pub async fn fork(&self) -> Result<(Self, AgentEvents)> {
        self.request_fork(None).await
    }

    /// Forks from an exact historical completed turn while this agent may keep
    /// advancing on its current branch.
    ///
    /// # Errors
    ///
    /// Returns an error when the result belongs to another conversation, the
    /// driver stopped, or the configured Responses stack cannot be recreated.
    pub async fn fork_from(&self, completed: &TurnResult) -> Result<(Self, AgentEvents)> {
        if completed.checkpoint.lineage_id != self.lineage_id {
            return Err(AgentError::CheckpointLineageMismatch.into());
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
    let (result, receiver) = oneshot::channel();
    commands
        .send(Command::Fork { checkpoint, result })
        .await
        .map_err(|_| NanocodexError::Agent(AgentError::DriverStopped))?;
    receiver
        .await
        .map_err(|_| NanocodexError::Agent(AgentError::DriverStopped))?
}

async fn request_spawn(commands: &mpsc::Sender<Command>) -> Result<(Nanocodex, AgentEvents)> {
    let (result, receiver) = oneshot::channel();
    commands
        .send(Command::Spawn { result })
        .await
        .map_err(|_| NanocodexError::Agent(AgentError::DriverStopped))?;
    receiver
        .await
        .map_err(|_| NanocodexError::Agent(AgentError::DriverStopped))?
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
        let service = service_factory();
        build_agent(
            config,
            self.tools,
            self.workspace,
            self.session_id,
            service,
            Some(service_factory),
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
        let service = service_factory();
        build_agent(
            config,
            self.tools,
            self.workspace,
            self.session_id,
            service,
            Some(service_factory),
        )
    }
}

impl<F, S> NanocodexBuilder<FactoryResponses<F>>
where
    F: Fn() -> S + Clone + Send + Sync + 'static,
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
        let factory = self.responses.service.0;
        let service_factory: ServiceFactory<S> = Arc::new(move || factory.clone()());
        let service = service_factory();
        build_agent(
            config,
            self.tools,
            self.workspace,
            self.session_id,
            service,
            Some(service_factory),
        )
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
            None,
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
    service_factory: Option<ServiceFactory<S>>,
}

impl<S> Clone for BranchSpawner<S> {
    fn clone(&self) -> Self {
        Self {
            config: Arc::clone(&self.config),
            tools: self.tools.clone(),
            lineage_id: Arc::clone(&self.lineage_id),
            service_factory: self.service_factory.clone(),
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
                self.events,
                Arc::clone(&self.spawner.config),
                self.client,
                Arc::clone(&self.transport_stats),
                self.tools.clone(),
                Arc::clone(&self.spawner.lineage_id),
                checkpoint,
            )
        } else {
            ModelRun::new(
                self.events,
                Arc::clone(&self.spawner.config),
                self.client,
                Arc::clone(&self.transport_stats),
                self.tools.clone(),
                Arc::clone(&self.spawner.lineage_id),
            )
        };
        let mut turn_index = 0_u64;
        let mut latest_checkpoint = inherited_checkpoint;
        let mut queued_prompts: VecDeque<(Prompt, oneshot::Sender<Result<TurnResult>>)> =
            VecDeque::new();
        let mut commands_open = true;
        loop {
            let command = if let Some(prompt) = queued_prompts.pop_front() {
                Command::Prompt {
                    prompt: prompt.0,
                    result: prompt.1,
                }
            } else if commands_open {
                let Some(command) = self.commands.recv().await else {
                    break;
                };
                command
            } else {
                break;
            };
            let Command::Prompt { prompt, result } = command else {
                handle_idle_command(
                    command,
                    latest_checkpoint.as_ref(),
                    &self.spawner,
                    self.workspace.clone(),
                );
                continue;
            };
            if self.workspace.is_none() {
                self.workspace = prompt.workspace.as_deref().map(Arc::from);
            }
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
            let mut execution = Box::pin(
                model
                    .execute(prompt, steer_rx)
                    .instrument(turn_span.clone()),
            );
            let completed = loop {
                if !commands_open {
                    break execution.await;
                }
                tokio::select! {
                    biased;
                    command = self.commands.recv() => {
                        match command {
                            Some(Command::Prompt { prompt, result }) => {
                                queued_prompts.push_back((prompt, result));
                            }
                            Some(Command::Steer { prompt, result }) => {
                                let outcome = steers.try_send(prompt).map_err(|error| match error {
                                    mpsc::error::TrySendError::Full(_) => {
                                        NanocodexError::Agent(AgentError::SteerQueueFull)
                                    }
                                    mpsc::error::TrySendError::Closed(_) => {
                                        NanocodexError::Agent(AgentError::NoActiveTurnToSteer)
                                    }
                                });
                                drop(result.send(outcome));
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
                .ok_or_else(|| NanocodexError::Agent(AgentError::ForkBeforeCompletedTurn))
                .and_then(|checkpoint| spawner.spawn_fork(&checkpoint));
            drop(result.send(outcome));
        }
        Command::Spawn { result } => {
            drop(result.send(spawner.spawn_clean(workspace)));
        }
        Command::Steer { result, .. } => {
            drop(result.send(Err(AgentError::NoActiveTurnToSteer.into())));
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
        let service_factory = self
            .service_factory
            .as_ref()
            .ok_or(AgentError::ChildUnsupportedForResponsesService)?;
        let session_id = new_session_id();
        let workspace = Some(Arc::<str>::from(checkpoint.model.workspace()));
        spawn_agent_driver(
            self.clone(),
            session_id,
            workspace,
            service_factory(),
            Some(checkpoint.model.clone()),
        )
    }

    fn spawn_clean(&self, workspace: Option<Arc<str>>) -> Result<(Nanocodex, AgentEvents)> {
        let service_factory = self
            .service_factory
            .as_ref()
            .ok_or(AgentError::ChildUnsupportedForResponsesService)?;
        let session_id = new_session_id();
        let spawner = Self {
            config: Arc::clone(&self.config),
            tools: self.tools.clone(),
            lineage_id: Arc::from(session_id.as_str()),
            service_factory: self.service_factory.clone(),
        };
        spawn_agent_driver(spawner, session_id, workspace, service_factory(), None)
    }
}

fn build_agent<S>(
    config: Arc<ModelConfig>,
    tools: ToolsConfiguration,
    workspace: Option<PathBuf>,
    session_id: Option<String>,
    service: S,
    service_factory: Option<ServiceFactory<S>>,
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
                .map_err(|path| AgentError::WorkspaceNotUtf8 {
                    path: PathBuf::from(path),
                })
        })
        .transpose()?;
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
        .map_err(|_| NanocodexError::Agent(AgentError::TokioRuntimeUnavailable))?;
    let (events, event_stream) = EventSink::channel(session_id);
    let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
    let tools = spawner.tools.materialize(AgentHandle {
        commands: commands.downgrade(),
    })?;
    let transport_stats = Arc::new(TransportStats::default());
    let agent = Nanocodex {
        commands,
        workspace,
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
                workspace: agent.workspace.clone(),
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

    #[tokio::test]
    async fn forking_before_a_completed_turn_is_typed() {
        let (agent, events) = Nanocodex::new("test").unwrap();
        let Err(error) = agent.fork().await else {
            panic!("fork unexpectedly succeeded");
        };
        assert!(matches!(
            error,
            NanocodexError::Agent(AgentError::ForkBeforeCompletedTurn)
        ));
        drop((agent, events));
    }

    #[tokio::test]
    async fn steering_without_an_active_turn_is_typed() {
        let (agent, events) = Nanocodex::new("test").unwrap();
        let Err(error) = agent.steer("additional direction").await else {
            panic!("steer unexpectedly succeeded");
        };
        assert!(matches!(
            error,
            NanocodexError::Agent(AgentError::NoActiveTurnToSteer)
        ));
        drop((agent, events));
    }

    #[tokio::test]
    async fn accepts_a_caller_service_factory_for_future_children() {
        let responses = Responses::builder().service_factory(|| NeverCalled).build();
        let (agent, events) = Nanocodex::builder("test")
            .responses(responses)
            .build()
            .unwrap();
        drop((agent, events));
    }

    #[tokio::test]
    async fn clean_spawn_requires_a_fresh_responses_service_factory() {
        let (handles, mut received_handles) = mpsc::unbounded_channel();
        let responses = Responses::builder().service(NeverCalled).build();
        let (agent, events) = Nanocodex::builder("test")
            .responses(responses)
            .tools_factory(move |handle| {
                drop(handles.send(handle));
                Tools::builder().without_defaults().build()
            })
            .build()
            .unwrap();
        let handle = received_handles.recv().await.unwrap();

        let Err(error) = handle.spawn().await else {
            panic!("clean spawn unexpectedly recreated an opaque Responses service");
        };
        assert!(matches!(
            error,
            NanocodexError::Agent(AgentError::ChildUnsupportedForResponsesService)
        ));
        drop((agent, events));
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
        assert!(matches!(
            error,
            NanocodexError::Agent(AgentError::DriverStopped)
        ));
        let Err(error) = handle.fork().await else {
            panic!("agent handle unexpectedly kept its driver alive");
        };
        assert!(matches!(
            error,
            NanocodexError::Agent(AgentError::DriverStopped)
        ));
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
