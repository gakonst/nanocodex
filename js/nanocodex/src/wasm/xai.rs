use super::{
    AgentEvents, Cell, DurableAgentExt, HashMap, JavaScriptDurabilityStore, JavaScriptSpawnRouter,
    JsFuture, JsValue, Mutex, Prompt, Rc, RefCell, RustNanocodex, TurnState, WasmHarnessFactory,
    WasmSubagents, WasmSubagentsConfig, WasmTurn, forward_events, host_cancel_code_turn, js_error,
    validate_operation_id,
};
use nanocodex_xai::{
    ToolDefinition, Xai, XaiAuthFuture, XaiAuthProvider, XaiAuthUnavailable, XaiClient,
    XaiToolInvocation, XaiToolReply,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, rc::Weak, sync::Arc};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = xaiAuth)]
    fn host_xai_auth(auth_host_id: u32) -> Result<js_sys::Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = executeXaiTool)]
    fn host_execute_xai_tool(
        host_definition_id: u32,
        name: &str,
        input: &str,
        session_id: &str,
        call_id: &str,
        model: &str,
        turn_id: &str,
    ) -> Result<js_sys::Promise, JsValue>;
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct XaiConfig {
    model: String,
    session_id: Option<String>,
    api_key: Option<String>,
    auth_host_id: Option<u32>,
    endpoint: Option<String>,
    host_definition_id: Option<u32>,
    #[serde(default)]
    tools: Vec<ToolDefinition>,
    #[serde(default)]
    server_tools: Vec<Value>,
    thinking: Option<super::Thinking>,
    context_window_tokens: Option<u64>,
    auto_compact_threshold_percent: Option<u32>,
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    repetition_limit: Option<usize>,
    compaction_keep_tail: Option<usize>,
    request_timeout_ms: Option<u64>,
    instructions: Option<String>,
    workspace: Option<String>,
    durability_host_id: Option<String>,
    durability_id: Option<String>,
    terminal_receipt_retention: Option<usize>,
    subagents: Option<WasmSubagentsConfig>,
    #[serde(default)]
    subagent_routing: bool,
    codex_harness: Option<Value>,
    claude_harness: Option<Value>,
}

impl XaiConfig {
    fn validate(&self) -> Result<(), &'static str> {
        if self.model.trim().is_empty() {
            return Err("Xai model must not be empty");
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
            return Err("durable Xai sessionId must equal durabilityId");
        }
        match (&self.api_key, self.auth_host_id) {
            (Some(key), None) if !key.trim().is_empty() => {}
            (None, Some(_)) => {}
            _ => return Err("supply exactly one nonempty apiKey or authHostId"),
        }
        if !self.tools.is_empty() && self.host_definition_id.is_none() {
            return Err("explicit Xai tools require hostDefinitionId");
        }
        if self.context_window_tokens == Some(0)
            || self.max_steps == Some(0)
            || self.repetition_limit == Some(0)
            || self.request_timeout_ms == Some(0)
        {
            return Err("Xai limits must be positive");
        }
        if self
            .auto_compact_threshold_percent
            .is_some_and(|value| !(1..=100).contains(&value))
        {
            return Err("autoCompactThresholdPercent must be 1..100");
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
                "endpoint must be an explicit HTTP(S) Responses URL without userinfo or fragment",
            );
        }
        Ok(())
    }
}

struct JavaScriptXaiAuth {
    auth_host_id: u32,
}

impl XaiAuthProvider for JavaScriptXaiAuth {
    fn headers(&self) -> XaiAuthFuture<'_, Result<reqwest::header::HeaderMap, XaiAuthUnavailable>> {
        Box::pin(async move {
            let promise = host_xai_auth(self.auth_host_id).map_err(|_| XaiAuthUnavailable)?;
            let result = JsFuture::from(promise)
                .await
                .map_err(|_| XaiAuthUnavailable)?;
            let encoded = result.as_string().ok_or(XaiAuthUnavailable)?;
            let headers: BTreeMap<String, String> =
                serde_json::from_str(&encoded).map_err(|_| XaiAuthUnavailable)?;
            if headers.is_empty() {
                return Err(XaiAuthUnavailable);
            }
            let mut output = reqwest::header::HeaderMap::new();
            for (name, value) in headers {
                let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| XaiAuthUnavailable)?;
                let mut value = reqwest::header::HeaderValue::from_str(&value)
                    .map_err(|_| XaiAuthUnavailable)?;
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
    output: Value,
    success: bool,
    metadata: Option<Value>,
    structured_result: Option<Value>,
}

async fn execute_tool(
    host_definition_id: u32,
    name: &str,
    input: Value,
    invocation: XaiToolInvocation,
) -> Result<XaiToolReply, String> {
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
    let promise = host_execute_xai_tool(
        host_definition_id,
        name,
        &input.to_string(),
        &invocation.session_id,
        &invocation.call_id,
        &invocation.model,
        &invocation.turn_id,
    )
    .map_err(|_| "Xai tool host rejected invocation".to_owned())?;
    let mut pending = PendingTool {
        session_id: &invocation.session_id,
        cancel: js_sys::Reflect::get(promise.as_ref(), &JsValue::from_str("cancel"))
            .ok()
            .and_then(|value| value.dyn_into().ok()),
        settled: false,
    };
    let response = JsFuture::from(promise).await;
    pending.settled = true;
    let response = response.map_err(|_| "Xai tool host invocation failed".to_owned())?;
    let response = response
        .as_string()
        .ok_or_else(|| "Xai tool host must return a JSON string".to_owned())?;
    let reply: HostToolReply = serde_json::from_str(&response)
        .map_err(|_| "Xai tool host returned an invalid Xai reply".to_owned())?;
    let success = reply.success;
    let metadata = reply.metadata;
    let structured_result = reply.structured_result;
    let mut reply = match reply.output {
        Value::String(text) => XaiToolReply::text(text),
        Value::Array(content) => {
            let content = serde_json::from_value(Value::Array(content))
                .map_err(|_| "Xai tool host returned invalid native content".to_owned())?;
            XaiToolReply {
                text: String::new(),
                content,
                is_error: false,
                metadata: None,
                structured_result: None,
            }
        }
        _ => return Err("Xai tool host returned invalid output".to_owned()),
    };
    reply.is_error = !success;
    reply.metadata = metadata;
    reply.structured_result = structured_result;
    Ok(reply)
}

/// Opt-in Xai-native WASM lifecycle. Authentication remains host-owned.
#[wasm_bindgen(js_name = Nanoxai)]
pub struct WasmNanoxai {
    inner: RustNanocodex,
    event_forwarding: Rc<Cell<bool>>,
    turns: RefCell<Vec<Weak<RefCell<TurnState>>>>,
    subagents: Option<WasmSubagents>,
}

#[wasm_bindgen(js_class = Nanoxai)]
impl WasmNanoxai {
    /// Creates only explicitly supplied Xai capabilities.
    pub async fn create(config_json: &str) -> Result<Self, JsValue> {
        // Serde errors can include caller-supplied strings; do not echo config secrets.
        let config: XaiConfig = serde_json::from_str(config_json)
            .map_err(|_| js_error("invalid Nanoxai configuration"))?;
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
                claude: config.claude_harness.clone(),
                xai: Some(serde_json::to_value(&config).map_err(js_error)?),
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
        let (inner, events) = build_xai(config, factory, None, None).await?;
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
        let mut turns = self.turns.borrow_mut();
        turns.retain(|turn| {
            turn.upgrade()
                .is_some_and(|state| state.borrow().completed.is_none())
        });
        turns.push(Rc::downgrade(&turn.state));
        Ok(turn)
    }

    pub async fn context(&self) -> Result<String, JsValue> {
        super::serialize_session_context(self.inner.context().await.map_err(js_error)?)
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

    /// Xai checkpoints are stored natively by durability, not OpenAI snapshots.
    pub fn snapshot(&self) -> Result<String, JsValue> {
        Err(js_error(
            "Xai snapshot export is unsupported; reopen the configured durabilityId",
        ))
    }

    pub fn checkpoint(&self) -> Result<String, JsValue> {
        Err(js_error(
            "Xai checkpoint export is unsupported; checkpoints are managed by durability",
        ))
    }
}

impl Drop for WasmNanoxai {
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

pub(super) async fn build_xai(
    config: XaiConfig,
    factory: Option<Arc<WasmHarnessFactory>>,
    snapshot: Option<nanocodex_agent::ChildSnapshot>,
    host_context: Option<Arc<str>>,
) -> Result<(RustNanocodex, AgentEvents), JsValue> {
    config.validate().map_err(js_error)?;
    let endpoint = config
        .endpoint
        .unwrap_or_else(|| "https://api.x.ai/v1/responses".to_owned());
    let http = reqwest::Client::new();
    let client = match (config.api_key, config.auth_host_id) {
        (Some(key), None) => XaiClient::new(http, endpoint, key),
        (None, Some(auth_host_id)) => XaiClient::with_auth_provider(
            http,
            endpoint,
            Arc::new(JavaScriptXaiAuth { auth_host_id }),
        ),
        _ => return Err(js_error("invalid explicit Xai authentication")),
    };
    let mut builder = RustNanocodex::builder(Xai::new(client, config.model));
    if let Some(session_id) = config.session_id {
        builder = builder.session_id(session_id);
    }
    if let Some(effort) = config.thinking {
        builder = builder.thinking(effort);
    }
    if let Some(tokens) = config.context_window_tokens {
        builder = builder.context_window_tokens(tokens);
    }
    if let Some(percent) = config.auto_compact_threshold_percent {
        builder = builder.auto_compact_threshold_percent(percent);
    }
    if let Some(steps) = config.max_steps {
        builder = builder.max_steps(steps);
    }
    if let Some(retries) = config.max_retries {
        builder = builder.max_retries(retries);
    }
    if let Some(limit) = config.repetition_limit {
        builder = builder.repetition_limit(limit);
    }
    if let Some(items) = config.compaction_keep_tail {
        builder = builder.compaction_keep_tail(items);
    }
    if let Some(ms) = config.request_timeout_ms {
        builder = builder.request_timeout(std::time::Duration::from_millis(ms));
    }
    if let Some(instructions) = config.instructions {
        builder = builder.system(instructions);
    }
    if let Some(workspace) = config.workspace {
        builder = builder.workspace(workspace);
    }
    for definition in config.tools {
        let host_id = config
            .host_definition_id
            .ok_or_else(|| js_error("explicit Xai tools require hostDefinitionId"))?;
        let name = definition.name.clone();
        builder = builder.tool_with_context(definition, move |input, invocation| {
            let name = name.clone();
            async move { execute_tool(host_id, &name, input, invocation).await }
        });
    }
    for definition in config.server_tools {
        builder = builder.hosted_tool(definition).map_err(js_error)?;
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
                nanocodex_subagents::install_xai_tools(
                    nanocodex_xai::XaiTools::new(),
                    agent,
                    registry.clone(),
                )
            });
    }
    builder = builder.host_context(host_context);
    if let Some(snapshot) = snapshot {
        builder = builder.restore_runtime(snapshot).map_err(js_error)?;
    }
    builder.build().map_err(js_error)
}
