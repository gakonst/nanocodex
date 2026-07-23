use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    sync::Arc,
};

use nanocodex_core::{EventSink, ModelConfig, Prompt, ReasoningMode, Thinking, UserInput};
use nanocodex_service::{
    DefaultResponsesService, ResponsesClient, ResponsesService, TransportStats,
};
use nanocodex_tools::Tools;
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot, watch};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::{
    NanocodexError,
    model::agent::{CompletedModelTurn, ModelCheckpoint, ModelRun, ModelTurnOutcome},
    prompt_cache::ModelPromptCache,
};

const STEER_CAPACITY: usize = 8;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["globalThis", "nanocodexHost"], js_name = emitEvent)]
    fn host_emit_event(event: &str);
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WasmConfig {
    api_key: String,
    #[serde(default = "default_thinking")]
    thinking: String,
    #[serde(default = "default_reasoning_mode")]
    reasoning_mode: String,
    #[serde(default = "default_websocket_url")]
    websocket_url: String,
    #[serde(default = "default_api_base_url")]
    api_base_url: String,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    workspace: Option<String>,
}

#[derive(Clone)]
struct WasmAgentFactory {
    config: Arc<ModelConfig>,
    tools: Tools,
    workspace: Option<Arc<str>>,
    lineage_id: Arc<str>,
    prompt_cache: ModelPromptCache,
}

#[derive(Clone)]
struct CommittedWasmCheckpoint {
    lineage_id: Arc<str>,
    model: ModelCheckpoint,
}

#[derive(Clone)]
struct CompletedWasmTurn {
    final_message: String,
    checkpoint: Arc<CommittedWasmCheckpoint>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct TurnKey(u64);

enum Command {
    Prompt {
        key: TurnKey,
        prompt: Prompt,
        result: oneshot::Sender<Result<CompletedWasmTurn, String>>,
    },
    Steer {
        key: TurnKey,
        prompt: Prompt,
        result: oneshot::Sender<Result<(), String>>,
    },
    Cancel {
        key: TurnKey,
        result: oneshot::Sender<Result<(), String>>,
    },
    Fork {
        checkpoint: Option<Arc<CommittedWasmCheckpoint>>,
        result: oneshot::Sender<Result<WasmNanocodex, String>>,
    },
    Spawn {
        result: oneshot::Sender<Result<WasmNanocodex, String>>,
    },
}

enum QueuedTurn {
    Pending {
        key: TurnKey,
        prompt: Prompt,
        result: oneshot::Sender<Result<CompletedWasmTurn, String>>,
    },
    Cancelled {
        prompt: Prompt,
        result: oneshot::Sender<Result<CompletedWasmTurn, String>>,
    },
}

/// Persistent WASM agent handle hosted by Node.js or a browser Worker.
#[wasm_bindgen(js_name = Nanocodex)]
pub struct WasmNanocodex {
    commands: mpsc::UnboundedSender<Command>,
    next_turn: Rc<Cell<u64>>,
    session_id: String,
    lineage_id: Arc<str>,
}

#[wasm_bindgen(js_class = Nanocodex)]
impl WasmNanocodex {
    /// Build a persistent agent from a JSON configuration object.
    ///
    /// # Errors
    ///
    /// Throws when the JSON or required configuration is invalid.
    #[wasm_bindgen(constructor)]
    pub fn new(config_json: &str) -> Result<WasmNanocodex, JsValue> {
        let config: WasmConfig = serde_json::from_str(config_json)
            .map_err(|error| js_error(format!("invalid Nanocodex configuration: {error}")))?;
        validate(&config)?;
        let thinking = config.thinking.parse::<Thinking>().map_err(js_error)?;
        let reasoning_mode = config
            .reasoning_mode
            .parse::<ReasoningMode>()
            .map_err(js_error)?;
        let session_id = config.session_id.unwrap_or_else(new_session_id);
        let model_config = Arc::new(ModelConfig {
            auth: nanocodex_core::OpenAiAuth::api_key(config.api_key),
            reasoning_mode,
            thinking,
            responses_transport: nanocodex_core::ResponsesTransport::WebSocket,
            responses_history: nanocodex_core::ResponsesHistory::Incremental,
            store_responses: true,
            websocket_url: config.websocket_url,
            api_base_url: config.api_base_url,
            system_prompt: config
                .instructions
                .map_or_else(|| ModelConfig::default().system_prompt, Arc::from),
        });
        let lineage_id = Arc::<str>::from(session_id.as_str());
        let prompt_cache = ModelPromptCache::new(Arc::clone(&lineage_id), None);
        Ok(spawn_agent(
            WasmAgentFactory {
                config: model_config,
                tools: Tools,
                workspace: config.workspace.map(Arc::<str>::from),
                lineage_id,
                prompt_cache,
            },
            session_id,
            None,
        ))
    }

    /// Stable session identifier used to route this agent's event stream.
    #[wasm_bindgen(getter, js_name = sessionId)]
    #[must_use]
    pub fn session_id(&self) -> String {
        self.session_id.clone()
    }

    /// Accept a prompt immediately and return its independently awaitable turn.
    ///
    /// # Errors
    ///
    /// Throws when the prompt is empty or the agent driver has stopped.
    pub fn prompt(&self, instruction: &str) -> Result<WasmTurn, JsValue> {
        if instruction.trim().is_empty() {
            return Err(js_error("prompt instruction must not be empty"));
        }
        self.accept_prompt(Prompt::new(instruction))
    }

    /// Accept ordered browser-safe text, image, or audio input encoded as JSON.
    ///
    /// # Errors
    ///
    /// Throws for malformed or empty content, local filesystem inputs, or a
    /// stopped driver.
    #[wasm_bindgen(js_name = promptContent)]
    pub fn prompt_content(&self, content_json: &str) -> Result<WasmTurn, JsValue> {
        self.accept_prompt(parse_browser_prompt(content_json)?)
    }

    fn accept_prompt(&self, prompt: Prompt) -> Result<WasmTurn, JsValue> {
        let key = TurnKey(self.next_turn.get());
        self.next_turn.set(key.0.saturating_add(1));
        let (result, receiver) = oneshot::channel();
        self.commands
            .send(Command::Prompt {
                key,
                prompt,
                result,
            })
            .map_err(|_| js_error("the Nanocodex driver stopped"))?;
        Ok(WasmTurn {
            key,
            commands: self.commands.clone(),
            lineage_id: Arc::clone(&self.lineage_id),
            state: Rc::new(RefCell::new(TurnState::Pending(receiver))),
        })
    }

    /// Fork the latest safe committed model boundary.
    ///
    /// # Errors
    ///
    /// Rejects before the first safe boundary or after the driver stops.
    pub async fn fork(&self) -> Result<WasmNanocodex, JsValue> {
        request_command(&self.commands, |result| Command::Fork {
            checkpoint: None,
            result,
        })
        .await
        .map_err(js_error)
    }

    /// Fork from the exact checkpoint retained by a completed historical turn.
    ///
    /// # Errors
    ///
    /// Rejects if the turn is incomplete, belongs to another lineage, or the
    /// driver stops.
    #[wasm_bindgen(js_name = forkFrom)]
    pub async fn fork_from(&self, turn: &WasmTurn) -> Result<WasmNanocodex, JsValue> {
        if turn.lineage_id != self.lineage_id {
            return Err(js_error("checkpoint belongs to another conversation"));
        }
        let checkpoint = turn.completed_checkpoint().map_err(js_error)?;
        request_command(&self.commands, |result| Command::Fork {
            checkpoint: Some(checkpoint),
            result,
        })
        .await
        .map_err(js_error)
    }

    /// Start a clean sibling with the same configuration and tools.
    ///
    /// # Errors
    ///
    /// Rejects after the driver stops.
    pub async fn spawn(&self) -> Result<WasmNanocodex, JsValue> {
        request_command(&self.commands, |result| Command::Spawn { result })
            .await
            .map_err(js_error)
    }
}

enum TurnState {
    Pending(oneshot::Receiver<Result<CompletedWasmTurn, String>>),
    Waiting,
    Completed(Result<CompletedWasmTurn, String>),
}

/// Completion and control handle for one accepted WASM turn.
#[wasm_bindgen(js_name = Turn)]
pub struct WasmTurn {
    key: TurnKey,
    commands: mpsc::UnboundedSender<Command>,
    lineage_id: Arc<str>,
    state: Rc<RefCell<TurnState>>,
}

#[wasm_bindgen(js_class = Turn)]
impl WasmTurn {
    /// Inject input at this active turn's next safe model boundary.
    ///
    /// # Errors
    ///
    /// Rejects for empty input, a queued or terminal turn, a full steer queue,
    /// or a stopped driver.
    pub async fn steer(&self, instruction: &str) -> Result<(), JsValue> {
        if instruction.trim().is_empty() {
            return Err(js_error("steer instruction must not be empty"));
        }
        self.steer_prompt(Prompt::new(instruction)).await
    }

    /// Inject ordered browser-safe text, image, or audio input at this active
    /// turn's next safe model boundary.
    ///
    /// # Errors
    ///
    /// Rejects for malformed or empty content, local filesystem inputs, a
    /// queued or terminal turn, a full steer queue, or a stopped driver.
    #[wasm_bindgen(js_name = steerContent)]
    pub async fn steer_content(&self, content_json: &str) -> Result<(), JsValue> {
        self.steer_prompt(parse_browser_prompt(content_json)?).await
    }

    async fn steer_prompt(&self, prompt: Prompt) -> Result<(), JsValue> {
        request_command(&self.commands, |result| Command::Steer {
            key: self.key,
            prompt,
            result,
        })
        .await
        .map_err(js_error)
    }

    /// Cancel this exact active or queued turn.
    ///
    /// # Errors
    ///
    /// Rejects if the turn is already terminal or the driver stops.
    pub async fn cancel(&self) -> Result<(), JsValue> {
        request_command(&self.commands, |result| Command::Cancel {
            key: self.key,
            result,
        })
        .await
        .map_err(js_error)
    }

    /// Wait for the turn and return its final assistant message.
    ///
    /// # Errors
    ///
    /// Rejects when the model run fails, the driver stops, or two consumers
    /// await the same pending turn concurrently.
    pub async fn result(&self) -> Result<String, JsValue> {
        let receiver = {
            let mut state = self.state.borrow_mut();
            match &*state {
                TurnState::Completed(result) => {
                    return result
                        .clone()
                        .map(|completed| completed.final_message)
                        .map_err(js_error);
                }
                TurnState::Waiting => return Err(js_error("this turn is already being awaited")),
                TurnState::Pending(_) => {}
            }
            match std::mem::replace(&mut *state, TurnState::Waiting) {
                TurnState::Pending(receiver) => receiver,
                TurnState::Waiting | TurnState::Completed(_) => {
                    return Err(js_error("invalid turn state"));
                }
            }
        };
        let result = receiver
            .await
            .map_err(|_| "the Nanocodex driver stopped before the turn completed".to_owned())
            .and_then(|result| result);
        *self.state.borrow_mut() = TurnState::Completed(result.clone());
        result
            .map(|completed| completed.final_message)
            .map_err(js_error)
    }
}

fn parse_browser_prompt(content_json: &str) -> Result<Prompt, JsValue> {
    let content = serde_json::from_str::<Vec<UserInput>>(content_json)
        .map_err(|error| js_error(format!("invalid prompt content: {error}")))?;
    if content.iter().any(|input| {
        matches!(
            input,
            UserInput::LocalImage { .. } | UserInput::LocalAudio { .. }
        )
    }) {
        return Err(js_error(
            "browser prompt content cannot reference local filesystem paths",
        ));
    }
    let prompt = Prompt::content(content);
    if prompt.instruction.is_empty() {
        return Err(js_error("prompt content must not be empty"));
    }
    Ok(prompt)
}

impl WasmTurn {
    fn completed_checkpoint(&self) -> Result<Arc<CommittedWasmCheckpoint>, String> {
        let state = self.state.borrow();
        match &*state {
            TurnState::Completed(Ok(completed)) => Ok(Arc::clone(&completed.checkpoint)),
            TurnState::Completed(Err(error)) => Err(error.clone()),
            TurnState::Pending(_) | TurnState::Waiting => {
                Err("historical fork requires a completed turn".to_owned())
            }
        }
    }
}

fn spawn_agent(
    factory: WasmAgentFactory,
    session_id: String,
    initial_checkpoint: Option<ModelCheckpoint>,
) -> WasmNanocodex {
    let (events, mut event_stream) = EventSink::channel(session_id.clone());
    spawn_local(async move {
        while let Some(event) = event_stream.recv().await {
            if let Ok(encoded) = serde_json::to_string(&event) {
                host_emit_event(&encoded);
            }
        }
    });

    let transport_stats = Arc::new(TransportStats::default());
    let model = match initial_checkpoint {
        None => ModelRun::new(
            events.clone(),
            Arc::clone(&factory.config),
            ResponsesClient::new(ResponsesService::standard(Arc::clone(&factory.config))),
            Arc::clone(&transport_stats),
            factory.tools.clone(),
            factory.prompt_cache.clone(),
            None,
        ),
        Some(checkpoint) => ModelRun::from_checkpoint(
            events.clone(),
            Arc::clone(&factory.config),
            ResponsesClient::new(ResponsesService::standard(Arc::clone(&factory.config))),
            Arc::clone(&transport_stats),
            factory.tools.clone(),
            factory.prompt_cache.clone(),
            checkpoint,
        ),
    };
    let (commands, receiver) = mpsc::unbounded_channel();
    spawn_local(run_driver(
        model,
        receiver,
        factory.clone(),
        events,
        transport_stats,
    ));

    WasmNanocodex {
        commands,
        next_turn: Rc::new(Cell::new(1)),
        session_id,
        lineage_id: factory.lineage_id,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the WASM driver keeps its command lifecycle in one auditable state machine"
)]
async fn run_driver(
    mut model: ModelRun<DefaultResponsesService>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    factory: WasmAgentFactory,
    events: EventSink,
    transport_stats: Arc<TransportStats>,
) {
    let mut latest_checkpoint: Option<Arc<CommittedWasmCheckpoint>> = None;
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
                        let outcome = model
                            .emit_cancelled_before_start(&prompt, factory.workspace.as_deref())
                            .map_or_else(
                                |error| Err(error.to_string()),
                                |()| Err(NanocodexError::TurnCancelled.to_string()),
                            );
                        drop(result.send(outcome));
                        continue;
                    }
                }
            }
            if !commands_open {
                return;
            }
            let Some(command) = commands.recv().await else {
                commands_open = false;
                continue;
            };
            break command;
        };

        let Command::Prompt {
            key,
            prompt,
            result,
        } = command
        else {
            handle_idle_command(command, latest_checkpoint.as_ref(), &factory);
            continue;
        };

        let (steers, steer_rx) = mpsc::channel(STEER_CAPACITY);
        let (cancel, cancel_rx) = oneshot::channel();
        let (fork_snapshots, mut fork_snapshot_rx) = watch::channel(None);
        let mut fork_snapshots_open = true;
        let mut cancel = Some(cancel);
        let mut cancel_result = None;
        let mut execution = Box::pin(model.execute(
            prompt,
            factory.workspace.clone(),
            steer_rx,
            cancel_rx,
            fork_snapshots,
        ));

        let completed = loop {
            if !commands_open {
                break execution.as_mut().await;
            }
            tokio::select! {
                biased;
                changed = fork_snapshot_rx.changed(), if fork_snapshots_open => {
                    if changed.is_err() {
                        fork_snapshots_open = false;
                        continue;
                    }
                    if let Some(snapshot) = fork_snapshot_rx.borrow_and_update().clone() {
                        latest_checkpoint = Some(Arc::new(CommittedWasmCheckpoint {
                            lineage_id: Arc::clone(&factory.lineage_id),
                            model: snapshot,
                        }));
                    }
                }
                command = commands.recv() => {
                    match command {
                        Some(Command::Prompt { key, prompt, result }) => {
                            queued_turns.push_back(QueuedTurn::Pending { key, prompt, result });
                        }
                        Some(Command::Steer { key: target, prompt, result }) => {
                            if target != key {
                                drop(result.send(Err(NanocodexError::TurnNotSteerable.to_string())));
                                continue;
                            }
                            let outcome = steers.try_send(prompt).map_err(|error| match error {
                                mpsc::error::TrySendError::Full(_) => NanocodexError::SteerQueueFull.to_string(),
                                mpsc::error::TrySendError::Closed(_) => NanocodexError::TurnNotSteerable.to_string(),
                            });
                            drop(result.send(outcome));
                        }
                        Some(Command::Cancel { key: target, result: cancellation }) => {
                            if target != key {
                                if cancel_queued_turn(&mut queued_turns, target) {
                                    drop(cancellation.send(Ok(())));
                                } else {
                                    drop(cancellation.send(Err(NanocodexError::TurnNotCancellable.to_string())));
                                }
                                continue;
                            }
                            let Some(cancel) = cancel.take() else {
                                drop(cancellation.send(Err(NanocodexError::TurnNotCancellable.to_string())));
                                continue;
                            };
                            let _ = cancel.send(());
                            cancel_result = Some(cancellation);
                            break execution.as_mut().await;
                        }
                        Some(command @ (Command::Fork { .. } | Command::Spawn { .. })) => {
                            handle_idle_command(command, latest_checkpoint.as_ref(), &factory);
                        }
                        None => commands_open = false,
                    }
                }
                outcome = &mut execution => break outcome,
            }
        };
        drop(execution);

        let (outcome, was_cancelled) = match completed {
            Ok(ModelTurnOutcome::Completed(CompletedModelTurn {
                final_message,
                checkpoint,
            })) => {
                let checkpoint = Arc::new(CommittedWasmCheckpoint {
                    lineage_id: Arc::clone(&factory.lineage_id),
                    model: checkpoint,
                });
                latest_checkpoint = Some(Arc::clone(&checkpoint));
                (
                    Ok(CompletedWasmTurn {
                        final_message,
                        checkpoint,
                    }),
                    false,
                )
            }
            Ok(ModelTurnOutcome::Cancelled(checkpoint)) => {
                let checkpoint = Arc::new(CommittedWasmCheckpoint {
                    lineage_id: Arc::clone(&factory.lineage_id),
                    model: checkpoint,
                });
                latest_checkpoint = Some(Arc::clone(&checkpoint));
                model = ModelRun::from_checkpoint(
                    events.clone(),
                    Arc::clone(&factory.config),
                    ResponsesClient::new(ResponsesService::standard(Arc::clone(&factory.config))),
                    Arc::clone(&transport_stats),
                    factory.tools.clone(),
                    factory.prompt_cache.clone(),
                    checkpoint.model.clone(),
                );
                (Err(NanocodexError::TurnCancelled.to_string()), true)
            }
            Err(error) => (Err(error.to_string()), false),
        };
        drop(result.send(outcome));
        if let Some(cancel_result) = cancel_result {
            drop(cancel_result.send(if was_cancelled {
                Ok(())
            } else {
                Err(NanocodexError::TurnNotCancellable.to_string())
            }));
        }
    }
}

fn handle_idle_command(
    command: Command,
    latest: Option<&Arc<CommittedWasmCheckpoint>>,
    factory: &WasmAgentFactory,
) {
    match command {
        Command::Fork { checkpoint, result } => {
            let checkpoint = checkpoint.or_else(|| latest.cloned());
            let outcome = checkpoint
                .ok_or_else(|| NanocodexError::ForkBeforeCompletedTurn.to_string())
                .and_then(|checkpoint| spawn_fork(factory, &checkpoint));
            drop(result.send(outcome));
        }
        Command::Spawn { result } => {
            let session_id = new_session_id();
            let lineage_id = Arc::<str>::from(session_id.as_str());
            let mut clean = factory.clone();
            clean.lineage_id = lineage_id;
            clean.prompt_cache = ModelPromptCache::new(Arc::clone(&clean.lineage_id), None);
            drop(result.send(Ok(spawn_agent(clean, session_id, None))));
        }
        Command::Steer { result, .. } => {
            drop(result.send(Err(NanocodexError::TurnNotSteerable.to_string())));
        }
        Command::Cancel { result, .. } => {
            drop(result.send(Err(NanocodexError::TurnNotCancellable.to_string())));
        }
        Command::Prompt { .. } => {}
    }
}

fn spawn_fork(
    factory: &WasmAgentFactory,
    checkpoint: &CommittedWasmCheckpoint,
) -> Result<WasmNanocodex, String> {
    if checkpoint.lineage_id != factory.lineage_id {
        return Err("checkpoint belongs to another conversation".to_owned());
    }
    let mut fork = factory.clone();
    fork.workspace = Some(Arc::from(checkpoint.model.workspace()));
    Ok(spawn_agent(
        fork,
        new_session_id(),
        Some(checkpoint.model.clone()),
    ))
}

fn cancel_queued_turn(queued_turns: &mut VecDeque<QueuedTurn>, target: TurnKey) -> bool {
    let Some(position) = queued_turns
        .iter()
        .position(|queued| matches!(queued, QueuedTurn::Pending { key, .. } if *key == target))
    else {
        return false;
    };
    let Some(QueuedTurn::Pending { prompt, result, .. }) = queued_turns.remove(position) else {
        return false;
    };
    queued_turns.insert(position, QueuedTurn::Cancelled { prompt, result });
    true
}

async fn request_command<T>(
    commands: &mpsc::UnboundedSender<Command>,
    command: impl FnOnce(oneshot::Sender<Result<T, String>>) -> Command,
) -> Result<T, String> {
    let (result, receiver) = oneshot::channel();
    commands
        .send(command(result))
        .map_err(|_| "the Nanocodex driver stopped".to_owned())?;
    receiver
        .await
        .map_err(|_| "the Nanocodex driver stopped".to_owned())?
}

fn validate(config: &WasmConfig) -> Result<(), JsValue> {
    for (name, value) in [
        ("api_key", config.api_key.as_str()),
        ("websocket_url", config.websocket_url.as_str()),
        ("api_base_url", config.api_base_url.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(js_error(format!("{name} must not be empty")));
        }
    }
    if config
        .session_id
        .as_deref()
        .is_some_and(|session_id| session_id.trim().is_empty())
    {
        return Err(js_error("session_id must not be empty"));
    }
    Ok(())
}

fn new_session_id() -> String {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let nonce = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("nanocodex-wasm-{:x}-{nonce}", js_sys::Date::now().to_bits())
}

fn default_thinking() -> String {
    "medium".to_owned()
}

fn default_reasoning_mode() -> String {
    "standard".to_owned()
}

fn default_websocket_url() -> String {
    "wss://api.openai.com/v1/responses".to_owned()
}

fn default_api_base_url() -> String {
    "https://api.openai.com/v1".to_owned()
}

#[allow(clippy::needless_pass_by_value)]
fn js_error(error: impl ToString) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}
