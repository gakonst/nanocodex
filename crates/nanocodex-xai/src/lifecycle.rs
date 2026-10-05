use super::*;

impl Xai {
    /// Overrides session identity for an in-memory agent.
    pub fn session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }
    /// Host workspace exposed by context export.
    pub fn workspace(mut self, path: impl Into<String>) -> Self {
        self.workspace = path.into();
        self
    }
    /// Private tool context inherited by children.
    pub fn host_context(mut self, context: Option<Arc<str>>) -> Self {
        self.host_context = context;
        self
    }
    /// Builds independent host callbacks for each agent.
    pub fn tools_factory<F>(mut self, factory: F) -> Self
    where
        F: Fn(AgentHandle) -> Result<XaiTools> + Send + Sync + 'static,
    {
        self.tools_factory = Some(Arc::new(factory));
        self
    }
    /// Installs mixed-family child construction.
    pub fn spawn_factory(mut self, factory: Arc<dyn AgentFactory>) -> Self {
        self.spawn_factory = Some(factory);
        self
    }
    /// Context budget; Grok 4.5 and 4.6 default to 500,000 tokens upstream.
    pub const fn context_window_tokens(mut self, tokens: u64) -> Self {
        self.context_window_tokens = tokens;
        self
    }
    /// Explicit percentage; zero selects the upstream model-specific default.
    pub const fn auto_compact_threshold_percent(mut self, percent: u32) -> Self {
        self.compact_percent = percent;
        self
    }
    /// Minimum recent item tail retained at a complete turn boundary.
    pub const fn compaction_keep_tail(mut self, items: usize) -> Self {
        self.keep_tail = items;
        self
    }
    /// Retries only explicit transient HTTP rejections. Streams and tools never replay.
    pub const fn max_retries(mut self, retries: usize) -> Self {
        self.max_retries = retries;
        self
    }
    /// Maximum executions of one identical tool name/argument pair per turn.
    pub const fn repetition_limit(mut self, limit: usize) -> Self {
        self.repetition_limit = limit;
        self
    }
    /// Rebinds credentials and host callbacks to an idle native runtime snapshot.
    pub fn restore_runtime(mut self, snapshot: ChildSnapshot) -> Result<Self> {
        let ChildSnapshot::Native {
            model,
            session_id,
            thinking,
            payload,
            ..
        } = snapshot
        else {
            return Err(invalid("xAI requires a native checkpoint"));
        };
        if model.family() != HarnessFamily::Xai {
            return Err(invalid("checkpoint is not xAI"));
        }
        let data: Value = serde_json::from_str(&payload).map_err(error)?;
        if data["version"] != 1 {
            return Err(invalid("unsupported xAI checkpoint version"));
        }
        let native = data["model"]
            .as_str()
            .ok_or_else(|| invalid("checkpoint missing model"))?;
        if native
            .parse::<HarnessModel>()
            .unwrap_or_else(|_| HarnessFamily::Xai.default_model())
            != model
        {
            return Err(invalid("checkpoint model mismatch"));
        }
        self.model = native.into();
        self.thinking = thinking;
        self.session_id = Some(session_id);
        self.restored_history = Some(
            data["history"]
                .as_array()
                .ok_or_else(|| invalid("checkpoint missing history"))?
                .clone(),
        );
        if let Some(n) = data["context_window_tokens"].as_u64() {
            self.context_window_tokens = n;
        }
        if let Some(n) = data["compact_percent"].as_u64() {
            self.compact_percent = u32::try_from(n).map_err(error)?;
        }
        if let Some(n) = data["keep_tail"].as_u64() {
            self.keep_tail = n as usize;
        }
        Ok(self)
    }
}
pub(crate) fn prompt_items(prompt: &Prompt) -> Result<Vec<Value>> {
    let mut items = Vec::new();
    for message in prompt.transcript() {
        items.push(json!({"type":"message","role":match message.role(){PromptMessageRole::User=>"user",PromptMessageRole::Assistant=>"assistant"},"content":message.content()}));
    }
    let content = match &prompt.instruction {
        PromptInput::Text(text) => json!(text),
        PromptInput::Content(parts) => {
            let mut values = Vec::new();
            for part in parts {
                let mut v = serde_json::to_value(part).map_err(error)?;
                match v["type"].as_str() {
                    Some("text") => v["type"] = json!("input_text"),
                    Some("image") => {
                        if v.get("file_id").is_some() {
                            return Err(invalid(
                                "xAI image file identifiers are not portable; supply an image URL",
                            ));
                        }
                        v["type"] = json!("input_image");
                    }
                    _ => {
                        return Err(invalid(
                            "xAI prompts support text and URL/data-URL images; load local media through a host tool",
                        ));
                    }
                }
                values.push(v);
            }
            json!(values)
        }
    };
    items.push(json!({"type":"message","role":"user","content":content}));
    Ok(items)
}
pub(crate) fn context(config: &Xai, history: &[Value]) -> Result<AgentSessionContext> {
    let mut result = Vec::new();
    for item in history {
        let mut item = item.clone();
        if item["type"] == "message" && item["content"].is_string() {
            let text = item["content"].take();
            let kind = if item["role"] == "assistant" {
                "output_text"
            } else {
                "input_text"
            };
            item["content"] = json!([{ "type":kind,"text":text }]);
        }
        result.push(serde_json::from_value(item).map_err(error)?);
    }
    Ok(AgentSessionContext::from_backend(
        config.workspace.clone(),
        result,
    ))
}
pub(crate) struct NativeFactory {
    pub(crate) state: Mutex<Weak<State>>,
}
impl NativeFactory {
    fn owner(&self) -> Result<Arc<State>> {
        let state = self
            .state
            .lock()
            .unwrap()
            .upgrade()
            .ok_or(NanocodexError::AgentStopped)?;
        if state.stopped.load(Ordering::SeqCst) {
            return Err(NanocodexError::AgentStopped);
        }
        Ok(state)
    }
}
impl State {
    pub(crate) fn recipe(&self) -> Xai {
        let mut recipe = self.config.lock().unwrap().clone();
        recipe.session_id = None;
        recipe.restored_history = None;
        recipe.policy = None;
        recipe.checkpoint = None;
        recipe
    }
    pub(crate) async fn native_snapshot(&self) -> Result<ChildSnapshot> {
        let history = self.history.lock().await;
        let config = self.config.lock().unwrap();
        let model = config
            .model
            .parse()
            .unwrap_or_else(|_| HarnessFamily::Xai.default_model());
        Ok(ChildSnapshot::Native{model,session_id:self.session.clone(),thinking:config.thinking,has_conversation:!history.is_empty(),payload:json!({"version":1,"model":config.model,"history":*history,"context_window_tokens":config.context_window_tokens,"compact_percent":config.compact_percent,"keep_tail":config.keep_tail}).to_string()})
    }
}
impl AgentFactory for NativeFactory {
    fn ensure_available(&self, _: AgentHandle) -> BackendFuture<Result<()>> {
        let result = self.owner().map(|_| ());
        Box::pin(async move { result })
    }
    fn settings(&self, _: AgentHandle) -> BackendFuture<Result<(HarnessModel, Thinking)>> {
        let state = self.owner();
        Box::pin(async move {
            let state = state?;
            let config = state.config.lock().unwrap();
            Ok((
                config
                    .model
                    .parse()
                    .unwrap_or_else(|_| HarnessFamily::Xai.default_model()),
                config.thinking,
            ))
        })
    }
    fn spawn(
        &self,
        _: AgentHandle,
        options: SpawnOptions,
        context: Option<Arc<str>>,
    ) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let state = self.owner();
        Box::pin(async move {
            let state = state?;
            options.validate_harness()?;
            let mut recipe = state.recipe();
            if options
                .selected_harness()
                .is_some_and(|f| f != HarnessFamily::Xai)
                || options
                    .selected_harness_model()
                    .is_some_and(|m| m.family() != HarnessFamily::Xai)
            {
                return Err(invalid(
                    "mixed-family spawn requires an embedding AgentFactory",
                ));
            }
            if options.selected_harness_model().is_some() {
                let inherited = recipe
                    .model
                    .parse()
                    .unwrap_or_else(|_| HarnessFamily::Xai.default_model());
                let resolved = options.resolve(inherited, recipe.thinking)?;
                recipe.model = resolved
                    .selected_harness_model()
                    .expect("resolved model")
                    .as_str()
                    .into();
                recipe.thinking = resolved.selected_thinking().expect("resolved thinking");
            } else if let Some(thinking) = options.selected_thinking() {
                recipe.thinking = thinking;
            }
            if context.is_some() {
                recipe.host_context = context;
            }
            recipe.build()
        })
    }
    fn restore(
        &self,
        _: AgentHandle,
        snapshot: ChildSnapshot,
        context: Option<Arc<str>>,
    ) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let state = self.owner();
        Box::pin(async move {
            let state = state?;
            state
                .recipe()
                .restore_runtime(snapshot)?
                .host_context(context)
                .build()
        })
    }
    fn fork(&self, _: AgentHandle) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let state = self.owner();
        Box::pin(async move {
            let state = state?;
            let history = state.history.lock().await.clone();
            let mut recipe = state.recipe();
            recipe.restored_history = Some(history);
            recipe.build()
        })
    }
}

impl Xai {
    /// Adds a contextual rich-result host function.
    #[cfg(not(target_family = "wasm"))]
    pub fn tool_with_context<F, Fut>(mut self, definition: ToolDefinition, callback: F) -> Self
    where
        F: Fn(Value, XaiToolInvocation) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = std::result::Result<XaiToolReply, String>> + Send + 'static,
    {
        self.tools.extend(
            XaiTools::new()
                .tool_with_context(definition, callback)
                .tools,
        );
        self
    }
    /// Adds an isolate-local contextual rich-result host function.
    #[cfg(target_family = "wasm")]
    pub fn tool_with_context<F, Fut>(mut self, definition: ToolDefinition, callback: F) -> Self
    where
        F: Fn(Value, XaiToolInvocation) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = std::result::Result<XaiToolReply, String>> + 'static,
    {
        self.tools.extend(
            XaiTools::new()
                .tool_with_context(definition, callback)
                .tools,
        );
        self
    }
    /// Binds the host's durable ownership policy and validated native checkpoint.
    pub fn execution_policy(
        mut self,
        policy: Arc<dyn durable::XaiExecutionPolicy>,
        checkpoint: Option<Value>,
    ) -> Result<Self> {
        if let Some(value) = &checkpoint {
            self.restored_history = Some(durable::Snapshot::decode(value.clone())?.history);
        }
        self.policy = Some(policy);
        self.checkpoint = checkpoint;
        Ok(self)
    }
    /// Explicit provider-hosted tools supported by the Responses protocol.
    pub fn hosted_tool(mut self, definition: Value) -> Result<Self> {
        if !matches!(
            definition["type"].as_str(),
            Some("web_search" | "x_search" | "code_interpreter")
        ) {
            return Err(invalid("unsupported xAI hosted tool type"));
        }
        self.hosted.push(definition);
        Ok(self)
    }
}

/// Host authorization callback, invoked for every physical request.
#[cfg(not(target_family = "wasm"))]
pub type XaiAuthFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// Isolate-local host authorization callback.
#[cfg(target_family = "wasm")]
pub type XaiAuthFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;
/// Authorization could not be obtained from the host.
#[derive(Debug)]
pub struct XaiAuthUnavailable;
/// Host-owned renewable authorization, never retained in snapshots.
pub trait XaiAuthProvider: Send + Sync {
    fn headers(
        &self,
    ) -> XaiAuthFuture<'_, std::result::Result<reqwest::header::HeaderMap, XaiAuthUnavailable>>;
}
impl XaiClient {
    /// Creates a transport with renewable host-owned headers.
    pub fn with_auth_provider(
        http: reqwest::Client,
        endpoint: impl Into<String>,
        auth: Arc<dyn XaiAuthProvider>,
    ) -> Self {
        Self {
            http,
            endpoint: endpoint.into(),
            key: Arc::from(""),
            auth: Some(auth),
        }
    }
}
