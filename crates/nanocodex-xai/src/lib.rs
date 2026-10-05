//! Portable adaptation of Grok Build's Responses sampling and conversation core.
//!
//! Host applications supply credentials and tools. The shared Nanocodex lifecycle
//! wraps the xAI-native transcript; no Claude Messages conversion is performed.
//! See `UPSTREAM.md` for the pinned source and deliberate adaptation boundaries.
use futures_util::{FutureExt, StreamExt};
use nanocodex_agent::{
    AgentEvents, AgentHandle, AgentSessionContext, ChildSnapshot, CostStatus, HarnessFamily,
    HarnessModel, Model, Nanocodex, NanocodexError, ReportedTurnUsage, Result, SpawnOptions,
    Thinking, TurnResult, TurnUsage,
    backend::{
        AgentFactory, BackendFuture, BackendPrompt, BackendPromptRoute, BackendRuntime,
        BackendTurn, BackendTurnKey, BuilderBackend, LifecycleBackend,
    },
    events::{AgentEvent, AgentEventKind, AgentEventPublisher},
    input::{Prompt, PromptInput, PromptMessageRole},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, Notify, oneshot};
mod compaction;
mod conversation;
mod lifecycle;
mod stream;
pub use lifecycle::{XaiAuthFuture, XaiAuthProvider, XaiAuthUnavailable};
pub mod durable;
mod tools;
use tools::ToolHandler as Handler;
pub use tools::{ToolDefinition, XaiToolInvocation, XaiToolReply, XaiTools};
use web_time::Instant;

#[derive(Default)]
struct RunStats {
    model_calls: u32,
    tool_calls: u32,
    model_ns: u64,
    tool_ns: u64,
    usage: Option<TurnUsage>,
    steers: u32,
    compactions: u32,
    retries: u32,
}
fn elapsed_ns(start: Instant) -> u64 {
    start.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

/// Official public source revision from which this implementation is adapted.
pub const UPSTREAM_REVISION: &str = "2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8";
/// Monorepo revision recorded by upstream in SOURCE_REV.
pub const UPSTREAM_SOURCE_REVISION: &str = "559751fdcec02d413e4c57c8832ab275e4f44980";

/// Explicit xAI transport. Debug intentionally does not expose the credential.
#[derive(Clone)]
pub struct XaiClient {
    http: reqwest::Client,
    endpoint: String,
    key: Arc<str>,
    auth: Option<Arc<dyn XaiAuthProvider>>,
}
impl XaiClient {
    /// Creates a client for a complete Responses URL (normally
    /// `https://api.x.ai/v1/responses`). The caller owns credential acquisition.
    pub fn new(
        http: reqwest::Client,
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            http,
            endpoint: endpoint.into(),
            key: Arc::from(api_key.into()),
            auth: None,
        }
    }
}
/// Concrete xAI harness recipe and builder.
#[derive(Clone)]
pub struct Xai {
    client: XaiClient,
    model: String,
    thinking: Thinking,
    system: String,
    tools: HashMap<String, (ToolDefinition, Handler)>,
    hosted: Vec<Value>,
    max_steps: usize,
    timeout: Duration,
    session_id: Option<String>,
    workspace: String,
    host_context: Option<Arc<str>>,
    tools_factory: Option<Arc<dyn Fn(AgentHandle) -> Result<XaiTools> + Send + Sync>>,
    spawn_factory: Option<Arc<dyn AgentFactory>>,
    restored_history: Option<Vec<Value>>,
    context_window_tokens: u64,
    compact_percent: u32,
    keep_tail: usize,
    max_retries: usize,
    repetition_limit: usize,
    policy: Option<Arc<dyn durable::XaiExecutionPolicy>>,
    checkpoint: Option<Value>,
    build_guard: Arc<AtomicBool>,
}
impl BuilderBackend for Xai {
    type Builder = Self;
    fn into_builder(self) -> Self {
        self
    }
}
impl Xai {
    /// Starts a native xAI recipe using an explicit provider model identifier.
    pub fn new(client: XaiClient, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            thinking: Thinking::High,
            system: String::new(),
            tools: HashMap::new(),
            hosted: Vec::new(),
            max_steps: 32,
            timeout: Duration::from_secs(300),
            session_id: None,
            workspace: String::new(),
            host_context: None,
            tools_factory: None,
            spawn_factory: None,
            restored_history: None,
            context_window_tokens: 500_000,
            compact_percent: 0,
            keep_tail: 8,
            max_retries: 3,
            repetition_limit: 3,
            policy: None,
            checkpoint: None,
            build_guard: Arc::new(AtomicBool::new(false)),
        }
    }
    /// Sets the system instruction prepended to the native conversation.
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = system.into();
        self
    }
    /// Sets reasoning effort from the pinned upstream model catalog.
    pub const fn thinking(mut self, thinking: Thinking) -> Self {
        self.thinking = thinking;
        self
    }
    /// Bounds the number of model calls per submitted turn.
    pub const fn max_steps(mut self, steps: usize) -> Self {
        self.max_steps = steps;
        self
    }
    /// Bounds a complete HTTP sampling call, including its event stream.
    pub const fn request_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    /// Registers an application-authorized native function.
    #[cfg(not(target_family = "wasm"))]
    pub fn tool<F, Fut>(mut self, definition: ToolDefinition, callback: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = std::result::Result<String, String>> + Send + 'static,
    {
        self.tools.insert(
            definition.name.clone(),
            (
                definition,
                Arc::new(move |v, _| {
                    let future = callback(v);
                    Box::pin(async move { future.await.map(XaiToolReply::text) })
                }),
            ),
        );
        self
    }
    /// Registers an isolate-local application-authorized native function.
    #[cfg(target_family = "wasm")]
    pub fn tool<F, Fut>(mut self, definition: ToolDefinition, callback: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = std::result::Result<String, String>> + 'static,
    {
        self.tools.insert(
            definition.name.clone(),
            (
                definition,
                Arc::new(move |v, _| {
                    let future = callback(v);
                    Box::pin(async move { future.await.map(XaiToolReply::text) })
                }),
            ),
        );
        self
    }
    /// Enables xAI-hosted web search. Its calls stay server-side and are replayed
    /// as native context; they never execute an application callback.
    pub fn web_search(mut self) -> Self {
        self.hosted.push(json!({"type":"web_search"}));
        self
    }
    /// Enables xAI-hosted X search without registering a local callback.
    pub fn x_search(mut self) -> Self {
        self.hosted.push(json!({"type":"x_search"}));
        self
    }
    /// Builds an in-memory lifecycle. Credentials are used only when prompting.
    pub fn build(mut self) -> Result<(Nanocodex, AgentEvents)> {
        if self.policy.is_some() && self.build_guard.swap(true, Ordering::SeqCst) {
            return Err(invalid(
                "durable xAI recipe already built; reopen its execution policy",
            ));
        }
        validate_effort(&self.model, self.thinking)?;
        if self.model.trim().is_empty() || self.max_steps == 0 {
            return Err(invalid("xAI requires a model and max_steps > 0"));
        }
        let url = reqwest::Url::parse(&self.client.endpoint).map_err(error)?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(invalid(
                "xAI endpoint must be HTTP(S) without URL credentials",
            ));
        }
        let session = self
            .policy
            .as_ref()
            .map(|p| p.state_id().to_owned())
            .or_else(|| self.session_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let factory = Arc::new(lifecycle::NativeFactory {
            state: Mutex::new(Weak::new()),
        });
        let mut handle = AgentHandle::new(
            Arc::<str>::from(session.as_str()),
            self.model
                .parse()
                .unwrap_or_else(|_| HarnessFamily::Xai.default_model()),
            factory.clone(),
        )
        .with_native_model_id(self.model.as_str());
        if let Some(f) = &self.spawn_factory {
            handle = handle.with_spawn_factory(f.clone());
        }
        if let Some(f) = &self.tools_factory {
            self.tools.extend(f(handle.clone())?.tools);
        }
        let mut unique = HashMap::<String, Value>::new();
        let mut hosted = Vec::new();
        for definition in std::mem::take(&mut self.hosted) {
            let kind = definition["type"].as_str().unwrap_or_default().to_owned();
            if let Some(prior) = unique.get(&kind) {
                if prior != &definition {
                    return Err(invalid(format!(
                        "conflicting xAI-hosted tool definitions for {kind}"
                    )));
                }
            } else {
                unique.insert(kind, definition.clone());
                hosted.push(definition);
            }
        }
        self.hosted = hosted;
        validate_hosted(&self.model, &self.hosted)?;
        for hosted in &self.hosted {
            if let Some(name) = hosted["type"].as_str()
                && self.tools.contains_key(name)
            {
                return Err(invalid(format!(
                    "host function {name} conflicts with the xAI-hosted tool"
                )));
            }
        }
        for (definition, _) in self.tools.values() {
            if definition.name.trim().is_empty() || !definition.parameters.is_object() {
                return Err(invalid(
                    "xAI functions require a name and an object JSON schema",
                ));
            }
        }
        if self.context_window_tokens == 0
            || self.compact_percent > 100
            || self.repetition_limit == 0
        {
            return Err(invalid("invalid xAI context/recovery limits"));
        }
        let (runtime, events) = BackendRuntime::new(session.clone());
        let history = if let Some(history) = self.restored_history.take() {
            history
        } else if self.system.is_empty() {
            Vec::new()
        } else {
            vec![json!({"type":"message","role":"system","content":self.system})]
        };
        let driver = Driver {
            state: Arc::new(State {
                config: Mutex::new(self),
                history: AsyncMutex::new(history),
                active: Mutex::new(None),
                stopped: AtomicBool::new(false),
                seq: AtomicU64::new(1),
                session,
                events: runtime.events(),
                steering: Mutex::new(Vec::new()),
                steer_ids: Mutex::new(HashSet::new()),
                handle,
                last_input_tokens: AtomicU64::new(0),
                checkpoints: Mutex::new(Vec::new()),
                compaction_cancel: Mutex::new(None),
            }),
        };
        *factory.state.lock().unwrap() = Arc::downgrade(&driver.state);
        Ok((runtime.bind(driver), events))
    }
}
fn invalid(message: impl Into<String>) -> NanocodexError {
    NanocodexError::InvalidRequest(message.into())
}
fn error(error: impl std::fmt::Display) -> NanocodexError {
    invalid(format!("xAI: {error}"))
}
fn effort(thinking: Thinking) -> Result<&'static str> {
    match thinking {
        Thinking::Low => Ok("low"),
        Thinking::Medium => Ok("medium"),
        Thinking::High => Ok("high"),
        Thinking::Xhigh => Ok("xhigh"),
        _ => Err(invalid(
            "xAI supports low, medium, high and xhigh reasoning effort",
        )),
    }
}
fn unsupported<T: Send + 'static>(operation: &'static str) -> BackendFuture<Result<T>> {
    Box::pin(async move { Err(invalid(format!("xAI harness does not support {operation}"))) })
}
#[derive(Default)]
struct Cancellation {
    cancelled: AtomicBool,
    notify: Notify,
}
impl Cancellation {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }
    fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::SeqCst) {
            Err(NanocodexError::TurnCancelled)
        } else {
            Ok(())
        }
    }
    async fn wait(&self) {
        loop {
            let wait = self.notify.notified();
            if self.cancelled.load(Ordering::SeqCst) {
                return;
            }
            wait.await;
        }
    }
}
struct Active {
    key: BackendTurnKey,
    cancel: Arc<Cancellation>,
    done: Arc<Notify>,
}
struct State {
    config: Mutex<Xai>,
    history: AsyncMutex<Vec<Value>>,
    active: Mutex<Option<Active>>,
    stopped: AtomicBool,
    seq: AtomicU64,
    session: String,
    events: AgentEventPublisher,
    steering: Mutex<Vec<(Option<String>, Prompt)>>,
    steer_ids: Mutex<HashSet<String>>,
    handle: AgentHandle,
    last_input_tokens: AtomicU64,
    checkpoints: Mutex<Vec<(String, Vec<Value>)>>,
    compaction_cancel: Mutex<Option<Arc<Cancellation>>>,
}
#[derive(Clone)]
struct Driver {
    state: Arc<State>,
}
impl State {
    fn emit(&self, events: &AgentEventPublisher, kind: AgentEventKind, payload: Value) {
        let _ = events.publish(AgentEvent {
            protocol_version: 1,
            request_id: Arc::from(events.request_id()),
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            kind,
            payload: Arc::from(
                serde_json::value::to_raw_value(&payload).expect("JSON value serializes"),
            ),
        });
    }
    fn replay_events(
        &self,
        events: &AgentEventPublisher,
        config: &Xai,
        prompt: &Prompt,
        result: &Result<TurnResult>,
    ) {
        self.emit(events, AgentEventKind::RunStarted,
                        json!({"mode":"xai","model":config.model,"reasoning_mode":"effort",
                            "effort":effort(config.thinking).unwrap_or("high"),"transport":"responses_sse",
                            "orchestration":"grok_build","websocket_url":"","workspace":null,
                            "instruction_bytes":prompt.text_bytes(),"replayed":true}));
        if let Ok(result) = result {
            self.emit(events, AgentEventKind::AssistantMessage,
                            json!({"model_call_index":0,"item_id":null,"phase":null,"text":result.final_message()}));
        }
        let stats = RunStats {
            usage: result.as_ref().ok().and_then(|r| r.usage()).cloned(),
            ..RunStats::default()
        };
        self.emit_terminal(events, config, result, &stats, 0, true);
    }
    fn emit_terminal(
        &self,
        events: &AgentEventPublisher,
        config: &Xai,
        result: &Result<TurnResult>,
        stats: &RunStats,
        duration_ns: u64,
        replayed: bool,
    ) {
        let effort = effort(config.thinking).unwrap_or("high");
        if let Err(err) = result {
            self.emit(
                events,
                AgentEventKind::RunError,
                json!({"message":err.to_string()}),
            );
        }
        self.emit(events,if result.is_ok(){AgentEventKind::RunCompleted}else{AgentEventKind::RunFailed},json!({"status":if result.is_ok(){"completed"}else if matches!(result,Err(NanocodexError::TurnCancelled)){"cancelled"}else{"failed"},"model":config.model,"reasoning_mode":"effort","effort":effort,"transport":"responses_sse","orchestration":"grok_build","duration_ms":duration_ns/1_000_000,"duration_ns":duration_ns,"estimated_cost":null,"cost_usd":null,"cost_status":"other","model_calls":stats.model_calls,"steers":stats.steers,"compactions":stats.compactions,"tool_calls":stats.tool_calls,"connection_attempts":stats.model_calls,"websocket_reconnects":0,"response_attempts":stats.model_calls,"response_retries":stats.retries,"connection_duration_ns":0,"retry_backoff_duration_ns":0,"model_duration_ns":stats.model_ns,"compaction_duration_ns":0,"warmup_duration_ns":0,"tool_work_duration_ns":stats.tool_ns,"tool_wall_duration_ns":stats.tool_ns,"replayed":replayed,"usage":stats.usage.as_ref().map(|usage| serde_json::to_value(usage).unwrap()).unwrap_or_else(||json!({"input_tokens":0,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":0})),"warmup_usage":{"input_tokens":0,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":0}}));
    }

    #[allow(clippy::too_many_arguments)]
    async fn sample(
        &self,
        config: &Xai,
        history: &[Value],
        events: &AgentEventPublisher,
        index: usize,
        cancel: &Cancellation,
        template: Option<&Value>,
        phase: &str,
    ) -> Result<durable::SampleReceipt> {
        let mut tools:Vec<Value>=config.tools.values().map(|(definition,_)|json!({"type":"function","name":definition.name,"description":definition.description,"parameters":definition.parameters})).collect();
        tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        tools.extend(config.hosted.clone());
        let mut input = history.to_vec();
        conversation::patch_reasoning_text_types(&mut input);
        let mut body = json!({"model":config.model,"input":input,"tools":tools,"stream":true,"store":false,
            "reasoning":{"effort":effort(config.thinking)?,"summary":"concise"},"prompt_cache_key":self.session});
        if let Some(template) = template {
            for key in ["model", "tools", "reasoning"] {
                body[key] = template[key].clone();
            }
        }
        let request = async {
            let mut headers = reqwest::header::HeaderMap::new();
            if let Some(auth) = &config.client.auth {
                headers = auth
                    .headers()
                    .await
                    .map_err(|_| invalid("xAI authorization unavailable"))?;
            } else {
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    reqwest::header::HeaderValue::from_str(&format!(
                        "Bearer {}",
                        config.client.key
                    ))
                    .map_err(|_| invalid("invalid xAI credential header"))?,
                );
            }
            let response = config
                .client
                .http
                .post(&config.client.endpoint)
                .headers(headers)
                .header("accept", "text/event-stream")
                .json(&body)
                .send()
                .await
                .map_err(|e| error(e.without_url()))?;
            if !response.status().is_success() {
                let status = response.status();
                let detail = response.text().await.map_err(|e| error(e.without_url()))?;
                let code = serde_json::from_str::<Value>(&detail)
                    .ok()
                    .and_then(|v| v["error"]["code"].as_str().map(str::to_owned))
                    .unwrap_or_default();
                return Ok(durable::SampleReceipt::Rejected {
                    message: format!("xAI Responses HTTP {status}; code={code}"),
                });
            }
            let mut bytes = response.bytes_stream();
            let mut decoder = stream::Decoder::default();
            let mut rejection_only = true;
            while let Some(chunk) = bytes.next().await {
                for event in decoder
                    .push(&chunk.map_err(|e| error(e.without_url()))?)
                    .map_err(invalid)?
                {
                    self.emit(events,AgentEventKind::ApiEvent,json!({"provider":"xai","direction":"received","transport":"responses_sse","phase":phase,"model_call_index":index,"event":event}));
                    match event["type"].as_str() {
                        Some("response.output_text.delta") if phase == "generation"=>self.emit(events,AgentEventKind::AssistantDelta,json!({"model_call_index":index,"item_id":event["item_id"],"phase":null,"text":event["delta"]})),
                        Some("response.reasoning_summary_text.delta"|"response.reasoning_text.delta") if phase == "generation"=>self.emit(events,AgentEventKind::ReasoningSummaryDelta,json!({"model_call_index":index,"text":event["delta"]})),
                        _=>{},
                    }
                    // A terminal's empty output does not erase already observed
                    // output or hosted effects. Only status-only preludes permit
                    // prompt-limit recovery; unknown events fail closed too.
                    rejection_only &= matches!(
                        event["type"].as_str(),
                        Some(
                            "response.created"
                                | "response.queued"
                                | "response.in_progress"
                                | "response.incomplete"
                        )
                    ) && !event["response"]["output"]
                        .as_array()
                        .is_some_and(|output| !output.is_empty());
                    if rejection_only
                        && event["type"] == "response.incomplete"
                        && event["response"]["incomplete_details"]["reason"] == "max_prompt_tokens"
                        && event["response"]["output"]
                            .as_array()
                            .is_some_and(Vec::is_empty)
                    {
                        return Ok(durable::SampleReceipt::Rejected {
                            message: "xAI context_length_exceeded: max_prompt_tokens".into(),
                        });
                    }
                    if let Some(response) = stream::terminal(&event).map_err(invalid)? {
                        return Ok(durable::SampleReceipt::Completed { response });
                    }
                }
            }
            Err(invalid(
                "xAI Responses stream ended without a completed response; no tools were dispatched",
            ))
        };
        tokio::select! {
            biased;
            _=cancel.wait()=>Err(NanocodexError::TurnCancelled),
            _=deadline(config.timeout)=>Err(invalid("xAI sampling timed out; request was not retried")),
            outcome=request=>outcome,
        }
    }
    async fn run(
        &self,
        mut config: Xai,
        prompt: Prompt,
        events: &AgentEventPublisher,
        cancel: &Cancellation,
        stats: &mut RunStats,
        request_id: Option<&str>,
    ) -> Result<TurnResult> {
        let mut history = self.history.lock().await;
        let mut cursor = durable::Cursor::open(&config, request_id, &history).await?;
        config.model = cursor.model.clone();
        config.thinking = cursor.thinking;
        config.max_steps = cursor.max_steps;
        config.max_retries = cursor.max_retries;
        config.repetition_limit = cursor.repetition_limit;
        config.context_window_tokens = cursor.context_window_tokens;
        config.compact_percent = cursor.compact_percent;
        config.keep_tail = cursor.keep_tail;
        let mut candidate = cursor.snapshot.history.clone();
        if !cursor.prepared {
            candidate.extend(lifecycle::prompt_items(&prompt)?);
            cursor.snapshot = durable::Snapshot::new(candidate.clone());
            cursor.prepared = true;
            cursor.advance(&config).await?;
        }
        let revision = prompt.instruction_revision();
        let mut usage = ReportedTurnUsage {
            input_tokens: 0,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 0,
            estimated_cost: None,
            cost_status: CostStatus::Other,
        };
        let mut reported = false;
        if let Some(prior) = &cursor.usage {
            reported = true;
            usage.input_tokens = prior.input_tokens();
            usage.cached_input_tokens = prior.cached_input_tokens();
            usage.output_tokens = prior.output_tokens();
            usage.total_tokens = prior.total_tokens();
            usage.reasoning_output_tokens = prior.reasoning_output_tokens();
        }
        let mut seen: HashSet<String> = candidate
            .iter()
            .filter(|item| item["type"] == "function_call")
            .filter_map(|item| item["call_id"].as_str().map(str::to_owned))
            .collect();
        let mut repetitions = cursor.repetitions.clone();
        let mut compacted = cursor.compactions > 0;
        for index in cursor.index..config.max_steps {
            cancel.check()?;
            for (_, steer) in self.steering.lock().unwrap().drain(..) {
                candidate.extend(lifecycle::prompt_items(&steer)?);
                stats.steers += 1;
                self.emit(
                    events,
                    AgentEventKind::RunSteered,
                    json!({"steer_index":stats.steers,"instruction_bytes":steer.text_bytes()}),
                );
            }
            // The round baseline stays immutable until all receipts have been
            // consumed. Recovery reconstructs rejected attempts and compaction
            // in order, using the exact same identities and inputs.
            let compact_step = format!("compact-{index}-auto");
            #[allow(clippy::collapsible_if)]
            if cursor.has_step(&compact_step)
                || (!cursor.has_effects()
                    && !compacted
                    && compaction::should_compact(
                        &config,
                        &candidate,
                        self.last_input_tokens.load(Ordering::SeqCst),
                    ))
            {
                if let Some(replacement) = self
                    .durable_compact(
                        &config,
                        &candidate,
                        events,
                        index,
                        cancel,
                        &mut cursor,
                        &compact_step,
                    )
                    .await?
                {
                    candidate = replacement;
                    *history = candidate.clone();
                    compacted = true;
                    stats.compactions += 1;
                }
            }
            if !cursor.has_effects() {
                cursor.snapshot = durable::Snapshot::new(candidate.clone());
                cursor.advance(&config).await?;
            }
            let mut attempt = 0usize;
            let mut retries = 0usize;
            let response = loop {
                let model_step = format!("model-{index}-{attempt}");
                let sampled = Instant::now();
                let receipt = match cursor
                    .begin(
                        &config,
                        &model_step,
                        "model",
                        json!({"input":candidate,"template":cursor.request_template}),
                    )
                    .await?
                {
                    durable::Step::Execute => {
                        stats.model_calls = stats.model_calls.saturating_add(1);
                        let receipt = self
                            .sample(
                                &config,
                                &candidate,
                                events,
                                index,
                                cancel,
                                Some(&cursor.request_template),
                                "generation",
                            )
                            .await?;
                        cursor
                            .complete(
                                &config,
                                &model_step,
                                serde_json::to_value(&receipt).map_err(error)?,
                            )
                            .await?;
                        receipt
                    }
                    durable::Step::Replay(value) => {
                        serde_json::from_value(value).map_err(durable::recovery_error)?
                    }
                    durable::Step::Uncertain => {
                        candidate.push(json!({"type":"message","role":"developer","content":"The previous model request has an unknown outcome after runtime recovery. No unconfirmed tools were dispatched. Continue only from this retained history."}));
                        *history = candidate;
                        return Err(invalid(
                            "xAI model request outcome unknown; submit a new prompt to continue",
                        ));
                    }
                };
                stats.model_ns = stats.model_ns.saturating_add(elapsed_ns(sampled));
                match receipt {
                    durable::SampleReceipt::Completed { response } => break response,
                    durable::SampleReceipt::Rejected { message } => {
                        if !compacted && compaction::context_limit(&message) {
                            let compact_step = format!("compact-{index}-rejection-{attempt}");
                            let replacement = self
                                .durable_compact(
                                    &config,
                                    &candidate,
                                    events,
                                    index,
                                    cancel,
                                    &mut cursor,
                                    &compact_step,
                                )
                                .await?
                                .ok_or_else(|| {
                                    invalid(format!("{message}; context compaction failed"))
                                })?;
                            candidate = replacement;
                            *history = candidate.clone();
                            compacted = true;
                            stats.compactions += 1;
                        } else if retries < config.max_retries && compaction::retryable(&message) {
                            retries += 1;
                            stats.retries += 1;
                            tokio::select! {
                                _ = cancel.wait() => return Err(NanocodexError::TurnCancelled),
                                _ = deadline(Duration::from_millis(100u64.saturating_mul(1 << retries.min(6)))) => {}
                            }
                        } else {
                            return Err(invalid(message));
                        }
                        attempt += 1;
                    }
                }
            };
            let output = response["output"]
                .as_array()
                .ok_or_else(|| invalid("xAI response output is not an array"))?;
            let calls = conversation::function_calls(output).map_err(invalid)?;
            // Validate every call before any side effect or history mutation.
            for call in &calls {
                if !seen.insert(call.call_id.clone()) {
                    return Err(invalid(
                        "xAI repeated a function call_id from committed history or this response",
                    ));
                }
            }
            candidate.extend(conversation::replay_output(output));
            *history = candidate.clone();
            if let Some(tokens) = response["usage"].as_object() {
                self.last_input_tokens.store(
                    tokens
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    Ordering::SeqCst,
                );
                reported = true;
                usage.input_tokens = usage.input_tokens.saturating_add(
                    tokens
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
                usage.output_tokens = usage.output_tokens.saturating_add(
                    tokens
                        .get("output_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
                usage.total_tokens = usage.total_tokens.saturating_add(
                    tokens
                        .get("total_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
                usage.cached_input_tokens = usage.cached_input_tokens.saturating_add(
                    response["usage"]["input_tokens_details"]["cached_tokens"]
                        .as_u64()
                        .unwrap_or(0),
                );
                usage.reasoning_output_tokens = usage.reasoning_output_tokens.saturating_add(
                    response["usage"]["output_tokens_details"]["reasoning_tokens"]
                        .as_u64()
                        .unwrap_or(0),
                );
            }
            stats.usage = reported.then(|| TurnUsage::from_reported(usage.clone()));
            if calls.is_empty() && self.steering.lock().unwrap().is_empty() {
                let text = conversation::assistant_text(output);
                self.emit(
                    events,
                    AgentEventKind::AssistantMessage,
                    json!({"model_call_index":index,"item_id":null,"phase":null,"text":text}),
                );
                let checkpoint_id = uuid::Uuid::new_v4().to_string();
                {
                    let mut checkpoints = self.checkpoints.lock().unwrap();
                    checkpoints.push((checkpoint_id.clone(), candidate.clone()));
                    if checkpoints.len() > 32 {
                        checkpoints.remove(0);
                    }
                }
                return Ok(TurnResult::from_backend(
                    request_id.map(str::to_owned),
                    text,
                    reported.then(|| TurnUsage::from_reported(usage)),
                )
                .with_backend_checkpoint(checkpoint_id));
            }
            for call in calls {
                stats.tool_calls = stats.tool_calls.saturating_add(1);
                let started = Instant::now();
                let input: std::result::Result<Value, _> = serde_json::from_str(&call.arguments);
                self.emit(events,AgentEventKind::ToolCall,json!({"call_id":call.call_id,"tool":call.name,"arguments":input.as_ref().cloned().unwrap_or_else(|_|Value::String(call.arguments.clone())),"model_call_index":index}));
                let tool_step = format!("tool-{}", call.call_id);
                let admission = cursor
                    .begin(
                        &config,
                        &tool_step,
                        "tool",
                        json!({"call_id":call.call_id,"name":call.name,"arguments":call.arguments}),
                    )
                    .await?;
                // Count admitted calls while reconstructing the round too:
                // receipt replay must preserve the per-turn execution guard.
                let repeated = if let Ok(input) = &input {
                    let count = repetitions
                        .entry(format!("{}:{}", call.name, input))
                        .or_default();
                    *count += 1;
                    *count > config.repetition_limit
                } else {
                    false
                };
                let result = match admission {
                    durable::Step::Replay(value) => {
                        let mut reply =
                            XaiToolReply::text(value["text"].as_str().unwrap_or_default());
                        reply.is_error = value["is_error"].as_bool().unwrap_or(false);
                        reply.content =
                            serde_json::from_value(value["content"].clone()).map_err(error)?;
                        reply.metadata = value.get("metadata").filter(|v| !v.is_null()).cloned();
                        reply.structured_result = value
                            .get("structured_result")
                            .filter(|v| !v.is_null())
                            .cloned();
                        Ok(reply)
                    }
                    durable::Step::Uncertain => {
                        // Pair every remaining call before making the checkpoint reusable.
                        let paired: HashSet<String> = candidate
                            .iter()
                            .filter(|v| v["type"] == "function_call_output")
                            .filter_map(|v| v["call_id"].as_str().map(str::to_owned))
                            .collect();
                        let unpaired: Vec<String> = candidate
                            .iter()
                            .filter(|v| v["type"] == "function_call")
                            .filter_map(|v| v["call_id"].as_str())
                            .filter(|id| !paired.contains(*id))
                            .map(str::to_owned)
                            .collect();
                        for id in unpaired {
                            candidate.push(json!({"type":"function_call_output","call_id":id,"output":"Tool outcome unknown after runtime recovery; do not repeat automatically."}));
                        }
                        *history = candidate;
                        return Err(invalid(
                            "xAI tool outcome unknown; submit a new prompt to reconcile",
                        ));
                    }
                    durable::Step::Execute => {
                        if cancel.check().is_err() {
                            Err("Tool skipped because turn was cancelled".into())
                        } else {
                            match (config.tools.get(&call.name), input) {
                                (Some((_, handler)), Ok(input)) if input.is_object() => {
                                    // Once started, a host effect must finish before cancellation can
                                    // release the session, so it cannot silently replay on the next turn.
                                    if repeated {
                                        Err("Repeated identical tool call blocked by repetition limit".into())
                                    } else {
                                        let invocation = XaiToolInvocation {
                                            model: config.model.clone(),
                                            session_id: self.session.clone(),
                                            turn_id: request_id
                                                .unwrap_or(events.request_id())
                                                .to_owned(),
                                            call_id: call.call_id.clone(),
                                            instruction_revision: revision,
                                            host_context: config.host_context.clone(),
                                        };
                                        std::panic::AssertUnwindSafe(async {
                                            handler(input, invocation).await
                                        })
                                        .catch_unwind()
                                        .await
                                        .unwrap_or_else(|_| Err("Host tool panicked".into()))
                                    }
                                }
                                (None, _) => Err(format!("Unknown tool: {}", call.name)),
                                _ => Err("Function arguments must be a JSON object".into()),
                            }
                        }
                    }
                };
                let reply =
                    result.unwrap_or_else(|err| XaiToolReply::error(format!("Tool error: {err}")));
                cursor.complete(&config,&tool_step,json!({"text":reply.text,"content":reply.content,"is_error":reply.is_error,"metadata":reply.metadata,"structured_result":reply.structured_result})).await?;
                let output = reply.wire_output();
                let status = if reply.is_error {
                    "failed"
                } else {
                    "completed"
                };
                let duration_ns = elapsed_ns(started);
                stats.tool_ns = stats.tool_ns.saturating_add(duration_ns);
                self.emit(events,AgentEventKind::ToolResult,json!({"call_id":call.call_id,"tool":call.name,"status":status,"duration_ns":duration_ns,"started_after_ns":null,"result":output,"structured_result":reply.structured_result,"metadata":reply.metadata}));
                candidate.push(
                    json!({"type":"function_call_output","call_id":call.call_id,"output":output}),
                );
                *history = candidate.clone();
            }
            cursor.compactions = usize::from(compacted);
            cursor.repetitions = repetitions.clone();
            cursor
                .finish_round(&config, &candidate, stats.usage.clone())
                .await?;
        }
        Err(invalid(
            "xAI turn reached max_steps; completed tools remain in history",
        ))
    }
}
impl Driver {
    async fn admit(self, mut request: BackendPrompt) -> Result<BackendTurn> {
        if request.request_id.is_some() && self.state.config.lock().unwrap().policy.is_none() {
            return Err(invalid(
                "xAI in-memory harness does not provide durable request deduplication",
            ));
        }
        lifecycle::prompt_items(&request.prompt)?;
        let config;
        let cancel = Arc::new(Cancellation::default());
        let done = Arc::new(Notify::new());
        {
            let mut active = self.state.active.lock().unwrap();
            if self.state.stopped.load(Ordering::SeqCst) {
                return Err(NanocodexError::AgentStopped);
            }
            if active.is_some() || self.state.compaction_cancel.lock().unwrap().is_some() {
                return Err(invalid("xAI session already has active work"));
            }
            if request.cancel_on_admission {
                cancel.cancel();
            }
            self.state.steer_ids.lock().unwrap().clear();
            config = self.state.config.lock().unwrap().clone();
            *active = Some(Active {
                key: request.key,
                cancel: cancel.clone(),
                done: done.clone(),
            });
        }
        if let Some(policy) = &config.policy {
            let admission = async {
                let automatic = request.request_id.is_none();
                let id = request
                    .request_id
                    .clone()
                    .unwrap_or_else(|| durable::candidate_id("request"));
                policy
                    .admit(
                        id,
                        serde_json::to_value(&request.prompt).map_err(error)?,
                        automatic,
                    )
                    .await
            }
            .await;
            match admission {
                Ok((id, durable::Admission::Execute | durable::Admission::Resume)) => {
                    request.request_id = Some(id.clone());
                    if let Err(err) = policy.begin_attempt(id).await {
                        self.state.active.lock().unwrap().take();
                        done.notify_waiters();
                        return Err(err);
                    }
                }
                Ok((id, durable::Admission::Completed { checkpoint, output })) => {
                    self.state.active.lock().unwrap().take();
                    done.notify_waiters();
                    let result = durable::replay(id.clone(), output).and_then(|result| {
                        let history = durable::Snapshot::decode(checkpoint)?.history;
                        let identity = uuid::Uuid::new_v4().to_string();
                        let mut checkpoints = self.state.checkpoints.lock().unwrap();
                        checkpoints.push((identity.clone(), history));
                        if checkpoints.len() > 32 {
                            checkpoints.remove(0);
                        }
                        Ok(result.with_backend_checkpoint(identity))
                    });
                    self.state
                        .replay_events(&request.events, &config, &request.prompt, &result);
                    return Ok(BackendTurn {
                        request_id: Some(id),
                        result: Box::pin(async move { result }),
                    });
                }
                Ok((
                    id,
                    failure @ (durable::Admission::Failed { .. } | durable::Admission::Cancelled),
                )) => {
                    self.state.active.lock().unwrap().take();
                    done.notify_waiters();
                    let result = Err(match failure {
                        durable::Admission::Failed { error, .. } => invalid(error),
                        _ => NanocodexError::TurnCancelled,
                    });
                    self.state
                        .replay_events(&request.events, &config, &request.prompt, &result);
                    return Ok(BackendTurn {
                        request_id: Some(id),
                        result: Box::pin(async move { result }),
                    });
                }
                Err(err) => {
                    self.state.active.lock().unwrap().take();
                    done.notify_waiters();
                    return Err(err);
                }
            }
        }
        let accepted_id = request.request_id.clone();
        let (sender, receiver) = oneshot::channel();
        let task = async move {
            let effort = effort(config.thinking).unwrap_or("high");
            self.state.emit(&request.events,AgentEventKind::RunStarted,json!({"mode":"xai","model":config.model,"reasoning_mode":"effort","effort":effort,"transport":"responses_sse","orchestration":"grok_build","websocket_url":"","workspace":null,"instruction_bytes":request.prompt.text_bytes()}));
            let started = Instant::now();
            let mut stats = RunStats::default();
            let mut result = self
                .state
                .run(
                    config.clone(),
                    request.prompt,
                    &request.events,
                    &cancel,
                    &mut stats,
                    request.request_id.as_deref(),
                )
                .await;
            {
                let history = self.state.history.lock().await;
                if let Err(err) =
                    durable::settle(&config, request.request_id.as_deref(), &history, &result).await
                {
                    result = Err(err);
                }
            }
            if result
                .as_ref()
                .err()
                .is_some_and(|err| err.execution_policy_disposition().is_some())
            {
                self.state.stopped.store(true, Ordering::SeqCst);
            }
            let duration_ns = elapsed_ns(started);
            self.state.emit_terminal(
                &request.events,
                &config,
                &result,
                &stats,
                duration_ns,
                false,
            );
            {
                let mut active = self.state.active.lock().unwrap();
                self.state.steering.lock().unwrap().clear();
                self.state.steer_ids.lock().unwrap().clear();
                active.take();
            }
            done.notify_waiters();
            let _ = sender.send(result);
        };
        #[cfg(not(target_family = "wasm"))]
        tokio::spawn(task);
        #[cfg(target_family = "wasm")]
        wasm_bindgen_futures::spawn_local(task);
        Ok(BackendTurn {
            request_id: accepted_id,
            result: Box::pin(async { receiver.await.map_err(|_| NanocodexError::TurnStopped)? }),
        })
    }
    fn configure(
        &self,
        model: Option<String>,
        thinking: Option<Thinking>,
    ) -> BackendFuture<Result<()>> {
        let state = self.state.clone();
        Box::pin(async move {
            let active = state.active.lock().unwrap();
            if active.is_some() || state.compaction_cancel.lock().unwrap().is_some() {
                return Err(invalid("xAI session is busy"));
            }
            if state.stopped.load(Ordering::SeqCst) {
                return Err(NanocodexError::AgentStopped);
            }
            let mut config = state.config.lock().unwrap();
            let selected_model = model.as_deref().unwrap_or(&config.model);
            let selected_thinking = thinking.unwrap_or(config.thinking);
            validate_effort(selected_model, selected_thinking)?;
            validate_hosted(selected_model, &config.hosted)?;
            config.thinking = selected_thinking;
            if let Some(model) = model {
                config.model = model;
            }
            Ok(())
        })
    }
}
impl LifecycleBackend for Driver {
    fn harness_family(&self) -> HarnessFamily {
        HarnessFamily::Xai
    }
    fn submit(&self, request: BackendPrompt) -> BackendFuture<Result<BackendTurn>> {
        Box::pin(self.clone().admit(request))
    }
    fn route(&self, request: BackendPrompt) -> BackendFuture<Result<BackendPromptRoute>> {
        let driver = self.clone();
        Box::pin(async move {
            {
                let active = driver.state.active.lock().unwrap();
                if active.is_some() {
                    lifecycle::prompt_items(&request.prompt)?;
                    driver
                        .state
                        .steering
                        .lock()
                        .unwrap()
                        .push((None, request.prompt));
                    return Ok(BackendPromptRoute::Steered);
                }
            }
            driver.admit(request).await.map(BackendPromptRoute::Started)
        })
    }
    fn steer(&self, key: BackendTurnKey, prompt: Prompt) -> BackendFuture<Result<()>> {
        self.steer_with_id(key, uuid::Uuid::new_v4().to_string(), prompt)
    }
    fn steer_with_id(
        &self,
        key: BackendTurnKey,
        id: String,
        prompt: Prompt,
    ) -> BackendFuture<Result<()>> {
        let state = self.state.clone();
        Box::pin(async move {
            lifecycle::prompt_items(&prompt)?;
            let active = state.active.lock().unwrap();
            if !active.as_ref().is_some_and(|a| a.key == key) {
                return Err(invalid("xAI turn is no longer active"));
            }
            if id.is_empty() {
                return Err(invalid("steer identity must not be empty"));
            }
            if state.steer_ids.lock().unwrap().insert(id.clone()) {
                state.steering.lock().unwrap().push((Some(id), prompt));
            }
            Ok(())
        })
    }
    fn withdraw_steer(&self, key: BackendTurnKey, id: String) -> BackendFuture<Result<bool>> {
        let state = self.state.clone();
        Box::pin(async move {
            let active = state.active.lock().unwrap();
            if !active.as_ref().is_some_and(|a| a.key == key) {
                return Ok(false);
            }
            let mut queue = state.steering.lock().unwrap();
            if let Some(index) = queue
                .iter()
                .position(|(found, _)| found.as_ref() == Some(&id))
            {
                queue.remove(index);
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }
    fn cancel(&self, key: BackendTurnKey) -> BackendFuture<Result<()>> {
        let state = self.state.clone();
        Box::pin(async move {
            let done = {
                let active = state.active.lock().unwrap();
                active.as_ref().filter(|a| a.key == key).map(|a| {
                    a.cancel.cancel();
                    a.done.clone()
                })
            };
            if let Some(done) = done {
                let notified = done.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let still_active = state
                    .active
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|a| a.key == key);
                if still_active {
                    notified.await;
                }
            }
            Ok(())
        })
    }
    fn set_model(&self, _: Model) -> BackendFuture<Result<()>> {
        unsupported("OpenAI model selectors")
    }
    fn set_harness_model(&self, model: HarnessModel) -> BackendFuture<Result<()>> {
        if model.family() != HarnessFamily::Xai {
            return unsupported("another harness family");
        }
        self.configure(Some(model.as_str().into()), None)
    }
    fn set_thinking(&self, thinking: Thinking) -> BackendFuture<Result<()>> {
        self.configure(None, Some(thinking))
    }
    fn set_fast_mode(&self, _: bool) -> BackendFuture<Result<()>> {
        unsupported("fast mode")
    }
    fn compact(&self) -> BackendFuture<Result<()>> {
        let state = self.state.clone();
        Box::pin(async move {
            let mut history = state.history.lock().await;
            let cancel = Arc::new(Cancellation::default());
            {
                let active = state.active.lock().unwrap();
                let mut slot = state.compaction_cancel.lock().unwrap();
                if state.stopped.load(Ordering::SeqCst) {
                    return Err(NanocodexError::AgentStopped);
                }
                if active.is_some() {
                    return Err(invalid("xAI session is busy"));
                }
                *slot = Some(cancel.clone());
            }
            struct Registration(Arc<State>);
            impl Drop for Registration {
                fn drop(&mut self) {
                    self.0.compaction_cancel.lock().unwrap().take();
                }
            }
            let _registration = Registration(state.clone());
            let config = state.config.lock().unwrap().clone();
            let replacement = state
                .compact_history(&config, &history, &state.events, 0, &cancel)
                .await?;
            cancel.check()?;
            if let Some(policy) = &config.policy {
                policy
                    .checkpoint(durable::Snapshot::new(replacement.clone()).encode()?)
                    .await?;
            }
            *history = replacement;
            Ok(())
        })
    }
    fn append_developer_message(&self, text: String) -> BackendFuture<Result<AgentSessionContext>> {
        let state = self.state.clone();
        Box::pin(async move {
            let mut history = state.history.lock().await;
            if state.stopped.load(Ordering::SeqCst) {
                return Err(NanocodexError::AgentStopped);
            }
            let config = state.config.lock().unwrap().clone();
            let mut candidate = history.clone();
            candidate.push(json!({"type":"message","role":"developer","content":text}));
            if let Some(policy) = &config.policy {
                policy
                    .checkpoint(durable::Snapshot::new(candidate.clone()).encode()?)
                    .await?;
            }
            *history = candidate;
            lifecycle::context(&config, &history)
        })
    }
    fn context(&self) -> BackendFuture<Result<AgentSessionContext>> {
        let state = self.state.clone();
        Box::pin(async move {
            let history = state.history.lock().await;
            let config = state.config.lock().unwrap();
            lifecycle::context(&config, &history)
        })
    }
    fn runtime_snapshot(&self) -> BackendFuture<Result<ChildSnapshot>> {
        let state = self.state.clone();
        Box::pin(async move { state.native_snapshot().await })
    }
    fn spawn(&self, options: SpawnOptions) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let handle = self.state.handle.clone();
        Box::pin(async move { handle.spawn_with(options).await })
    }
    fn fork(
        &self,
        completed: Option<TurnResult>,
    ) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let state = self.state.clone();
        Box::pin(async move {
            if state.stopped.load(Ordering::SeqCst) {
                return Err(NanocodexError::AgentStopped);
            }
            let history = if let Some(completed) = completed {
                let id = completed
                    .backend_checkpoint()
                    .ok_or_else(|| invalid("xAI result has no native checkpoint identity"))?;
                state
                    .checkpoints
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(key, _)| key == id)
                    .map(|(_, history)| history.clone())
                    .ok_or_else(|| {
                        invalid("xAI checkpoint expired or belongs to another session")
                    })?
            } else {
                state.history.lock().await.clone()
            };
            let mut recipe = state.recipe();
            recipe.restored_history = Some(history);
            recipe.build()
        })
    }
    fn flush(&self) -> BackendFuture<Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn shutdown(&self) -> BackendFuture<Result<()>> {
        let state = self.state.clone();
        Box::pin(async move {
            state.stopped.store(true, Ordering::SeqCst);
            if let Some(cancel) = state.compaction_cancel.lock().unwrap().as_ref() {
                cancel.cancel();
            }
            loop {
                let done = {
                    let active = state.active.lock().unwrap();
                    active.as_ref().map(|a| {
                        a.cancel.cancel();
                        a.done.clone()
                    })
                };
                let Some(done) = done else { break };
                let notified = done.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if state.active.lock().unwrap().is_none() {
                    break;
                }
                notified.await;
            }
            let _boundary = state.history.lock().await;
            let config = state.config.lock().unwrap().clone();
            durable::shutdown(&config).await
        })
    }
}

fn validate_hosted(model: &str, hosted: &[Value]) -> Result<()> {
    if model
        .parse::<nanocodex_agent::XaiModel>()
        .is_ok_and(|m| !m.supports_backend_search())
        && hosted
            .iter()
            .any(|tool| matches!(tool["type"].as_str(), Some("web_search" | "x_search")))
    {
        return Err(invalid(
            "selected xAI model does not support backend search",
        ));
    }
    Ok(())
}
fn validate_effort(model: &str, thinking: Thinking) -> Result<()> {
    effort(thinking)?;
    if model
        .parse::<nanocodex_agent::XaiModel>()
        .is_ok_and(|model| !model.supports_thinking(thinking))
    {
        return Err(invalid(
            "unsupported reasoning effort for selected xAI model",
        ));
    }
    Ok(())
}
#[cfg(not(target_family = "wasm"))]
async fn deadline(duration: Duration) {
    tokio::time::sleep(duration).await;
}
#[cfg(target_family = "wasm")]
async fn deadline(duration: Duration) {
    use wasm_bindgen::{JsCast, JsValue};
    struct Timer(Option<JsValue>);
    impl Drop for Timer {
        fn drop(&mut self) {
            if let Some(id) = self.0.take()
                && let Ok(function) =
                    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("clearTimeout"))
                        .and_then(|v| v.dyn_into::<js_sys::Function>())
            {
                let _ = function.call1(&js_sys::global(), &id);
            }
        }
    }
    let mut timer = Timer(None);
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let result = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("setTimeout"))
            .and_then(|function| function.dyn_into::<js_sys::Function>())
            .and_then(|function| {
                function.call2(
                    &js_sys::global(),
                    &resolve,
                    &JsValue::from_f64(duration.as_millis().min(i32::MAX as u128) as f64),
                )
            });
        match result {
            Ok(id) => timer.0 = Some(id),
            Err(error) => {
                let _ = reject.call1(&JsValue::UNDEFINED, &error);
            }
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
