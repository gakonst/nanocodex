use super::*;
use nanocodex::{
    Xai,
    tools::{
        ToolContext, ToolInput,
        contract::{ToolOutputBody, ToolOutputContent},
        runtime::ToolRuntime,
        workspace_runtime::WorkspaceToolRuntime,
    },
    xai::{ToolDefinition, XaiClient, XaiToolReply, XaiTools},
    xai_tools::{BashRequest, HostFuture, SandboxBashExecutor, XaiBash},
};
use serde_json::{Value, json, value::to_raw_value};
use tokio::time::{Duration, Instant};

/// Each family resolves its own credentials only when its recipe is used.
#[derive(Clone)]
pub(super) struct XaiConnection {
    api_key: Option<String>,
    endpoint: Option<String>,
    client: Arc<tokio::sync::OnceCell<XaiClient>>,
}
impl XaiConnection {
    pub(super) fn new(api_key: Option<String>, endpoint: Option<String>) -> Self {
        Self {
            api_key,
            endpoint,
            client: Arc::new(tokio::sync::OnceCell::new()),
        }
    }
    async fn client(&self) -> std::result::Result<XaiClient, String> {
        self.client
            .get_or_try_init(|| async {
                let key = self
                    .api_key
                    .clone()
                    .filter(|key| !key.trim().is_empty())
                    .ok_or("xAI requires --xai-api-key or XAI_API_KEY")?;
                let http = reqwest::Client::builder()
                    .build()
                    .map_err(|e| e.to_string())?;
                Ok(XaiClient::new(
                    http,
                    self.endpoint
                        .clone()
                        .unwrap_or_else(|| "https://api.x.ai/v1/responses".into()),
                    key,
                ))
            })
            .await
            .cloned()
    }
}

async fn install_computer_tools(tools: Tools) -> Result<Tools> {
    let mut builder = tools.into_builder();
    if let Some(config) = nanocodex_computer::ComputerConfig::discover_or_install()
        .await
        .map_err(|e| eyre!(e))?
    {
        let computer = nanocodex_computer::ComputerTools::connect(config)
            .await
            .map_err(|e| eyre!(e.to_string()))?;
        for tool in computer.tools() {
            builder = builder.add(tool);
        }
    }
    Ok(builder.build()?)
}

impl AgentArgs {
    pub(super) async fn build_xai(
        self,
        durable: Option<DurableSession>,
        vm: VmArgs,
        tui: bool,
        local_durability: Option<LocalDurability>,
        requested_model: Option<HarnessModel>,
    ) -> Result<ConfiguredAgent> {
        let responses_transport = self.responses_transport();
        let web_search = self.web_search();
        let xai_search = self.model_policy.web_search;
        let model = requested_model.unwrap_or_else(|| HarnessFamily::Xai.default_model());
        let thinking = self
            .model_policy
            .requested_thinking(HarnessFamily::Xai)?
            .unwrap_or_else(|| model.default_thinking());
        if !model.supports_thinking(thinking) {
            return Err(eyre!("model {model} does not support thinking {thinking}"));
        }
        if local_durability.is_some() && self.rollouts {
            return Err(eyre!(
                "local durability testing requires `--rollouts false`"
            ));
        }
        if durable.is_some() {
            return Err(eyre!(
                "Codex rollouts cannot be resumed with --harness xai; use a native xAI durability store"
            ));
        }
        if vm.is_enabled() {
            return Err(eyre!("the native xAI CLI does not yet support --vm"));
        }
        if self.mpp.is_enabled() {
            return Err(eyre!(
                "the native xAI CLI does not yet support the Tempo provider"
            ));
        }
        if self.memory {
            return Err(eyre!("the native xAI CLI does not yet support --memory"));
        }
        self.validate_xai_options()?;
        xai_web_search(model, xai_search)?;
        let connection = XaiConnection::new(self.xai_api_key, self.xai_responses_url);
        let client = connection.client().await.map_err(|error| eyre!(error))?;
        let workspace = self
            .cwd
            .unwrap_or_else(|| PathBuf::from("."))
            .canonicalize()
            .wrap_err("failed to resolve the xAI workspace")?;
        let codex_home = default_codex_home()?;
        let workspaces = Arc::new(super::claude::WorkspaceRegistry::new(
            workspace.clone(),
            codex_home.clone(),
        ));
        let managed_mcp = if self.mcp.loads_managed() {
            load_managed_mcp_credential(&codex_home).await?
        } else {
            None
        };
        let mcp = self.mcp.build(&codex_home, None, managed_mcp.as_ref())?;
        let mcp_handle = mcp.as_ref().map(|mcp| mcp.handle.clone());
        let mut tools = Tools::builder()
            .workspace(false)
            .web_search(false)
            .image_generation(false);
        if let Some(ConfiguredMcp { provider, .. }) = mcp {
            tools = tools.provider(provider);
        }
        let tools = tools.build()?;
        let registry = self
            .subagents
            .then(|| subagents::channel(self.max_subagents));
        let tool_registry = registry
            .as_ref()
            .map(|(registry, _, _)| Arc::clone(registry));
        // The same CUA host catalog used by Codex remains available through exec.
        let tools = install_computer_tools(tools).await?;
        let instructions = self.instructions;
        let codex_auth = self.auth;
        let codex_tools = tools
            .clone()
            .into_builder()
            .workspace(true)
            .web_search(web_search)
            .image_generation(self.image_generation.unwrap_or(true))
            .build()?;
        let codex_home_for_recipe = codex_home.clone();
        let codex_workspace = workspace.clone();
        let codex_workspaces = Arc::clone(&workspaces);
        let codex_registry = tool_registry.clone();
        let codex_instructions = instructions.clone();
        let websocket_url = self.websocket_url;
        let api_base_url = self.api_base_url;
        let model_id_prefix = self.model_id_prefix;
        let reasoning_mode = self.reasoning_mode;
        let fast_mode = self.fast_mode.unwrap_or(true);
        let websocket_warmup = self.websocket_warmup;
        let store_responses = self.store_responses;
        let harness_builder =
            nanocodex::Harness::builder().register(HarnessFamily::Codex, move |request| {
                let auth = codex_auth.clone();
                let tools = codex_tools.clone();
                let workspace = codex_workspace.clone();
                let workspaces = Arc::clone(&codex_workspaces);
                let codex_home = codex_home_for_recipe.clone();
                let registry = codex_registry.clone();
                let instructions = codex_instructions.clone();
                let websocket_url = websocket_url.clone();
                let api_base_url = api_base_url.clone();
                let model_id_prefix = model_id_prefix.clone();
                async move {
                    let HarnessModel::Codex(model) = request.model else {
                        return Err(nanocodex::NanocodexError::InvalidRequest(
                            "Codex recipe received another family model".into(),
                        ));
                    };
                    workspaces
                        .authorize_cross_family(request.parent.as_ref())
                        .map_err(nanocodex::NanocodexError::InvalidRequest)?;
                    let session_id = match &request.snapshot {
                        Some(nanocodex::agent::ChildSnapshot::Codex(snapshot)) => {
                            snapshot.session_id.parse::<SessionId>().map_err(|error| {
                                nanocodex::NanocodexError::InvalidRequest(error.to_string())
                            })?
                        }
                        _ => SessionId::new(),
                    };
                    let session_key = session_id.to_string();
                    if let Some(parent) = &request.parent {
                        workspaces.initialize(parent.session_id(), &session_key)
                    } else {
                        workspaces.seed(&session_key, workspace)
                    }
                    .map_err(nanocodex::NanocodexError::InvalidRequest)?;
                    let workspace = workspaces
                        .current(&session_key)
                        .map_err(nanocodex::NanocodexError::InvalidRequest)?;
                    let tool_workspace = workspace.clone();
                    let auth = auth
                        .resolve()
                        .map_err(|error| {
                            nanocodex::NanocodexError::InvalidRequest(error.to_string())
                        })?
                        .nanocodex()
                        .map_err(|error| {
                            nanocodex::NanocodexError::InvalidRequest(error.to_string())
                        })?;
                    let mut openai = OpenAi::builder(auth.clone())
                        .transport(responses_transport)
                        .websocket_warmup(websocket_warmup)
                        .websocket_url(direct_websocket_url(websocket_url, auth.mode()));
                    if let Some(store) = store_responses {
                        openai = openai.store(store);
                    }
                    if let Some(url) = api_base_url {
                        openai = openai.api_base_url(url);
                    }
                    if let Some(prefix) = model_id_prefix {
                        openai = openai.model_id_prefix(prefix);
                    }
                    let client = openai.build().map_err(|error| {
                        nanocodex::NanocodexError::InvalidRequest(error.to_string())
                    })?;
                    let registry_enabled = registry.is_some();
                    let mut builder = Nanocodex::builder(client)
                        .session_id(session_id)
                        .workspace(workspace)
                        .codex_home(codex_home)
                        .model(model)
                        .thinking(request.thinking)
                        .reasoning_mode(reasoning_mode)
                        .fast_mode(fast_mode)
                        .host_context(request.host_context)
                        .spawn_factory(request.spawn_factory)
                        .tools_factory(move |parent| {
                            workspaces
                                .seed(parent.session_id(), tool_workspace.clone())
                                .map_err(
                                    nanocodex::tools::runtime::ToolsBuildError::HostInitialization,
                                )?;
                            if let Some(registry) = &registry {
                                nanocodex_subagents::install_tools(
                                    tools.clone(),
                                    parent,
                                    Arc::clone(registry),
                                )
                            } else {
                                Ok(tools.clone())
                            }
                        });
                    if instructions.is_none()
                        && let Some(extra) = session_instructions(None, registry_enabled, false)
                    {
                        builder = builder.additional_instructions(extra);
                    }
                    if let Some(instructions) = instructions {
                        builder = builder.instructions(instructions);
                    }
                    if let Some(checkpoint) = request.snapshot {
                        builder = builder.restore_runtime(checkpoint)?;
                    }
                    builder.build()
                }
            });
        let harness_builder = super::claude::register_claude_recipe(
            harness_builder,
            super::claude::ClaudeConnection::new(
                self.claude_auth,
                self.claude_api_key,
                self.claude_messages_url,
            )
            .with_hooks(self.claude_hooks)
            .with_permission_config(
                self.claude_permissions.as_deref(),
                self.permission_mode.as_deref(),
            )?,
            workspace.clone(),
            instructions.clone(),
            tools.clone(),
            web_search,
            tool_registry.clone(),
            mcp_handle.clone(),
            Arc::clone(&workspaces),
        );
        let harness = register_xai_recipe(
            harness_builder,
            connection,
            workspace.clone(),
            instructions.clone(),
            tools.clone(),
            xai_search,
            tool_registry.clone(),
            Arc::clone(&workspaces),
        )
        .build();
        let mut builder = configured_xai_builder(
            client,
            model,
            thinking,
            workspace,
            instructions,
            tools,
            xai_search,
            tool_registry,
            workspaces,
        )?
        .spawn_factory(harness.spawn_factory());
        // Persist native Responses history in its own family journal by default.
        let persistence = local_durability.or_else(|| {
            self.rollouts.then(|| LocalDurability {
                path: codex_home.join("xai/sessions.sqlite"),
                state_id: SessionId::new().to_string(),
            })
        });
        if let Some(persistence) = persistence {
            if let Some(parent) = persistence.path.parent() {
                std::fs::create_dir_all(parent)
                    .wrap_err("failed to create xAI durability directory")?;
            }
            let store = SqliteStore::open(&persistence.path)
                .wrap_err("failed to open xAI durability store")?;
            let state = PortableDurableSession::open(store, persistence.state_id).await?;
            builder = builder.durability(state).await?;
        }
        let (handle, events) = builder.build()?;
        let (child_agents, subagent_updates) =
            registry.map_or((None, None), |(_, control, updates)| {
                let (drain, updates) = if tui {
                    (None, Some(updates))
                } else {
                    (Some(updates), None)
                };
                (
                    Some(ChildAgents::new(
                        handle.session_id().to_owned(),
                        control,
                        drain,
                    )),
                    updates,
                )
            });
        Ok(ConfiguredAgent {
            claude_interactions: None,
            claude_scheduler: None,
            handle,
            events,
            realtime: None,
            child_agents,
            subagent_updates,
            mpp_adapter: None,
            mcp: mcp_handle,
            browser: None,
            vm: None,
            model,
        })
    }
}

impl AgentArgs {
    fn validate_xai_options(&self) -> Result<()> {
        if self.reasoning_mode != ReasoningMode::Standard {
            return Err(eyre!(
                "--reasoning-mode is a Codex option; xAI uses --thinking"
            ));
        }
        if self.fast_mode == Some(true) {
            return Err(eyre!(
                "--fast-mode true is not supported by the native xAI harness"
            ));
        }
        if self.image_generation == Some(true) {
            return Err(eyre!(
                "--image-generation true is not supported by the native xAI harness"
            ));
        }
        if self.responses_transport == Some(ResponsesTransport::WebSocket) || self.websocket_warmup
        {
            return Err(eyre!(
                "the native xAI harness uses HTTPS Responses SSE; WebSocket transport and warmup are unsupported"
            ));
        }
        if self.store_responses == Some(true) {
            return Err(eyre!(
                "--store-responses true is unsupported by xAI; native sessions use local persistence"
            ));
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn configured_xai_builder(
    client: XaiClient,
    model: HarnessModel,
    thinking: Thinking,
    workspace: PathBuf,
    instructions: Option<String>,
    tools: Tools,
    web_search: Option<bool>,
    registry: Option<Arc<nanocodex_subagents::Registry>>,
    workspaces: Arc<super::claude::WorkspaceRegistry>,
) -> nanocodex::agent::Result<Xai> {
    let web_search = xai_web_search(model, web_search)?;
    let instructions = super::instructions::native(
        HarnessFamily::Xai,
        instructions,
        &workspace,
        web_search,
        registry.is_some(),
    );
    let mut builder = Nanocodex::builder(Xai::new(client, model.as_str()))
        .thinking(thinking)
        .workspace(workspace.to_string_lossy().into_owned())
        .system(instructions)
        .tools_factory(move |parent| {
            workspaces
                .seed(parent.session_id(), workspace.clone())
                .map_err(nanocodex::NanocodexError::InvalidRequest)?;
            let tools = if let Some(registry) = &registry {
                nanocodex_subagents::install_tools(tools.clone(), parent, Arc::clone(registry))?
            } else {
                tools.clone()
            };
            native_tools(&workspace, tools)
        });
    if web_search {
        builder = builder.web_search();
    }
    Ok(builder)
}

fn xai_web_search(model: HarnessModel, requested: Option<bool>) -> nanocodex::agent::Result<bool> {
    let HarnessModel::Xai(model) = model else {
        return Err(nanocodex::NanocodexError::InvalidRequest(
            "xAI recipe requires an xAI model".into(),
        ));
    };
    if requested == Some(true) && !model.supports_backend_search() {
        return Err(nanocodex::NanocodexError::InvalidRequest(format!(
            "model {model} does not support --web-search true"
        )));
    }
    Ok(requested.unwrap_or_else(|| model.supports_backend_search()))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn register_xai_recipe(
    harness: nanocodex::HarnessBuilder,
    connection: XaiConnection,
    workspace: PathBuf,
    instructions: Option<String>,
    tools: Tools,
    web_search: Option<bool>,
    registry: Option<Arc<nanocodex_subagents::Registry>>,
    workspaces: Arc<super::claude::WorkspaceRegistry>,
) -> nanocodex::HarnessBuilder {
    harness.register(HarnessFamily::Xai, move |request| {
        let connection = connection.clone();
        let workspace = workspace.clone();
        let instructions = instructions.clone();
        let tools = tools.clone();
        let registry = registry.clone();
        let workspaces = Arc::clone(&workspaces);
        async move {
            workspaces
                .authorize_cross_family(request.parent.as_ref())
                .map_err(nanocodex::NanocodexError::InvalidRequest)?;
            let session_id = match &request.snapshot {
                Some(nanocodex::agent::ChildSnapshot::Native { session_id, .. }) => {
                    session_id.clone()
                }
                _ => SessionId::new().to_string(),
            };
            if let Some(parent) = &request.parent {
                workspaces.initialize(parent.session_id(), &session_id)
            } else {
                workspaces.seed(&session_id, workspace)
            }
            .map_err(nanocodex::NanocodexError::InvalidRequest)?;
            let workspace = workspaces
                .current(&session_id)
                .map_err(nanocodex::NanocodexError::InvalidRequest)?;
            let client = connection
                .client()
                .await
                .map_err(nanocodex::NanocodexError::InvalidRequest)?;
            let mut builder = configured_xai_builder(
                client,
                request.model,
                request.thinking,
                workspace,
                instructions,
                tools,
                web_search,
                registry,
                workspaces,
            )?
            .session_id(session_id)
            .spawn_factory(request.spawn_factory)
            .host_context(request.host_context);
            if let Some(checkpoint) = request.snapshot {
                builder = builder.restore_runtime(checkpoint)?;
            }
            builder.build()
        }
    })
}

fn native_tools(workspace: &Path, tools: Tools) -> nanocodex::agent::Result<XaiTools> {
    let files = nanocodex::xai_tools::XaiWorkspaceFiles::new(workspace)
        .map_err(nanocodex::NanocodexError::InvalidRequest)?;
    let shell = XaiBash::new(Arc::new(RetainedBash {
        runtime: Arc::new(WorkspaceToolRuntime::new(workspace.to_path_buf())),
        gate: Arc::new(tokio::sync::Mutex::new(())),
    }));
    let mut native = XaiTools::new().host(Arc::new(files)).host(Arc::new(shell));
    // The retained host owns MCP discovery, browser tools and shared subagents.
    let runtime = Arc::new(RetainedHost(ToolRuntime::new_with_tools(
        workspace, None, None, &tools,
    )));
    let descriptions = runtime.model_specs("native-xai");
    for (name, description, schema) in [
        (
            "exec",
            "Execute JavaScript against retained host capabilities. Use text(value) to return output.",
            json!({"type":"object","properties":{"code":{"type":"string"}},"required":["code"],"additionalProperties":false}),
        ),
        (
            "wait",
            "Wait for a yielded JavaScript cell.",
            json!({"type":"object","properties":{"cell_id":{"type":"string"},"yield_time_ms":{"type":"integer"},"max_tokens":{"type":"integer"},"terminate":{"type":"boolean"}},"required":["cell_id"],"additionalProperties":false}),
        ),
        (
            "tool_search",
            "Discover currently available MCP tools before calling them from exec.",
            json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer"}},"required":["query"],"additionalProperties":false}),
        ),
    ] {
        let description = descriptions
            .iter()
            .find(|definition| definition.name() == name)
            .map_or(description, |definition| definition.description())
            .to_owned();
        let definition = ToolDefinition {
            name: name.into(),
            description,
            parameters: schema,
        };
        let runtime = Arc::clone(&runtime);
        native = native.tool_with_context(definition, move |input: Value, invocation| {
            let runtime = Arc::clone(&runtime);
            async move {
                let context = ToolContext::new(
                    &invocation.model,
                    &invocation.session_id,
                    &invocation.call_id,
                    &[],
                    16_000,
                )
                .with_turn_id(Some(&invocation.turn_id))
                .with_host_context(invocation.host_context.as_deref())
                .with_instruction_revision(invocation.instruction_revision);
                if name == "tool_search" {
                    let input =
                        ToolInput::Function(to_raw_value(&input).map_err(|e| e.to_string())?);
                    let output = runtime
                        .execute_tool(name, input, context)
                        .await
                        .map_err(|e| e.to_string())?;
                    let mut reply = runtime_reply(&output.output, output.success)?;
                    reply.structured_result = Some(output.structured_result());
                    reply.metadata = output
                        .metadata
                        .as_ref()
                        .and_then(|m| serde_json::from_str(m.get()).ok());
                    return Ok(reply);
                }
                let execution = if name == "exec" {
                    let code = input
                        .get("code")
                        .and_then(Value::as_str)
                        .ok_or("exec requires code")?;
                    runtime.execute_code(code, context).await
                } else {
                    runtime.wait_for_code(&input.to_string(), context).await
                }
                .map_err(|e| e.to_string())?;
                runtime_reply(&execution.output, execution.success)
            }
        });
    }
    Ok(native)
}

fn runtime_reply(
    output: &ToolOutputBody,
    success: bool,
) -> std::result::Result<XaiToolReply, String> {
    use nanocodex::xai_tools::ToolContent;
    let mut reply = XaiToolReply::text("");
    reply.is_error = !success;
    match output {
        ToolOutputBody::Text(text) => reply.text = text.clone(),
        ToolOutputBody::Content(items) => {
            for item in items {
                reply.content.push(match item {
                    ToolOutputContent::InputText { text } => {
                        ToolContent::InputText { text: text.clone() }
                    }
                    ToolOutputContent::InputImage { image_url, detail } => {
                        ToolContent::InputImage {
                            image_url: image_url.clone(),
                            detail: serde_json::to_value(detail)
                                .map_err(|e| e.to_string())?
                                .as_str()
                                .map(str::to_owned),
                        }
                    }
                    ToolOutputContent::InputImageFile { .. }
                    | ToolOutputContent::InputAudio { .. }
                    | ToolOutputContent::EncryptedContent { .. } => {
                        return Err(
                            "host media cannot be represented in native xAI Responses".into()
                        );
                    }
                });
            }
        }
    }
    Ok(reply)
}

struct RetainedHost(ToolRuntime);
impl std::ops::Deref for RetainedHost {
    type Target = ToolRuntime;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Drop for RetainedHost {
    fn drop(&mut self) {
        let control = self.0.control();
        tokio::spawn(async move { control.cancel().await });
    }
}

#[derive(Clone)]
struct RetainedBash {
    runtime: Arc<WorkspaceToolRuntime>,
    gate: Arc<tokio::sync::Mutex<()>>,
}

struct CancelShell {
    runtime: Option<Arc<WorkspaceToolRuntime>>,
    gate: Option<tokio::sync::OwnedMutexGuard<()>>,
}
impl Drop for CancelShell {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            let gate = self.gate.take();
            tokio::spawn(async move {
                runtime.control().cancel().await;
                drop(gate);
            });
        }
    }
}

impl SandboxBashExecutor for RetainedBash {
    fn execute(
        &self,
        request: BashRequest,
    ) -> HostFuture<std::result::Result<XaiToolReply, String>> {
        let executor = self.clone();
        Box::pin(async move { executor.run(request).await })
    }
}
impl RetainedBash {
    async fn run(&self, request: BashRequest) -> std::result::Result<XaiToolReply, String> {
        let gate = Arc::clone(&self.gate).lock_owned().await;
        let mut cleanup = CancelShell {
            runtime: Some(Arc::clone(&self.runtime)),
            gate: Some(gate),
        };
        let deadline = Instant::now() + Duration::from_millis(request.timeout_ms);
        let context = ToolContext::new(
            &request.context.model,
            &request.context.session_id,
            &request.context.call_id,
            &[],
            1024,
        )
        .with_turn_id(Some(&request.context.turn_id))
        .with_host_context(request.context.host_context.as_deref())
        .with_instruction_revision(request.context.instruction_revision);
        let input = json!({"cmd":request.command,"yield_time_ms":250,"max_output_tokens":1024});
        let mut output = match tokio::time::timeout_at(
            deadline,
            self.runtime.execute_tool(
                "exec_command",
                ToolInput::Function(to_raw_value(&input).map_err(|e| e.to_string())?),
                context,
            ),
        )
        .await
        {
            Ok(output) => output,
            Err(_) => {
                self.runtime.control().cancel().await;
                cleanup.runtime = None;
                return Err("Bash timed out; retained process terminated".to_owned());
            }
        };
        let mut stdout = String::new();
        let mut truncated = false;
        loop {
            if !output.success {
                return Err(output.structured_result().to_string());
            }
            let result = output.structured_result();
            let chunk = result
                .get("output")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let remaining = request.max_output_bytes.saturating_sub(stdout.len());
            let mut end = chunk.len().min(remaining);
            while !chunk.is_char_boundary(end) {
                end -= 1;
            }
            stdout.push_str(&chunk[..end]);
            truncated |= end < chunk.len();
            truncated |= result
                .get("original_token_count")
                .and_then(Value::as_u64)
                .is_some_and(|tokens| tokens.saturating_mul(4) > chunk.len() as u64 + 3);
            if let Some(code) = result.get("exit_code").and_then(Value::as_i64) {
                cleanup.runtime = None;
                let value = json!({"stdout":stdout,"exit_code":code,"truncated":truncated});
                let mut reply = XaiToolReply::text(value.to_string()).with_structured_result(value);
                reply.is_error = code != 0;
                return Ok(reply);
            }
            let session = result
                .get("session_id")
                .and_then(Value::as_i64)
                .ok_or("Bash host returned neither exit status nor retained process")?;
            let input = json!({"session_id":session,"yield_time_ms":250,"max_output_tokens":1024});
            output = match tokio::time::timeout_at(
                deadline,
                self.runtime.execute_tool(
                    "write_stdin",
                    ToolInput::Function(to_raw_value(&input).map_err(|e| e.to_string())?),
                    context,
                ),
            )
            .await
            {
                Ok(output) => output,
                Err(_) => {
                    self.runtime.control().cancel().await;
                    cleanup.runtime = None;
                    return Err("Bash timed out; retained process terminated".to_owned());
                }
            };
        }
    }
}
