use super::*;

mod agents;
mod checkpoints;
mod code_mode;
pub(crate) mod frontend;
mod loop_frontend;
mod permissions;
pub(crate) mod scheduler;
mod shared_tools;
mod workflow;
mod worktree;
pub(crate) use checkpoints::rewind as rewind_files;

/// Preserve current host restrictions before a fresh rewind journal is published.
pub(crate) fn prepare_rewind_branch(
    home: &std::path::Path,
    source: &str,
    target: &str,
) -> std::result::Result<(), String> {
    interaction::prepare_rewind_branch(home, source, target)
        .map_err(|error| format!("{error:#}"))?;
    worktree::prepare_rewind_branch(home, source, target)
}
mod hooks;
pub(crate) mod interaction;
mod mcp;
mod monitor;
mod skills;
mod web;
use nanocodex::{
    Claude,
    claude::{
        ClaudeClient, ClaudeToolReply, ClaudeTools, Effort, ToolDefinition, ToolResultContent,
    },
    claude_tools::ClaudeWorkspaceFiles,
    tools::{
        ToolContext, ToolInput,
        contract::{ToolOutputBody, ToolOutputContent},
        runtime::ToolRuntime,
    },
};
use serde_json::{Value, json, value::to_raw_value};
use tokio::time::Duration;

/// Task-tree workspace bindings. A new child snapshots its parent's current
/// directory once; later transitions never retarget an existing child's tools.
pub(super) struct WorkspaceRegistry {
    home: PathBuf,
    sessions: std::sync::Mutex<std::collections::BTreeMap<String, Arc<worktree::Workspace>>>,
    policies: std::sync::Mutex<
        std::collections::BTreeMap<String, std::sync::Weak<interaction::Interaction>>,
    >,
}
impl WorkspaceRegistry {
    pub(super) fn new(_initial: PathBuf, home: PathBuf) -> Self {
        Self {
            home,
            sessions: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            policies: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        }
    }
    pub(super) fn seed(&self, session: &str, initial: PathBuf) -> std::result::Result<(), String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "workspace registry poisoned")?;
        if !sessions.contains_key(session) {
            sessions.insert(
                session.to_owned(),
                Arc::new(worktree::Workspace::new(
                    initial,
                    self.home.clone(),
                    session,
                )?),
            );
        }
        if let Some(workspace) = sessions.get(session) {
            agents::profiles::restore(session, workspace.clone())?;
        }
        Ok(())
    }
    fn get(&self, session: &str) -> std::result::Result<Arc<worktree::Workspace>, String> {
        self.sessions
            .lock()
            .map_err(|_| "workspace registry poisoned")?
            .get(session)
            .cloned()
            .ok_or_else(|| "session workspace has not been initialized".into())
    }
    pub(super) fn current(&self, session: &str) -> std::result::Result<PathBuf, String> {
        Ok(self.get(session)?.current())
    }
    pub(super) fn initialize(&self, parent: &str, child: &str) -> std::result::Result<(), String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "workspace registry poisoned")?;
        if !sessions.contains_key(child) {
            let owner = sessions
                .get(parent)
                .ok_or("parent workspace has not been initialized")?;
            let workspace = Arc::new(match workflow::inherited_workspace() {
                Some((path, lease)) => {
                    worktree::Workspace::child_from_pin(self.home.clone(), child, path, lease)?
                }
                None => owner.child(self.home.clone(), child)?,
            });
            agents::profiles::bind(parent, child, workspace.clone())?;
            sessions.insert(child.to_owned(), workspace);
        }
        Ok(())
    }
    fn bind_policy(
        &self,
        session: &str,
        interaction: &Arc<interaction::Interaction>,
    ) -> std::result::Result<(), String> {
        // Validate restored rules before publishing a model-visible tool catalog.
        interaction.resolved_policy(session)?;
        self.policies
            .lock()
            .map_err(|_| "workspace policies poisoned")?
            .insert(session.into(), Arc::downgrade(interaction));
        Ok(())
    }
    pub(super) fn parent_policy(
        &self,
        parent: Option<&nanocodex::agent::AgentHandle>,
    ) -> std::result::Result<permissions::Policy, String> {
        let policy = match parent {
            Some(parent) => self
                .policies
                .lock()
                .map_err(|_| "workspace policies poisoned")?
                .get(parent.session_id())
                .cloned(),
            None => None,
        };
        match (parent, policy.and_then(|policy| policy.upgrade())) {
            (Some(parent), Some(policy)) => policy.resolved_policy(parent.session_id()),
            _ => Ok(permissions::Policy::default()),
        }
    }
    pub(super) fn authorize_cross_family(
        &self,
        parent: Option<&nanocodex::agent::AgentHandle>,
    ) -> std::result::Result<(), String> {
        if self.parent_policy(parent)?.restricted() {
            Err("cross-family delegation is unavailable under a restricted Claude permission policy; use a Claude child to retain enforcement".into())
        } else {
            Ok(())
        }
    }
    pub(super) fn parent_path(
        &self,
        parent: Option<&nanocodex::agent::AgentHandle>,
        fallback: &Path,
    ) -> std::result::Result<PathBuf, String> {
        match parent {
            Some(parent) => self.current(parent.session_id()),
            None => Ok(fallback.to_path_buf()),
        }
    }
}

/// Read the same durable workspace binding used by native tools without
/// acquiring the active conversation lock (also safe for user steering).
pub(super) fn current_session_workspace(session_id: &str) -> std::result::Result<PathBuf, String> {
    let home = default_codex_home().map_err(|error| error.to_string())?;
    worktree::Workspace::saved_current(&home, session_id)
}

/// Resolve Claude credentials only when its family is used, then share the
/// native client and its refresh gate across every root and child session.
#[derive(Clone)]
pub(super) struct ClaudeConnection {
    auth: crate::auth::ClaudeAuthArgs,
    api_key: Option<String>,
    endpoint: Option<String>,
    client: Arc<tokio::sync::OnceCell<ClaudeClient>>,
    hooks_path: Option<PathBuf>,
    policy: permissions::Policy,
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
            hooks_path: None,
            policy: permissions::Policy::default(),
        }
    }

    pub(super) fn with_hooks(mut self, path: Option<PathBuf>) -> Self {
        self.hooks_path = path;
        self
    }

    pub(super) fn with_permission_config(
        mut self,
        path: Option<&Path>,
        mode: Option<&str>,
    ) -> Result<Self> {
        self.policy = permissions::Policy::load(path, mode)?;
        Ok(self)
    }

    fn hooks(
        &self,
        workspaces: Arc<WorkspaceRegistry>,
    ) -> Result<Option<Arc<dyn nanocodex::claude::ClaudeToolHooks>>> {
        self.hooks_path
            .as_deref()
            .map(|path| {
                hooks::load_with_workspace(path, Arc::new(move |id| workspaces.current(id)))
            })
            .transpose()
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

/// Claude's view of the shared host catalog: the same workspace shell tools
/// (exec_command/write_stdin) as Codex, without Codex's Responses built-ins,
/// which Claude replaces with its native equivalents.
pub(super) fn host_tools(tools: &Tools) -> Result<Tools> {
    Ok(tools
        .clone()
        .into_builder()
        .web_search(false)
        .image_generation(false)
        .build()?)
}

impl AgentArgs {
    pub(super) async fn build_claude(
        self,
        root: RootSession,
        codex_home: PathBuf,
        vm: VmArgs,
        tui: bool,
        requested_model: Option<HarnessModel>,
    ) -> Result<ConfiguredAgent> {
        let web_search = self.web_search();
        let model = requested_model.unwrap_or_else(|| HarnessFamily::Claude.default_model());
        let thinking = self
            .model_policy
            .requested_thinking(HarnessFamily::Claude)?
            .unwrap_or_else(|| model.default_thinking());
        if !model.supports_thinking(thinking) {
            return Err(eyre!("model {model} does not support thinking {thinking}"));
        }
        // Provider necessities: the VM workspace is served through Codex's
        // workspace tools, and Tempo proxies OpenAI Responses.
        if vm.is_enabled() {
            return Err(eyre!(
                "--vm requires the Codex harness; the Claude harness has no VM workspace tools"
            ));
        }
        if self.mpp.is_enabled() {
            return Err(eyre!(
                "the Tempo provider serves OpenAI Responses and requires the Codex harness"
            ));
        }
        let connection = self.claude_connection()?;
        let client = connection.client().await.map_err(|error| eyre!(error))?;
        let session_id = root.session_id.clone();
        let workspaces = Arc::new(WorkspaceRegistry::new(
            root.workspace.clone(),
            codex_home.clone(),
        ));
        workspaces
            .seed(&session_id, root.workspace.clone())
            .map_err(|error| eyre!(error))?;
        let workspace = workspaces
            .current(&session_id)
            .map_err(|error| eyre!(error))?;
        let claude_scheduler = scheduler::SessionScheduler::enabled(tui)
            .then(|| Arc::new(scheduler::SessionScheduler::new(codex_home.clone())));
        let managed_memory = self.managed_memory(&codex_home, &session_id).await?;
        let (catalog, mcp_handle) = self
            .host_tools(&codex_home, None, None, managed_memory.as_ref())
            .await?;
        let tools = host_tools(&catalog)?;
        let registry = self
            .subagents
            .then(|| subagents::channel(self.max_subagents));
        let tool_registry = registry
            .as_ref()
            .map(|(registry, _, _)| Arc::clone(registry));
        let responses = self.responses_settings();
        let codex_auth = self.auth.clone();
        let codex = self.codex_recipe(
            HarnessFamily::Claude,
            codex::CodexConnection::lazy(move || {
                responses.client(codex_auth.clone().resolve()?.nanocodex()?, None)
            }),
            catalog.clone(),
            &codex_home,
            &workspace,
            &workspaces,
            tool_registry.clone(),
            managed_memory.is_some(),
        );
        let harness = self
            .harness(
                codex,
                connection.clone(),
                &catalog,
                &workspace,
                tool_registry.clone(),
                mcp_handle.clone(),
                &workspaces,
            )?
            .build();
        let tool_hooks = connection.hooks(workspaces.clone())?;
        let permission_workspaces = workspaces.clone();
        let (interaction, interactions) = interaction::Interaction::new_with_policy_and_workspace(
            interaction::Interaction::available(tui),
            codex_home.join("claude/plan-mode"),
            tool_hooks,
            connection.policy.clone(),
            Arc::new(move |id| permission_workspaces.current(id)),
        );
        let mut builder = configured_claude_builder(
            client,
            model,
            thinking,
            workspace,
            self.instructions.clone(),
            tools,
            web_search,
            tool_registry,
            mcp_handle.clone(),
            interaction.clone(),
            codex_home.clone(),
            workspaces.clone(),
            session_id,
            claude_scheduler.clone(),
            self.claude_workflows,
            self.claude_monitor_ws_origin.clone(),
        )
        // Validated by check_model_settings; a recorded boundary overrides it.
        .fast_mode(self.fast_mode())
        .spawn_factory(harness.spawn_factory());
        if let Some(persistence) = &root.persistence {
            // The same Codex-format JSONL mirror as Codex roots, for this
            // session and every fork, side conversation and subagent.
            if let Some(mirror) = persistence.mirror() {
                builder = builder.rollout(mirror);
            }
            let state = persistence.open(model, &root.workspace).await?;
            builder = builder
                .durability(state)
                .await
                .wrap_err("failed to attach session durability")?;
        }
        let (handle, events) = {
            let _timing = crate::startup_timing::Stage::new("native_agent");
            builder.build()?
        };
        if let Some(scheduler) = &claude_scheduler {
            scheduler
                .resume(handle.session_id())
                .map_err(|error| eyre!(error))?;
            frontend::register(
                handle.session_id(),
                scheduler,
                &workspaces,
                &interaction,
                std::env::var_os("HOME").map(PathBuf::from),
            )
            .map_err(|error| eyre!(error))?;
        }
        let (child_agents, subagent_updates) = super::child_agents(&handle, registry, tui);
        // Voice needs OpenAI realtime credentials whatever the agent family.
        let realtime = self
            .auth
            .clone()
            .resolve()
            .and_then(|auth| auth.nanocodex())
            .and_then(|auth| Ok(OpenAi::new(auth)?))
            .ok();
        Ok(ConfiguredAgent {
            host: HostChannels {
                interactions,
                scheduler: claude_scheduler,
            },
            handle,
            events,
            realtime,
            child_agents,
            subagent_updates,
            mpp_adapter: None,
            mcp: mcp_handle,
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
    instructions: Option<String>,
    tools: Tools,
    web_search: bool,
    registry: Option<Arc<nanocodex_subagents::Registry>>,
    mcp_handle: Option<McpHandle>,
    interaction: Arc<interaction::Interaction>,
    codex_home: PathBuf,
    workspaces: Arc<WorkspaceRegistry>,
    session_id: String,
    scheduler: Option<Arc<scheduler::SessionScheduler>>,
    workflows_enabled: bool,
    monitor_ws_origins: Vec<String>,
) -> nanocodex::claude::ClaudeBuilder {
    let load_context = interaction
        .resolved_policy(&session_id)
        .is_ok_and(|policy| !policy.has_read_restrictions())
        && agents::profiles::allows_context(&session_id);
    let initial_instructions = agents::profiles::instructions(
        &session_id,
        super::instructions::native_with_context(
            HarnessFamily::Claude,
            instructions.clone(),
            &workspace,
            web_search,
            registry.is_some(),
            load_context,
        ),
    );
    let profile_guard = Arc::new(agents::profiles::Guard {
        workspaces: workspaces.clone(),
        registry: registry.as_ref().map(Arc::downgrade),
    });
    let interaction_tools = interaction.clone();
    let interaction_children = interaction.clone();
    let interaction_context = interaction.clone();
    let checkpoint_workspaces = workspaces.clone();
    let checkpoints = Arc::new(checkpoints::Checkpoints::new_with_workspace(
        Arc::new(move |id| checkpoint_workspaces.current(id)),
        codex_home,
    ));
    let workspace_labels = workspaces.clone();
    let child_workspaces = workspaces.clone();
    let system_workspaces = workspaces.clone();
    let has_registry = registry.is_some();
    let schedule_owner = session_id.clone();
    let mut builder = Nanocodex::builder(Claude::new(client, model.as_str()))
        .subagent_type_resolver(agents::profiles::selected_name)
        .session_id(session_id)
        .workspace(workspace.to_string_lossy().into_owned())
        .workspace_resolver(move |id| {
            workspace_labels
                .current(id)
                .expect("initialized session workspace")
                .to_string_lossy()
                .into_owned()
        })
        .child_workspace_init(move |parent, child| {
            child_workspaces
                .initialize(parent, child)
                .map_err(nanocodex::NanocodexError::InvalidRequest)?;
            interaction_children
                .initialize_child(parent, child)
                .map_err(nanocodex::NanocodexError::InvalidRequest)
        })
        .system(initial_instructions)
        .system_resolver(move |id| {
            agents::profiles::instructions(
                id,
                super::instructions::native_with_context(
                    HarnessFamily::Claude,
                    instructions.clone(),
                    &system_workspaces
                        .current(id)
                        .expect("initialized context workspace"),
                    web_search,
                    has_registry,
                    interaction_context
                        .resolved_policy(id)
                        .is_ok_and(|policy| !policy.has_read_restrictions())
                        && agents::profiles::allows_context(id),
                ),
            )
        })
        .parallel_tools(false)
        .tool_hooks(profile_guard.clone())
        .tool_hooks(interaction)
        .tool_hooks(profile_guard)
        .tool_hooks(checkpoints)
        .tasks(Arc::new(nanocodex::claude_tools::ClaudeTasks::new()))
        .tools_factory(move |parent| {
            let workspace = workspaces
                .get(parent.session_id())
                .map_err(nanocodex::NanocodexError::InvalidRequest)?;
            workspaces
                .bind_policy(parent.session_id(), &interaction_tools)
                .map_err(nanocodex::NanocodexError::InvalidRequest)?;
            let owner_scheduler = scheduler
                .as_ref()
                .filter(|_| parent.session_id() == schedule_owner)
                .cloned();
            let monitor = owner_scheduler.as_ref().map(|scheduler| {
                Arc::new(monitor::Monitor::new(
                    workspace.clone(),
                    scheduler.clone(),
                    web_search,
                    monitor_ws_origins.clone(),
                ))
            });
            let workflow = if workflows_enabled && parent.session_id() == schedule_owner {
                registry.as_ref().map(|registry| {
                    Arc::new(workflow::Workflow::new(
                        workspace.clone(),
                        parent.clone(),
                        registry.clone(),
                    ))
                })
            } else {
                None
            };
            let tools = if let Some(registry) = &registry {
                nanocodex_subagents::install_tools(
                    tools.clone(),
                    parent.clone(),
                    Arc::clone(registry),
                )
                .map_err(|error| nanocodex::NanocodexError::InvalidRequest(error.to_string()))?
            } else {
                tools.clone()
            };
            let mut native = native_tools(
                workspace,
                &parent.session_environment(),
                tools,
                registry.is_some(),
                mcp_handle.clone(),
                interaction_tools.clone(),
                monitor,
                workflow,
            )?;
            if let Some(registry) = &registry {
                native =
                    nanocodex_subagents::install_claude_tools(native, parent, registry.clone())?;
            }
            native = interaction::install(native, interaction_tools.clone());
            if let Some(scheduler) = owner_scheduler {
                native = scheduler::install(native, scheduler);
            }
            Ok(native)
        });
    builder = builder.code_only(true).tools_adapter(code_mode::wrap);
    if let Some(effort) = claude_effort(thinking) {
        builder = builder.adaptive_thinking().keep_thinking().effort(effort);
    }
    if web_search {
        builder = builder
            .nested_web_search(false)
            .web_fetch_with_source(Arc::new(web::PublicWebFetch::new()), false);
    }
    builder
}

#[allow(clippy::too_many_arguments)]
pub(super) fn register_claude_recipe(
    harness: nanocodex::HarnessBuilder,
    connection: ClaudeConnection,
    workspace: PathBuf,
    instructions: Option<String>,
    tools: Tools,
    web_search: bool,
    registry: Option<Arc<nanocodex_subagents::Registry>>,
    mcp_handle: Option<McpHandle>,
    workspaces: Arc<WorkspaceRegistry>,
    fast_mode: bool,
) -> nanocodex::HarnessBuilder {
    harness.register(HarnessFamily::Claude, move |request| {
        let connection = connection.clone();
        let workspace = workspaces.parent_path(request.parent.as_ref(), &workspace);
        let workspaces = workspaces.clone();
        let instructions = instructions.clone();
        let tools = tools.clone();
        let registry = registry.clone();
        let mcp_handle = mcp_handle.clone();
        async move {
            let workspace = workspace.map_err(nanocodex::NanocodexError::InvalidRequest)?;
            // Reopened sessions keep their checkpointed or durable identity.
            let session_id = match (&request.checkpoint, &request.durable_state) {
                (Some(checkpoint), _) => checkpoint.session_id().to_owned(),
                (None, Some(state)) => state.state_id().to_owned(),
                (None, None) => SessionId::new().to_string(),
            };
            if let Some(parent) = &request.parent {
                workspaces.initialize(parent.session_id(), &session_id)
            } else {
                workspaces.seed(&session_id, workspace.clone())
            }
            .map_err(nanocodex::NanocodexError::InvalidRequest)?;
            let workspace = workspaces
                .current(&session_id)
                .map_err(nanocodex::NanocodexError::InvalidRequest)?;
            let policy = if connection.policy.explicit {
                connection.policy.clone()
            } else {
                workspaces
                    .parent_policy(request.parent.as_ref())
                    .map_err(nanocodex::NanocodexError::InvalidRequest)?
            };
            let hooks = connection
                .hooks(workspaces.clone())
                .map_err(|error| nanocodex::NanocodexError::InvalidRequest(error.to_string()))?;
            let codex_home = default_codex_home()
                .map_err(|error| nanocodex::NanocodexError::InvalidRequest(error.to_string()))?;
            let permission_workspaces = workspaces.clone();
            let (interaction, _) = interaction::Interaction::new_with_policy_and_workspace(
                false,
                codex_home.join("claude/plan-mode"),
                hooks,
                policy,
                Arc::new(move |id| permission_workspaces.current(id)),
            );
            let client = connection
                .client()
                .await
                .map_err(nanocodex::NanocodexError::InvalidRequest)?;
            let mut builder = configured_claude_builder(
                client,
                request.model,
                request.thinking,
                workspace,
                instructions,
                tools,
                web_search,
                registry,
                mcp_handle,
                interaction,
                codex_home,
                workspaces,
                session_id,
                None,
                false,
                Vec::new(),
            )
            .spawn_factory(request.spawn_factory)
            .subagent_type("general-purpose")
            .host_context(request.host_context)
            // The session's fast preference where the child's model offers it;
            // a reopened checkpoint restores its own recorded setting.
            .fast_mode(fast_mode && request.model.supports_fast_mode());
            if let Some(checkpoint) = request.checkpoint {
                builder = builder.resume(checkpoint)?;
            }
            if let Some(state) = request.durable_state {
                builder = nanocodex::DurableAgentExt::durability(builder, state).await?;
            }
            builder.build()
        }
    })
}

const fn claude_effort(thinking: Thinking) -> Option<Effort> {
    match thinking {
        Thinking::None => None,
        Thinking::Low => Some(Effort::Low),
        Thinking::Medium => Some(Effort::Medium),
        Thinking::High => Some(Effort::High),
        Thinking::Xhigh => Some(Effort::Xhigh),
        Thinking::Max => Some(Effort::Max),
    }
}

// Keep host-owned capabilities explicit at this single assembly boundary.
#[allow(clippy::too_many_arguments)]
fn native_tools(
    workspace: Arc<worktree::Workspace>,
    session: &nanocodex::tools::SessionEnvironment,
    tools: Tools,
    subagents: bool,
    mcp_handle: Option<McpHandle>,
    interaction: Arc<interaction::Interaction>,
    monitor: Option<Arc<monitor::Monitor>>,
    workflow: Option<Arc<workflow::Workflow>>,
) -> nanocodex::agent::Result<ClaudeTools> {
    let mut native = ClaudeTools::new();
    for schema in ClaudeWorkspaceFiles::definitions() {
        let definition: ToolDefinition =
            serde_json::from_value(schema).expect("native file schema");
        let name = definition.name.clone();
        let workspace = workspace.clone();
        let interaction = interaction.clone();
        native = native.tool_with_context(definition, move |input, invocation| {
            let workspace = workspace.clone();
            let name = name.clone();
            let interaction = interaction.clone();
            async move {
                let include_context = !interaction
                    .resolved_policy(&invocation.session_id)?
                    .has_read_restrictions()
                    && agents::profiles::allows_context(&invocation.session_id);
                workspace
                    .files()?
                    .execute_output_with_context(&name, input, include_context)
                    .await
                    .and_then(output_reply)
            }
        });
    }
    for schema in nanocodex::claude_tools::ClaudeNotebook::definitions() {
        let definition: ToolDefinition =
            serde_json::from_value(schema).expect("native notebook schema");
        let name = definition.name.clone();
        let workspace = workspace.clone();
        native = native.tool_with_context(definition, move |input, _| {
            let workspace = workspace.clone();
            let name = name.clone();
            async move {
                workspace
                    .notebook()?
                    .execute(&name, input)
                    .await
                    .map(text_reply)
            }
        });
    }
    native = skills::install(native, workspace.clone())?;
    native = worktree::install(native, workspace.clone());
    if let Some(handle) = mcp_handle {
        native = mcp::install(native, handle);
    }
    // Shared MCP stdio servers and host tools, including exec_command and
    // write_stdin, run with this session's identity.
    let direct_tools = tools
        .for_session(session)
        .into_builder()
        .exposure(nanocodex::tools::runtime::ToolExposure::DirectOnly)
        .build()
        .map_err(|error| nanocodex::NanocodexError::InvalidRequest(error.to_string()))?;
    let runtime = Arc::new(RetainedHost(ToolRuntime::new_with_tools(
        workspace.current(),
        None,
        None,
        &direct_tools,
    )));
    native = shared_tools::install(native, runtime.clone(), workspace.clone());
    if let Some(monitor) = &monitor {
        native = monitor::install(native, monitor.clone());
    }
    if let Some(workflow) = &workflow {
        native = workflow::install(native, workflow.clone());
    }
    native = agents::install(
        native,
        runtime,
        subagents,
        monitor,
        workspace,
        interaction,
        workflow,
    );
    Ok(native)
}

/// Converts a host tool-result document (a base64 data URL) into a Claude
/// `document` block with the same bounds and media checks that
/// `nanocodex-claude` applies to prompt documents.
fn document_block(file_data: &str) -> std::result::Result<Value, String> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    const MAX_DOCUMENT_BYTES: usize = 10 * 1024 * 1024;
    if file_data.len() > MAX_DOCUMENT_BYTES.div_ceil(3) * 4 + 64 {
        return Err("Claude document exceeds 10 MiB".into());
    }
    let (header, data) = file_data
        .strip_prefix("data:")
        .and_then(|value| value.split_once(','))
        .ok_or("Claude documents require a base64 data URL")?;
    let media_type = header
        .strip_suffix(";base64")
        .ok_or("Claude document data URL must use base64")?;
    let bytes = STANDARD
        .decode(data)
        .map_err(|_| "invalid Claude document base64")?;
    if bytes.is_empty() || bytes.len() > MAX_DOCUMENT_BYTES {
        return Err("Claude document must contain 1 byte through 10 MiB".into());
    }
    let source = match media_type {
        "application/pdf" if bytes.starts_with(b"%PDF-") => {
            json!({"type":"base64","media_type":"application/pdf","data":data})
        }
        "application/pdf" => {
            return Err("Claude document media type does not match its bytes".into());
        }
        "text/plain" => {
            let text =
                String::from_utf8(bytes).map_err(|_| "Claude text document must be UTF-8")?;
            json!({"type":"text","media_type":"text/plain","data":text})
        }
        _ => return Err("Claude documents support application/pdf and text/plain".into()),
    };
    Ok(json!({"type":"document","source":source}))
}

fn output_reply(
    output: nanocodex::claude_tools::ToolOutput,
) -> std::result::Result<ClaudeToolReply, String> {
    use nanocodex::claude_tools::{ImageSource, ToolContent, ToolResultBlock};
    let content = match output.content {
        ToolContent::Text(text) => ToolResultContent::Text(text),
        ToolContent::Blocks(items) => {
            let mut blocks = Vec::with_capacity(items.len());
            for item in items {
                blocks.push(match item {
                    ToolResultBlock::Text { text } => json!({"type":"text","text":text}),
                    ToolResultBlock::Image { source } => {
                        let source = match source {
                            ImageSource::Base64 { media_type, data } => {
                                if !matches!(
                                    media_type.as_str(),
                                    "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                                ) {
                                    return Err("unsupported Claude image media type".into());
                                }
                                json!({"type":"base64","media_type":media_type,"data":data})
                            }
                            ImageSource::Url { url } => json!({"type":"url","url":url}),
                        };
                        json!({"type":"image","source":source})
                    }
                    ToolResultBlock::Document { file_data } => document_block(&file_data)?,
                    ToolResultBlock::UnsupportedMedia { media_type } => {
                        return Err(format!(
                            "host returned media unsupported by the Claude adapter: {media_type}"
                        ));
                    }
                });
            }
            ToolResultContent::Blocks(blocks)
        }
    };
    Ok(ClaudeToolReply {
        content,
        is_error: output.is_error,
        metadata: output.metadata,
        structured_result: output.structured_result,
    })
}

const fn text_reply(text: String) -> ClaudeToolReply {
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
