//! Explicit Claude embedding. No Codex model catalog or ambient tools are installed.
//!
//! `create` accepts camelCase JSON: model (required), apiKey OR authHostId,
//! sessionId (optional; durable IDs must match durabilityId), endpoint,
//! subscriptionCompatibility, hostDefinitionId (required with tools),
//! tools (exec/wait definitions), maxTokens, thinking (effort),
//! adaptiveThinking, keepThinking, cache ("off", "5m", "1h"), autoCompact
//! (false rejects unsupported disabling, true keeps backend policy),
//! autoCompactWindowTokens, contextWindowTokens, instructions, systemBlocks,
//! workspace, parallelTools, parallelSafeTools (host-derived),
//! durabilityHostId, durabilityId,
//! terminalReceiptRetention. Code Mode is mandatory; provider tools and direct
//! client tool search are rejected. Credentials never enter a checkpoint.
//!
//! Host contracts: claudeAuth(authHostId) -> Promise<JSON header map string>;
//! executeClaudeTool(hostDefinitionId, name, inputJson, sessionId, callId, model,
//! turnId) -> Promise<JSON {content: string | block[], isError?: boolean,
//! metadata?: value, structuredResult?: value}>. Host errors are redacted.

use super::{
    AgentEvents, Cell, DurableAgentExt, HashMap, JavaScriptDurabilityStore, JavaScriptSpawnRouter,
    JsFuture, JsValue, Mutex, Prompt, PromptRoute, Rc, RefCell, RustNanocodex, TurnState,
    WasmHarnessFactory, WasmSubagents, WasmSubagentsConfig, WasmTurn, forward_events,
    host_cancel_code_turn, js_error, validate_operation_id,
};
use nanocodex_claude::{
    Claude, ClaudeAuthFuture, ClaudeAuthProvider, ClaudeAuthUnavailable, ClaudeClient,
    ClaudeNestedToolUpdate, ClaudeToolInvocation, ClaudeToolProgress, ClaudeToolReply, ClaudeTools,
    ServerToolDefinition, ToolDefinition, ToolResultContent,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, rc::Weak, sync::Arc};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = claudeAuth)]
    fn host_claude_auth(auth_host_id: u32) -> Result<js_sys::Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = executeClaudeTool)]
    fn host_execute_claude_tool(
        host_definition_id: u32,
        name: &str,
        input: &str,
        session_id: &str,
        call_id: &str,
        model: &str,
        turn_id: &str,
        local_definitions: &str,
        execute_local_tool: &JsValue,
    ) -> Result<js_sys::Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = nextClaudeCodeUpdate)]
    fn host_next_claude_code_update(
        host_definition_id: u32,
        session_id: &str,
        call_id: &str,
    ) -> Result<js_sys::Promise, JsValue>;
}

/// Streams the live nested starts and results of one exec/wait observation
/// until the host closes it. Missing support or host errors only end live
/// observation; the final receipt still settles every reported call.
async fn observe_live_updates(
    host_definition_id: u32,
    session_id: &str,
    call_id: &str,
    progress: &ClaudeToolProgress,
) {
    #[derive(Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum Update {
        NestedCallStarted {
            call_id: String,
            name: String,
            input: Value,
        },
        NestedCallCompleted {
            call: Value,
        },
        #[serde(other)]
        Other,
    }
    loop {
        let Ok(next) = host_next_claude_code_update(host_definition_id, session_id, call_id) else {
            return;
        };
        let Ok(value) = JsFuture::from(next).await else {
            return;
        };
        let Some(encoded) = value.as_string() else {
            return;
        };
        match serde_json::from_str::<Update>(&encoded) {
            Ok(Update::NestedCallStarted {
                call_id,
                name,
                input,
            }) => progress.update(ClaudeNestedToolUpdate::Started {
                call_id,
                name,
                input,
            }),
            Ok(Update::NestedCallCompleted { call }) => {
                progress.update(ClaudeNestedToolUpdate::Completed(call));
            }
            Ok(Update::Other) | Err(_) => {}
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct ClaudeConfig {
    model: String,
    tool_mode: Option<String>,
    session_id: Option<String>,
    api_key: Option<String>,
    auth_host_id: Option<u32>,
    endpoint: Option<String>,
    #[serde(default)]
    subscription_compatibility: bool,
    subscription_identity: Option<nanocodex_claude::SubscriptionIdentity>,
    host_definition_id: Option<u32>,
    #[serde(default)]
    tools: Vec<ToolDefinition>,
    #[serde(default)]
    server_tools: Vec<ServerToolDefinition>,
    max_tokens: Option<u32>,
    thinking: Option<super::Thinking>,
    #[serde(default)]
    adaptive_thinking: bool,
    #[serde(default)]
    keep_thinking: bool,
    #[serde(default)]
    cache: CachePolicy,
    auto_compact: Option<bool>,
    auto_compact_window_tokens: Option<u64>,
    context_window_tokens: Option<u64>,
    instructions: Option<String>,
    system_blocks: Option<Vec<Value>>,
    workspace: Option<String>,
    #[serde(default)]
    parallel_tools: bool,
    /// Host tools declaring `supportsParallelToolCalls`; consecutive calls overlap.
    #[serde(default)]
    parallel_safe_tools: Vec<String>,
    #[serde(default)]
    client_tool_search: bool,
    durability_host_id: Option<String>,
    durability_id: Option<String>,
    terminal_receipt_retention: Option<usize>,
    subagents: Option<WasmSubagentsConfig>,
    #[serde(default)]
    subagent_routing: bool,
    codex_harness: Option<Value>,
}

#[derive(Default, Deserialize, Serialize)]
enum CachePolicy {
    #[default]
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "5m")]
    FiveMinutes,
    #[serde(rename = "1h")]
    OneHour,
}

impl ClaudeConfig {
    fn validate(&self) -> Result<(), &'static str> {
        if !matches!(self.tool_mode.as_deref(), None | Some("code-only")) {
            return Err("unsupported Claude toolMode");
        }
        if !self.server_tools.is_empty()
            || self.client_tool_search
            || self.tools.len() != 2
            || !self.tools.iter().any(|tool| tool.name == "exec")
            || !self.tools.iter().any(|tool| tool.name == "wait")
        {
            return Err("Claude Code Mode must expose only exec and wait");
        }
        if self.model.trim().is_empty() {
            return Err("Claude model must not be empty");
        }
        if self
            .session_id
            .as_ref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err("sessionId must not be empty");
        }
        if let (Some(session_id), Some(durability_id)) = (&self.session_id, &self.durability_id)
            && session_id != durability_id
        {
            return Err("durable Claude sessionId must equal durabilityId");
        }
        match (&self.api_key, self.auth_host_id) {
            (Some(key), None) if !key.trim().is_empty() => {}
            (None, Some(_)) => {}
            _ => return Err("supply exactly one nonempty apiKey or authHostId"),
        }
        if !self.tools.is_empty() && self.host_definition_id.is_none() {
            return Err("explicit Claude tools require hostDefinitionId");
        }
        if self.instructions.is_some() && self.system_blocks.is_some() {
            return Err("instructions and systemBlocks are mutually exclusive");
        }
        if self.auto_compact == Some(false) {
            return Err("disabling Claude automatic compaction is unsupported by this backend");
        }
        if self.max_tokens == Some(0)
            || self.context_window_tokens == Some(0)
            || self.auto_compact_window_tokens == Some(0)
        {
            return Err("Claude token limits must be positive");
        }
        match (&self.durability_host_id, &self.durability_id) {
            (None, None) => {
                if self.terminal_receipt_retention.is_some() {
                    return Err("terminalReceiptRetention requires durability");
                }
            }
            (Some(host), Some(id)) if !host.trim().is_empty() && !id.trim().is_empty() => {}
            _ => {
                return Err(
                    "durabilityHostId and durabilityId must be nonempty and supplied together",
                );
            }
        }
        if self
            .terminal_receipt_retention
            .is_some_and(|limit| limit > 4_096)
        {
            return Err("terminalReceiptRetention must be from 0 through 4096");
        }
        if self.endpoint.as_ref().is_some_and(|endpoint| {
            reqwest::Url::parse(endpoint).map_or(true, |url| {
                !matches!(url.scheme(), "http" | "https")
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.fragment().is_some()
            })
        }) {
            return Err(
                "endpoint must be an explicit HTTP(S) Messages URL without userinfo or fragment",
            );
        }
        Ok(())
    }
}

struct JavaScriptClaudeAuth {
    auth_host_id: u32,
}

impl ClaudeAuthProvider for JavaScriptClaudeAuth {
    fn headers(
        &self,
    ) -> ClaudeAuthFuture<'_, Result<reqwest::header::HeaderMap, ClaudeAuthUnavailable>> {
        Box::pin(async move {
            let promise = host_claude_auth(self.auth_host_id).map_err(|_| ClaudeAuthUnavailable)?;
            let result = JsFuture::from(promise)
                .await
                .map_err(|_| ClaudeAuthUnavailable)?;
            let encoded = result.as_string().ok_or(ClaudeAuthUnavailable)?;
            let headers: BTreeMap<String, String> =
                serde_json::from_str(&encoded).map_err(|_| ClaudeAuthUnavailable)?;
            if headers.is_empty() {
                return Err(ClaudeAuthUnavailable);
            }
            let mut output = reqwest::header::HeaderMap::new();
            for (name, value) in headers {
                let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| ClaudeAuthUnavailable)?;
                let mut value = reqwest::header::HeaderValue::from_str(&value)
                    .map_err(|_| ClaudeAuthUnavailable)?;
                value.set_sensitive(true);
                output.insert(name, value);
            }
            Ok(output)
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct HostToolReply {
    content: ToolResultContent,
    #[serde(default)]
    is_error: bool,
    metadata: Option<Value>,
    structured_result: Option<Value>,
}

async fn execute_tool(
    host_definition_id: u32,
    name: &str,
    input: Value,
    invocation: ClaudeToolInvocation,
    local_tools: Option<ClaudeTools>,
) -> Result<ClaudeToolReply, String> {
    // Dropping a JsFuture only stops Rust observation. Abort the host's active
    // handlers when native cancellation drops this invocation, preserving the
    // session registration so an interrupted child can be reused.
    struct PendingTool<'a> {
        session_id: &'a str,
        cancel: Option<js_sys::Function>,
        settled: bool,
    }
    impl Drop for PendingTool<'_> {
        fn drop(&mut self) {
            if !self.settled {
                if let Some(cancel) = &self.cancel {
                    let _ = cancel.call0(&JsValue::UNDEFINED);
                } else {
                    // Compatibility with hosts that only expose session abort.
                    host_cancel_code_turn(self.session_id);
                }
            }
        }
    }
    let local_tools = local_tools.unwrap_or_default();
    let definitions = serde_json::to_string(
        &local_tools
            .definitions()
            .iter()
            .map(|definition| {
                serde_json::json!({ "type": "function", "name": definition.name,
            "description": definition.description, "parameters": definition.input_schema })
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|error| error.to_string())?;
    let original = invocation.clone();
    // JS owns the callback for the entire cell, including yielded continuations.
    let callback = Closure::wrap(
        Box::new(move |name: String, input: String, call_id: String| {
            let tools = local_tools.clone();
            let mut invocation = original.clone();
            invocation.call_id = call_id;
            invocation.progress = None;
            let (abort, registration) = futures_util::future::AbortHandle::new_pair();
            let future = futures_util::future::Abortable::new(
                async move {
                    let input = serde_json::from_str(&input)
                        .map_err(|_| js_error("Claude nested tool input is invalid"))?;
                    let reply = tools
                        .execute(&name, input, invocation)
                        .await
                        .map_err(js_error)?;
                    let output = match reply.content {
                        ToolResultContent::Text(text) => text,
                        ToolResultContent::Blocks(blocks) => {
                            serde_json::to_string(&blocks).map_err(js_error)?
                        }
                    };
                    Ok(JsValue::from_str(
                        &serde_json::json!({ "output": output, "success": !reply.is_error,
                "structured_result": reply.structured_result, "metadata": reply.metadata })
                        .to_string(),
                    ))
                },
                registration,
            );
            let promise = wasm_bindgen_futures::future_to_promise(async move {
                future
                    .await
                    .map_err(|_| js_error("Claude nested tool execution cancelled"))?
            });
            let cancel =
                Closure::wrap(Box::new(move || abort.abort()) as Box<dyn FnMut()>).into_js_value();
            let _ = js_sys::Reflect::set(promise.as_ref(), &JsValue::from_str("cancel"), &cancel);
            promise
        }) as Box<dyn FnMut(String, String, String) -> js_sys::Promise>,
    )
    .into_js_value();
    let promise = host_execute_claude_tool(
        host_definition_id,
        name,
        &input.to_string(),
        &invocation.session_id,
        &invocation.call_id,
        &invocation.model,
        &invocation.turn_id,
        &definitions,
        &callback,
    )
    .map_err(|_| "Claude tool host rejected invocation".to_owned())?;
    let mut pending = PendingTool {
        session_id: &invocation.session_id,
        cancel: js_sys::Reflect::get(promise.as_ref(), &JsValue::from_str("cancel"))
            .ok()
            .and_then(|value| value.dyn_into().ok()),
        settled: false,
    };
    if let Some(progress) = &invocation.progress {
        observe_live_updates(
            host_definition_id,
            &invocation.session_id,
            &invocation.call_id,
            progress,
        )
        .await;
    }
    let response = JsFuture::from(promise).await;
    pending.settled = true;
    let response = response.map_err(|error| {
        if js_sys::Reflect::get(&error, &JsValue::from_str("code"))
            .ok()
            .and_then(|value| value.as_string())
            .as_deref()
            == Some("host_interrupted")
        {
            ClaudeTools::HOST_INTERRUPTED.to_owned()
        } else {
            "Claude tool host invocation failed".to_owned()
        }
    })?;
    let response = response
        .as_string()
        .ok_or_else(|| "Claude tool host must return a JSON string".to_owned())?;
    let reply: HostToolReply = serde_json::from_str(&response)
        .map_err(|_| "Claude tool host returned an invalid Claude reply".to_owned())?;
    Ok(ClaudeToolReply {
        content: reply.content,
        is_error: reply.is_error,
        metadata: reply.metadata,
        structured_result: reply.structured_result,
    })
}

/// Opt-in Claude-native WASM lifecycle. Authentication remains host-owned.
#[wasm_bindgen(js_name = Nanoclaude)]
pub struct WasmNanoclaude {
    inner: RustNanocodex,
    event_forwarding: Rc<Cell<bool>>,
    turns: RefCell<Vec<Weak<RefCell<TurnState>>>>,
    subagents: Option<WasmSubagents>,
}

#[wasm_bindgen(js_class = Nanoclaude)]
impl WasmNanoclaude {
    /// Creates only explicitly supplied Claude capabilities.
    pub async fn create(config_json: &str) -> Result<Self, JsValue> {
        // Serde errors can include caller-supplied strings; do not echo config secrets.
        let config: ClaudeConfig = serde_json::from_str(config_json)
            .map_err(|_| js_error("invalid Nanoclaude configuration"))?;
        let (factory, subagents) = if let Some(settings) = &config.subagents {
            let host = config
                .host_definition_id
                .ok_or_else(|| js_error("subagents require hostDefinitionId"))?;
            let (registry, control, updates) =
                nanocodex_subagents::channel(settings.max_concurrency);
            if config.subagent_routing {
                registry.set_spawn_router(Arc::new(JavaScriptSpawnRouter {
                    host_definition_id: host,
                }));
            }
            let parents = Arc::new(Mutex::new(HashMap::new()));
            let codex = config
                .codex_harness
                .clone()
                .map(|recipe| {
                    let key = recipe
                        .get("api_key")
                        .and_then(Value::as_str)
                        .ok_or_else(|| js_error("Codex harness requires explicit transport"))?;
                    let auth = nanocodex::oai::auth::OpenAiAuth::api_key(key.to_owned());
                    Ok::<_, JsValue>((recipe, auth))
                })
                .transpose()?;
            let factory = Arc::new(WasmHarnessFactory {
                registry: registry.clone(),
                parents: parents.clone(),
                hosts: Arc::new(Mutex::new(HashMap::new())),
                codex,
                claude: Some(serde_json::to_value(&config).map_err(js_error)?),
            });
            let subagents = WasmSubagents::new(
                host,
                registry,
                control,
                updates,
                parents,
                factory.hosts.clone(),
            );
            (Some(factory), Some(subagents))
        } else {
            (None, None)
        };
        let (inner, events) = build_claude(config, factory, None, None).await?;
        let event_forwarding = Rc::new(Cell::new(false));
        forward_events(events, Rc::clone(&event_forwarding));
        Ok(Self {
            inner,
            event_forwarding,
            turns: RefCell::new(Vec::new()),
            subagents,
        })
    }

    #[wasm_bindgen(getter, js_name = sessionId)]
    pub fn session_id(&self) -> String {
        self.inner.session_id().to_owned()
    }

    #[wasm_bindgen(getter, js_name = agentId)]
    pub fn agent_id(&self) -> String {
        self.inner.agent_id().to_owned()
    }

    #[wasm_bindgen(js_name = setEventForwarding)]
    pub fn set_event_forwarding(&self, enabled: bool) {
        if self.event_forwarding.replace(enabled) != enabled
            && let Some(subagents) = &self.subagents
        {
            subagents.set_event_forwarding(enabled);
        }
    }

    /// Accepts text using the shared Turn/TurnResult and durable request-ID path.
    pub fn prompt(
        &self,
        input: &str,
        request_id: Option<String>,
        cancel_on_admission: Option<bool>,
    ) -> Result<WasmTurn, JsValue> {
        validate_operation_id(request_id.as_deref())?;
        if input.trim().is_empty() {
            return Err(js_error("prompt input must not be empty"));
        }
        let turn = WasmTurn::accept(
            self.inner.clone(),
            Prompt::new(input),
            request_id,
            cancel_on_admission.unwrap_or(false),
        );
        self.track(&turn);
        Ok(turn)
    }

    /// Accepts ordered multimodal JSON content: text, image URLs or data URLs,
    /// and inline `file` documents (PDF or plain text). Local paths are rejected.
    #[wasm_bindgen(js_name = promptContent)]
    pub fn prompt_content(
        &self,
        content_json: &str,
        request_id: Option<String>,
        cancel_on_admission: Option<bool>,
    ) -> Result<WasmTurn, JsValue> {
        validate_operation_id(request_id.as_deref())?;
        let turn = WasmTurn::accept(
            self.inner.clone(),
            super::parse_browser_prompt(content_json)?,
            request_id,
            cancel_on_admission.unwrap_or(false),
        );
        self.track(&turn);
        Ok(turn)
    }

    /// Atomically steers the active Claude turn or starts a new one, for live
    /// frontends such as realtime voice. Returns `undefined` when steered.
    #[wasm_bindgen(js_name = routePrompt)]
    pub async fn route_prompt(&self, instruction: &str) -> Result<Option<WasmTurn>, JsValue> {
        if instruction.trim().is_empty() {
            return Err(js_error("prompt instruction must not be empty"));
        }
        match self
            .inner
            .route_prompt(Prompt::new(instruction))
            .await
            .map_err(js_error)?
        {
            PromptRoute::Steered => Ok(None),
            PromptRoute::Started(turn) => {
                let turn = WasmTurn::started(turn);
                self.track(&turn);
                Ok(Some(turn))
            }
        }
    }

    fn track(&self, turn: &WasmTurn) {
        let mut turns = self.turns.borrow_mut();
        turns.retain(|turn| {
            turn.upgrade()
                .is_some_and(|state| state.borrow().completed.is_none())
        });
        turns.push(Rc::downgrade(&turn.state));
    }

    pub async fn compact(&self) -> Result<(), JsValue> {
        self.inner.compact().await.map_err(js_error)
    }

    /// Cancels nonterminal prompts issued by this handle (not an in-flight compact).
    pub async fn cancel(&self) -> Result<(), JsValue> {
        let pending: Vec<_> = self
            .turns
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|state| state.borrow().completed.is_none())
            .collect();
        for state in pending {
            let turn = WasmTurn { state };
            match turn.control().await {
                Ok(control) => control.cancel().await.map_err(js_error)?,
                Err(error) if turn.state.borrow().completed.is_none() => {
                    return Err(js_error(error));
                }
                Err(_) => {} // Completion can race cancellation.
            }
        }
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), JsValue> {
        if let Some(subagents) = &self.subagents {
            subagents
                .close_all(self.inner.session_id())
                .await
                .map_err(js_error)?;
        }
        self.inner.shutdown().await.map_err(js_error)?;
        self.set_event_forwarding(false);
        Ok(())
    }

    #[wasm_bindgen(js_name = spawnSubagent)]
    pub async fn spawn_subagent(&self, task: &str) -> Result<String, JsValue> {
        self.subagents
            .as_ref()
            .ok_or_else(|| js_error("this agent was not created with the subagent extension"))?
            .spawn_subagent(self.inner.session_id(), task)
            .await
    }
    #[wasm_bindgen(js_name = waitSubagents)]
    pub async fn wait_subagents(&self, task: &str) -> Result<String, JsValue> {
        self.subagents
            .as_ref()
            .ok_or_else(|| js_error("this agent was not created with the subagent extension"))?
            .wait_subagents(self.inner.session_id(), task)
            .await
    }
    #[wasm_bindgen(js_name = listSubagents)]
    pub async fn list_subagents(&self, task: &str) -> Result<String, JsValue> {
        self.subagents
            .as_ref()
            .ok_or_else(|| js_error("this agent was not created with the subagent extension"))?
            .list_subagents(self.inner.session_id(), task)
            .await
    }
    #[wasm_bindgen(js_name = sendSubagentMessage)]
    pub async fn send_subagent_message(&self, task: &str) -> Result<String, JsValue> {
        self.subagents
            .as_ref()
            .ok_or_else(|| js_error("this agent was not created with the subagent extension"))?
            .send_subagent_message(self.inner.session_id(), task)
            .await
    }
    #[wasm_bindgen(js_name = interruptSubagent)]
    pub async fn interrupt_subagent(&self, task: &str) -> Result<String, JsValue> {
        self.subagents
            .as_ref()
            .ok_or_else(|| js_error("this agent was not created with the subagent extension"))?
            .interrupt_subagent(self.inner.session_id(), task)
            .await
    }
    #[wasm_bindgen(js_name = closeSubagent)]
    pub async fn close_subagent(&self, task: &str) -> Result<String, JsValue> {
        self.subagents
            .as_ref()
            .ok_or_else(|| js_error("this agent was not created with the subagent extension"))?
            .close_subagent(self.inner.session_id(), task)
            .await
    }
    #[wasm_bindgen(js_name = spawnSubagents)]
    pub async fn spawn_subagents(&self, tasks_json: &str) -> Result<String, JsValue> {
        self.subagents
            .as_ref()
            .ok_or_else(|| js_error("this agent was not created with the subagent extension"))?
            .spawn_subagents(self.inner.session_id(), tasks_json)
            .await
    }

    /// Claude checkpoints are stored natively by durability, not OpenAI snapshots.
    pub fn snapshot(&self) -> Result<String, JsValue> {
        Err(js_error(
            "Claude snapshot export is unsupported; reopen the configured durabilityId",
        ))
    }

    pub fn checkpoint(&self) -> Result<String, JsValue> {
        Err(js_error(
            "Claude checkpoint export is unsupported; checkpoints are managed by durability",
        ))
    }
}

impl Drop for WasmNanoclaude {
    fn drop(&mut self) {
        if self.event_forwarding.replace(false)
            && let Some(subagents) = &self.subagents
        {
            subagents.set_event_forwarding(false);
        }
        if let Some(subagents) = &self.subagents
            && subagents.remove_parent(self.inner.session_id())
        {
            let subagents = subagents.clone();
            let session_id = self.inner.session_id().to_owned();
            wasm_bindgen_futures::spawn_local(async move {
                let _ = subagents.close_all(&session_id).await;
            });
        }
    }
}

pub(super) async fn build_claude(
    config: ClaudeConfig,
    factory: Option<Arc<WasmHarnessFactory>>,
    snapshot: Option<nanocodex_agent::ChildSnapshot>,
    host_context: Option<Arc<str>>,
) -> Result<(RustNanocodex, AgentEvents), JsValue> {
    config.validate().map_err(js_error)?;
    let endpoint = config.endpoint.unwrap_or_else(|| {
        if config.subscription_compatibility {
            nanocodex_claude::ANTHROPIC_SUBSCRIPTION_MESSAGES_URL.to_owned()
        } else {
            nanocodex_claude::ANTHROPIC_MESSAGES_URL.to_owned()
        }
    });
    let http = reqwest::Client::new();
    let mut client = match (config.api_key, config.auth_host_id) {
        (Some(key), None) => ClaudeClient::new(http, endpoint, key),
        (None, Some(auth_host_id)) => ClaudeClient::with_auth_provider(
            http,
            endpoint,
            Arc::new(JavaScriptClaudeAuth { auth_host_id }),
        ),
        _ => return Err(js_error("invalid explicit Claude authentication")),
    };
    if config.subscription_compatibility {
        client = client.subscription_compatibility();
        if let Some(identity) = config.subscription_identity {
            identity
                .validate()
                .map_err(|_| js_error("invalid subscription identity"))?;
            client = client.with_subscription_identity(identity);
        }
    } else if config.subscription_identity.is_some() {
        return Err(js_error(
            "subscription identity requires subscription compatibility",
        ));
    }
    let builder_model = config.model.clone();
    let mut builder = RustNanocodex::builder(Claude::new(client, config.model))
        .parallel_tools(config.parallel_tools)
        .parallel_safe_tools(config.parallel_safe_tools);
    if let Some(session_id) = config.session_id {
        builder = builder.session_id(session_id);
    }
    if let Some(tokens) = config.max_tokens {
        builder = builder.max_tokens(tokens);
    }
    if let Some(effort) = config.thinking {
        if builder_model
            .parse::<nanocodex_agent::HarnessModel>()
            .is_ok()
        {
            builder = builder.thinking(effort).map_err(js_error)?;
        } else if effort != super::Thinking::None {
            let native = serde_json::from_value(serde_json::to_value(effort).map_err(js_error)?)
                .map_err(js_error)?;
            builder = builder.effort(native);
        }
    }
    if config.adaptive_thinking {
        builder = builder.adaptive_thinking();
    }
    if config.keep_thinking {
        builder = builder.keep_thinking();
    }
    builder = match config.cache {
        CachePolicy::Off => builder,
        CachePolicy::FiveMinutes => builder.automatic_cache(true),
        CachePolicy::OneHour => builder.cache_one_hour(),
    };
    if let Some(tokens) = config.context_window_tokens {
        builder = builder.context_window_tokens(tokens);
    }
    if let Some(tokens) = config.auto_compact_window_tokens {
        builder = builder.auto_compact_window_tokens(tokens);
    }
    if let Some(instructions) = config.instructions {
        builder = builder.system(instructions);
    }
    if let Some(blocks) = config.system_blocks {
        builder = builder.system_blocks(blocks);
    }
    if let Some(workspace) = config.workspace {
        builder = builder.workspace(workspace);
    }
    builder = builder.code_only(true);
    let code_definitions = if factory.is_some() {
        config.tools.clone()
    } else {
        Vec::new()
    };
    for definition in config.tools.into_iter().filter(|_| factory.is_none()) {
        let host_id = config
            .host_definition_id
            .ok_or_else(|| js_error("explicit Claude tools require hostDefinitionId"))?;
        let name = definition.name.clone();
        builder = builder.tool_with_context(definition, move |input, invocation| {
            let name = name.clone();
            async move { execute_tool(host_id, &name, input, invocation, None).await }
        });
    }
    if let (Some(route_id), Some(state_id)) = (config.durability_host_id, config.durability_id) {
        let store = JavaScriptDurabilityStore { route_id };
        let durable = if let Some(limit) = config.terminal_receipt_retention {
            nanocodex::agent::durability::DurableSession::open_with_terminal_receipt_limit(
                store, state_id, limit,
            )
            .await
        } else {
            nanocodex::agent::durability::DurableSession::open(store, state_id).await
        }
        .map_err(js_error)?;
        builder = builder.durability(durable).await.map_err(js_error)?;
    }
    if let Some(factory) = factory {
        let host = config
            .host_definition_id
            .ok_or_else(|| js_error("subagents require hostDefinitionId"))?;
        let registry = factory.registry.clone();
        let parents = factory.parents.clone();
        builder = builder
            .spawn_factory(factory.clone())
            .tools_factory(move |agent| {
                let agent = agent.with_spawn_factory(factory.clone());
                factory
                    .hosts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(agent.session_id().to_owned(), host);
                parents
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(agent.session_id().to_owned(), agent.clone());
                let native = nanocodex_subagents::install_claude_tools(
                    ClaudeTools::new(),
                    agent,
                    registry.clone(),
                )?;
                let mut tools = ClaudeTools::new();
                for definition in &code_definitions {
                    let name = definition.name.clone();
                    let native = native.clone();
                    tools =
                        tools.tool_with_context(definition.clone(), move |input, invocation| {
                            let name = name.clone();
                            let native = native.clone();
                            async move {
                                execute_tool(host, &name, input, invocation, Some(native)).await
                            }
                        });
                }
                Ok(tools)
            });
    }
    builder = builder.host_context(host_context);
    if let Some(snapshot) = snapshot {
        builder = builder.restore_runtime(snapshot).map_err(js_error)?;
    }
    builder.build().map_err(js_error)
}
