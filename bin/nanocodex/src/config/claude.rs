use super::*;
use nanocodex::{
    Claude,
    claude::{
        ClaudeClient, ClaudeToolReply, ClaudeTools, Effort, ToolDefinition, ToolResultContent,
    },
    claude_tools::{
        BashRequest, BashResult, ClaudeBash, ClaudeWorkspaceFiles, SandboxBashExecutor,
    },
    tools::{
        ToolContext, ToolInput,
        contract::{ToolOutputBody, ToolOutputContent},
        runtime::ToolRuntime,
        workspace_runtime::WorkspaceToolRuntime,
    },
};
use serde_json::{Value, json, value::to_raw_value};
use tokio::time::{Duration, Instant};

/// Resolve Claude credentials only when its family is used, then share the
/// native client and its refresh gate across every root and child session.
#[derive(Clone)]
pub(super) struct ClaudeConnection {
    auth: crate::auth::ClaudeAuthArgs,
    api_key: Option<String>,
    endpoint: Option<String>,
    client: Arc<tokio::sync::OnceCell<ClaudeClient>>,
}

impl ClaudeConnection {
    pub(super) fn new(
        auth: crate::auth::ClaudeAuthArgs,
        api_key: Option<String>,
        endpoint: Option<String>,
    ) -> Self {
        Self {
            auth,
            api_key,
            endpoint,
            client: Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    async fn client(&self) -> std::result::Result<ClaudeClient, String> {
        self.client
            .get_or_try_init(|| async {
                self.auth
                    .clone()
                    .client(self.api_key.clone(), self.endpoint.clone())
                    .await
                    .map_err(|error| error.to_string())
            })
            .await
            .cloned()
    }
}

impl AgentArgs {
    pub(super) async fn build_claude(
        self,
        durable: Option<DurableSession>,
        vm: VmArgs,
        tui: bool,
        local_durability: Option<LocalDurability>,
        requested_model: Option<HarnessModel>,
    ) -> Result<ConfiguredAgent> {
        let responses_transport = self.responses_transport();
        let web_search = self.web_search();
        let model = requested_model.unwrap_or_else(|| HarnessFamily::Claude.default_model());
        let thinking = self
            .model_policy
            .requested_thinking(HarnessFamily::Claude)?
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
                "Codex rollouts cannot be resumed with --harness claude; use a native Claude durability store"
            ));
        }
        if vm.is_enabled() {
            return Err(eyre!("the native Claude CLI does not yet support --vm"));
        }
        if self.mpp.is_enabled() {
            return Err(eyre!(
                "the native Claude CLI does not yet support the Tempo provider"
            ));
        }
        if self.memory {
            return Err(eyre!("the native Claude CLI does not yet support --memory"));
        }
        let connection = ClaudeConnection::new(
            self.claude_auth,
            self.claude_api_key,
            self.claude_messages_url,
        );
        let client = connection.client().await.map_err(|error| eyre!(error))?;
        let workspace = self
            .cwd
            .unwrap_or_else(|| PathBuf::from("."))
            .canonicalize()
            .wrap_err("failed to resolve the Claude workspace")?;
        let files = Arc::new(ClaudeWorkspaceFiles::new(&workspace).map_err(|error| eyre!(error))?);
        let codex_home = default_codex_home()?;
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
        // Open the root before registering recipes so every family captures
        // the same child store and owned registry barrier.
        let persistence = local_durability.or_else(|| {
            self.rollouts.then(|| LocalDurability {
                path: codex_home.join("claude/sessions.sqlite"),
                state_id: SessionId::new().to_string(),
            })
        });
        let durability = match persistence {
            Some(persistence) => Some(
                durability::CliDurability::open(
                    persistence,
                    HarnessFamily::Claude,
                    None,
                    registry
                        .as_ref()
                        .map(|(registry, _, _)| Arc::clone(registry)),
                )
                .await?,
            ),
            None => None,
        };
        let tool_registry = registry
            .as_ref()
            .map(|(registry, _, _)| Arc::clone(registry));
        let instructions = self.instructions.unwrap_or_else(|| {
            "You are a coding agent. Use the native Claude workspace tools to inspect and change the authorized workspace. Bash runs foreground commands through the retained workspace host. Use exec for MCP and shared subagent tools.".to_owned()
        });
        let codex_auth = self.auth.resolve().map_err(|error| error.to_string());
        let codex_tools = tools
            .clone()
            .into_builder()
            .workspace(true)
            .web_search(web_search)
            .image_generation(self.image_generation)
            .build()?;
        let codex_home_for_recipe = codex_home.clone();
        let codex_workspace = workspace.clone();
        let codex_registry = tool_registry.as_ref().map(Arc::downgrade);
        let codex_instructions = instructions.clone();
        let websocket_url = self.websocket_url;
        let api_base_url = self.api_base_url;
        let model_id_prefix = self.model_id_prefix;
        let reasoning_mode = self.reasoning_mode;
        let fast_mode = self.fast_mode;
        let websocket_warmup = self.websocket_warmup;
        let store_responses = self.store_responses;
        let child_durability = durability.clone();
        let harness_builder =
            nanocodex::Harness::builder().register(HarnessFamily::Codex, move |request| {
                let durability = child_durability.clone();
                let auth = codex_auth.clone();
                let tools = codex_tools.clone();
                let workspace = codex_workspace.clone();
                let codex_home = codex_home_for_recipe.clone();
                let registry = codex_registry.clone();
                let instructions = codex_instructions.clone();
                let websocket_url = websocket_url.clone();
                let api_base_url = api_base_url.clone();
                let model_id_prefix = model_id_prefix.clone();
                async move {
                    let HarnessModel::Codex(model) = request.model else {
                        return Err(nanocodex::NanocodexError::InvalidRequest(
                            "Codex recipe received a Claude model".into(),
                        ));
                    };
                    let auth = auth
                        .map_err(nanocodex::NanocodexError::InvalidRequest)?
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
                    let mut builder = Nanocodex::builder(client)
                        .workspace(workspace)
                        .codex_home(codex_home)
                        .model(model)
                        .thinking(request.thinking)
                        .reasoning_mode(reasoning_mode)
                        .fast_mode(fast_mode)
                        .host_context(request.host_context)
                        .instructions(instructions)
                        .spawn_factory(request.spawn_factory)
                        .tools_factory(move |parent| {
                            if let Some(registry) = &registry {
                                nanocodex_subagents::install_tools(
                                    tools.clone(),
                                    parent,
                                    registry.upgrade().expect("live CLI owns child registry"),
                                )
                            } else {
                                Ok(tools.clone())
                            }
                        });
                    if let Some(durability) = durability {
                        builder = durability.codex_child(builder, request.snapshot).await?;
                    } else if let Some(checkpoint) = request.snapshot {
                        builder = builder.restore_runtime(checkpoint)?;
                    }
                    builder.build()
                }
            });
        let harness = register_claude_recipe(
            harness_builder,
            connection,
            workspace.clone(),
            instructions.clone(),
            tools.clone(),
            web_search,
            tool_registry.clone(),
            durability.clone(),
        )
        .build();
        let mut builder = configured_claude_builder(
            client,
            model,
            thinking,
            workspace,
            files,
            instructions,
            tools,
            web_search,
            tool_registry,
        )
        .spawn_factory(harness.spawn_factory());
        if let Some(durability) = durability {
            builder = durability.claude_root(builder).await?;
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

#[allow(clippy::too_many_arguments)]
fn configured_claude_builder(
    client: ClaudeClient,
    model: HarnessModel,
    thinking: Thinking,
    workspace: PathBuf,
    files: Arc<ClaudeWorkspaceFiles>,
    mut instructions: String,
    tools: Tools,
    web_search: bool,
    registry: Option<Arc<nanocodex_subagents::Registry>>,
) -> nanocodex::claude::ClaudeBuilder {
    if registry.is_some() && !instructions.contains(SUBAGENT_INSTRUCTIONS) {
        instructions.push_str("\n\n");
        instructions.push_str(SUBAGENT_INSTRUCTIONS);
    }
    // The native Claude factory retains this tools recipe after construction.
    // Installed tools retain the live registry, so the recipe must be weak.
    let registry = registry.as_ref().map(Arc::downgrade);
    let mut builder = Nanocodex::builder(Claude::new(client, model.as_str()))
        .workspace(workspace.to_string_lossy().into_owned())
        .system(instructions)
        .max_tokens(16_384)
        .tools_factory(move |parent| {
            let tools = if let Some(registry) = &registry {
                nanocodex_subagents::install_tools(
                    tools.clone(),
                    parent,
                    registry
                        .upgrade()
                        .ok_or(nanocodex::NanocodexError::AgentStopped)?,
                )
                .map_err(|error| nanocodex::NanocodexError::InvalidRequest(error.to_string()))?
            } else {
                tools.clone()
            };
            native_tools(&workspace, Arc::clone(&files), tools)
        });
    if let Some(effort) = claude_effort(thinking) {
        builder = builder.adaptive_thinking().keep_thinking().effort(effort);
    }
    if web_search {
        builder = builder.server_tool(nanocodex::claude::ServerToolDefinition::web_search_basic(5));
    }
    builder
}

#[allow(clippy::too_many_arguments)]
pub(super) fn register_claude_recipe(
    harness: nanocodex::HarnessBuilder,
    connection: ClaudeConnection,
    workspace: PathBuf,
    instructions: String,
    tools: Tools,
    web_search: bool,
    registry: Option<Arc<nanocodex_subagents::Registry>>,
    durability: Option<durability::CliDurability>,
) -> nanocodex::HarnessBuilder {
    let registry = registry.as_ref().map(Arc::downgrade);
    harness.register(HarnessFamily::Claude, move |request| {
        let durability = durability.clone();
        let connection = connection.clone();
        let workspace = workspace.clone();
        let instructions = instructions.clone();
        let tools = tools.clone();
        let registry = registry.clone();
        async move {
            let registry = registry
                .as_ref()
                .map(|registry| {
                    registry
                        .upgrade()
                        .ok_or(nanocodex::NanocodexError::AgentStopped)
                })
                .transpose()?;
            let client = connection
                .client()
                .await
                .map_err(nanocodex::NanocodexError::InvalidRequest)?;
            let files = Arc::new(
                ClaudeWorkspaceFiles::new(&workspace)
                    .map_err(nanocodex::NanocodexError::InvalidRequest)?,
            );
            let mut builder = configured_claude_builder(
                client,
                request.model,
                request.thinking,
                workspace,
                files,
                instructions,
                tools,
                web_search,
                registry,
            )
            .spawn_factory(request.spawn_factory)
            .host_context(request.host_context);
            if let Some(durability) = durability {
                builder = durability.claude_child(builder, request.snapshot).await?;
            } else if let Some(checkpoint) = request.snapshot {
                builder = builder.restore_runtime(checkpoint)?;
            }
            builder.build()
        }
    })
}

fn claude_effort(thinking: Thinking) -> Option<Effort> {
    match thinking {
        Thinking::None => None,
        Thinking::Low => Some(Effort::Low),
        Thinking::Medium => Some(Effort::Medium),
        Thinking::High => Some(Effort::High),
        Thinking::Xhigh => Some(Effort::Xhigh),
        Thinking::Max => Some(Effort::Max),
    }
}

fn native_tools(
    workspace: &Path,
    files: Arc<ClaudeWorkspaceFiles>,
    tools: Tools,
) -> nanocodex::agent::Result<ClaudeTools> {
    let mut native = ClaudeTools::new();
    for schema in ClaudeWorkspaceFiles::definitions() {
        let definition: ToolDefinition =
            serde_json::from_value(schema).expect("native file schema");
        let name = definition.name.clone();
        let files = Arc::clone(&files);
        native = native.tool_with_context(definition, move |input, _| {
            let files = Arc::clone(&files);
            let name = name.clone();
            async move { files.execute(&name, input).await.map(text_reply) }
        });
    }
    let shell = Arc::new(ClaudeBash::new(RetainedBash {
        runtime: Arc::new(WorkspaceToolRuntime::new(workspace.to_path_buf())),
        gate: Arc::new(tokio::sync::Mutex::new(())),
    }));
    for schema in ClaudeBash::<RetainedBash>::definitions() {
        let definition = serde_json::from_value(schema).expect("native Bash schema");
        let shell = Arc::clone(&shell);
        native = native.tool_with_context(definition, move |input, _| {
            let shell = Arc::clone(&shell);
            async move { shell.execute("Bash", input).await.map(text_reply) }
        });
    }
    // The retained host exposes real MCP discovery and calls through Code Mode.
    // Only tool contracts cross this boundary; Claude conversations stay native.
    let runtime = Arc::new(RetainedHost(ToolRuntime::new_with_tools(
        workspace, None, None, &tools,
    )));
    let descriptions = runtime.model_specs("native-claude");
    for (name, description, schema) in [
        (
            "exec",
            "Execute JavaScript against the retained MCP and subagent capabilities. Use text(value) to return output.",
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
            .map_or(description, |definition| definition.description());
        let definition: ToolDefinition = serde_json::from_value(
            json!({"name":name,"description":description,"input_schema":schema}),
        )
        .expect("native host schema");
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
                .with_host_context(invocation.host_context.as_deref())
                .with_instruction_revision(invocation.instruction_revision);
                if name == "tool_search" {
                    let input = ToolInput::Function(
                        to_raw_value(&input).map_err(|error| error.to_string())?,
                    );
                    let output = runtime
                        .execute_tool(name, input, context)
                        .await
                        .map_err(|error| error.to_string())?;
                    let mut reply = runtime_reply(&output.output, output.success)?;
                    reply.structured_result = Some(output.structured_result());
                    reply.metadata = output
                        .metadata
                        .as_ref()
                        .and_then(|metadata| serde_json::from_str(metadata.get()).ok());
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
                .map_err(|error| error.to_string())?;
                runtime_reply(&execution.output, execution.success)
            }
        });
    }
    Ok(native)
}

fn text_reply(text: String) -> ClaudeToolReply {
    ClaudeToolReply::success(ToolResultContent::Text(text))
}

fn runtime_reply(
    output: &ToolOutputBody,
    success: bool,
) -> std::result::Result<ClaudeToolReply, String> {
    let content =
        match output {
            ToolOutputBody::Text(text) => ToolResultContent::Text(text.clone()),
            ToolOutputBody::Content(items) => {
                let mut blocks = Vec::new();
                for item in items {
                    blocks.push(match item {
                        ToolOutputContent::InputText { text } => json!({"type":"text","text":text}),
                        ToolOutputContent::InputImage { image_url, .. } => {
                            let source = if image_url.starts_with("data:") {
                                let (header, data) = image_url
                                    .split_once(',')
                                    .ok_or("invalid host image data URL")?;
                                let media_type = header
                                    .strip_prefix("data:")
                                    .and_then(|header| header.strip_suffix(";base64"))
                                    .ok_or("host image must use base64 encoding")?;
                                json!({"type":"base64","media_type":media_type,"data":data})
                            } else {
                                json!({"type":"url","url":image_url})
                            };
                            json!({"type":"image","source":source})
                        }
                        ToolOutputContent::InputImageFile { .. }
                        | ToolOutputContent::InputAudio { .. }
                        | ToolOutputContent::EncryptedContent { .. } => return Err(
                            "this host tool result cannot be represented in native Claude Messages"
                                .into(),
                        ),
                    });
                }
                ToolResultContent::Blocks(blocks)
            }
        };
    let mut reply = ClaudeToolReply::success(content);
    reply.is_error = !success;
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
    async fn execute(&self, request: BashRequest) -> std::result::Result<BashResult, String> {
        let gate = Arc::clone(&self.gate).lock_owned().await;
        let mut cleanup = CancelShell {
            runtime: Some(Arc::clone(&self.runtime)),
            gate: Some(gate),
        };
        let deadline = Instant::now() + Duration::from_millis(request.timeout_ms);
        let context = ToolContext::new("claude", "native-bash", "bash", &[], 1024);
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
            let remaining = request.max_stdout_bytes.saturating_sub(stdout.len());
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
                return Ok(BashResult {
                    stdout,
                    stderr: String::new(),
                    exit_code: i32::try_from(code).map_err(|e| e.to_string())?,
                    truncated,
                });
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
