use std::{cell::RefCell, rc::Rc, sync::Arc};

use nanocodex_core::{EventSink, ModelConfig, Prompt, ReasoningMode, Thinking};
use nanocodex_service::{ResponsesClient, ResponsesService, TransportStats};
use nanocodex_tools::Tools;
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot, watch};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::{
    NanocodexError,
    model::agent::{ModelRun, ModelTurnOutcome},
};

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

struct Command {
    prompt: Prompt,
    result: oneshot::Sender<Result<String, String>>,
}

/// Persistent WASM agent handle hosted by Node.js or a browser Worker.
#[wasm_bindgen(js_name = Nanocodex)]
pub struct WasmNanocodex {
    commands: mpsc::UnboundedSender<Command>,
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
            websocket_url: config.websocket_url,
            api_base_url: config.api_base_url,
            system_prompt: config
                .instructions
                .map_or_else(|| ModelConfig::default().system_prompt, Arc::from),
        });
        let (events, mut event_stream) = EventSink::channel(session_id);
        let lineage_id = Arc::<str>::from(events.request_id());
        spawn_local(async move {
            while let Some(event) = event_stream.recv().await {
                if let Ok(encoded) = serde_json::to_string(&event) {
                    host_emit_event(&encoded);
                }
            }
        });

        let service = ResponsesService::standard(Arc::clone(&model_config));
        let transport_stats = Arc::new(TransportStats::default());
        let mut model = ModelRun::new(
            events,
            model_config,
            ResponsesClient::new(service),
            transport_stats,
            Tools,
            lineage_id,
        );
        let workspace = config.workspace.map(Arc::<str>::from);
        let (commands, mut receiver) = mpsc::unbounded_channel::<Command>();
        spawn_local(async move {
            while let Some(command) = receiver.recv().await {
                let (steers, steer_rx) = mpsc::channel(1);
                drop(steers);
                let (_cancel, cancel_rx) = tokio::sync::oneshot::channel();
                let (fork_snapshots, _fork_snapshot_rx) = watch::channel(None);
                let outcome = model
                    .execute(
                        command.prompt,
                        workspace.clone(),
                        steer_rx,
                        cancel_rx,
                        fork_snapshots,
                    )
                    .await
                    .and_then(|outcome| match outcome {
                        ModelTurnOutcome::Completed(completed) => Ok(completed.final_message),
                        ModelTurnOutcome::Cancelled(checkpoint) => {
                            drop(checkpoint);
                            Err(NanocodexError::TurnCancelled)
                        }
                    })
                    .map_err(|error| error.to_string());
                drop(command.result.send(outcome));
            }
        });
        Ok(Self { commands })
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
        let prompt = Prompt::new(instruction);
        let (result, receiver) = oneshot::channel();
        self.commands
            .send(Command { prompt, result })
            .map_err(|_| js_error("the Nanocodex driver stopped"))?;
        Ok(WasmTurn {
            state: Rc::new(RefCell::new(TurnState::Pending(receiver))),
        })
    }
}

enum TurnState {
    Pending(oneshot::Receiver<Result<String, String>>),
    Waiting,
    Completed(Result<String, String>),
}

/// Completion handle for one accepted WASM turn.
#[wasm_bindgen(js_name = Turn)]
pub struct WasmTurn {
    state: Rc<RefCell<TurnState>>,
}

#[wasm_bindgen(js_class = Turn)]
impl WasmTurn {
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
                TurnState::Completed(result) => return result.clone().map_err(js_error),
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
        result.map_err(js_error)
    }
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
