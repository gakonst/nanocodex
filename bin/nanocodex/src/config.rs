use std::{
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::Arc,
};

use clap::{ArgAction, Args, builder::NonEmptyStringValueParser};
use eyre::{Result, WrapErr, eyre};
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
use nanocodex::NanocodexBuilder;
use nanocodex::{
    AgentEvents, DurableAgentExt as _, HarnessFamily, HarnessModel, Model, Nanocodex, OpenAi,
    ReasoningMode, Thinking, Tools,
    agent::{
        rollout::{DurableSession, RolloutConfig},
        session::{SessionId, SessionSnapshot},
    },
    oai::{
        auth::{OpenAiAuth, OpenAiAuthMode},
        transport::ResponsesTransport,
    },
    tools::mcp::McpHandle,
};
use nanocodex_durability::{DurableSession as PortableDurableSession, SqliteStore};

use crate::browser::{BrowserArgs, ConfiguredBrowser};
use crate::login::load_managed_mcp_credential;
use crate::managed_memory::{ConfiguredManagedMemory, MEMORY_INSTRUCTIONS};
use crate::mcp::{ConfiguredMcp, McpArgs};
use crate::mpp::{MppAdapter, MppArgs};
use crate::subagents::{self, ChildAgents, DEFAULT_MAX_SUBAGENTS, SubagentToolSet};
use crate::vm::{ConfiguredVm, VmArgs};

mod claude;
pub(crate) use claude::frontend as claude_frontend;
pub(crate) use claude::interaction::{
    InteractionReceiver, PendingInteraction, serve_terminal as serve_claude_terminal,
};
pub(crate) use claude::scheduler::SessionScheduler;
pub(crate) use claude::{prepare_rewind_branch, rewind_files};
mod instructions;
pub(crate) use instructions::{expand_session_user_skill, expand_user_skill};

pub(crate) struct ConfiguredAgent {
    pub(crate) claude_scheduler: Option<Arc<SessionScheduler>>,
    pub(crate) claude_interactions: Option<InteractionReceiver>,
    pub(crate) handle: Nanocodex,
    pub(crate) events: AgentEvents,
    pub(crate) realtime: Option<OpenAi>,
    pub(crate) child_agents: Option<Arc<ChildAgents>>,
    pub(crate) subagent_updates:
        Option<tokio::sync::mpsc::UnboundedReceiver<nanocodex_subagents::ScopedAgentUpdate>>,
    pub(crate) mpp_adapter: Option<MppAdapter>,
    pub(crate) mcp: Option<McpHandle>,
    pub(crate) browser: Option<ConfiguredBrowser>,
    pub(crate) vm: Option<ConfiguredVm>,
    pub(crate) model: HarnessModel,
}

struct SessionBuild {
    workspace: PathBuf,
    session_id: Option<SessionId>,
    snapshot: Option<SessionSnapshot>,
    rollout: Option<RolloutConfig>,
}

/// Authentication flags shared by every direct-OpenAI CLI consumer.
#[derive(Args, Clone)]
pub(crate) struct AuthArgs {
    /// Explicit `OpenAI` API key override.
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    api_key: Option<String>,

    /// Explicitly use `ChatGPT` authorization from this credential file.
    #[arg(long, env = "NANOCODEX_AUTH_FILE")]
    auth_file: Option<PathBuf>,

    /// Use a persistent `ChatGPT` Business or Enterprise access token.
    #[arg(
        long,
        env = "CODEX_ACCESS_TOKEN",
        value_parser = NonEmptyStringValueParser::new()
    )]
    access_token: Option<String>,
}

/// Model-facing flags shared by normal agents and evaluator agents.
#[derive(Args, Clone)]
pub(crate) struct ModelArgs {
    /// Reasoning effort: none, low, medium, high, xhigh, or max.
    #[arg(long)]
    thinking: Option<Thinking>,

    /// Whether standalone web search is exposed to the model.
    #[arg(long, env = "NANOCODEX_WEB_SEARCH", action = ArgAction::Set)]
    web_search: Option<bool>,
}

/// The credential source selected once by the CLI and reusable by paired eval
/// implementations.
#[derive(Clone)]
pub(crate) enum SharedAuth {
    ApiKey(Arc<str>),
    AccessToken(Arc<str>),
    AuthFile(PathBuf),
}

impl ModelArgs {
    fn requested_thinking(&self, family: HarnessFamily) -> Result<Option<Thinking>> {
        if let Some(thinking) = self.thinking {
            return Ok(Some(thinking));
        }
        let variable = match family {
            HarnessFamily::Codex => "OPENAI_REASONING_EFFORT",
            HarnessFamily::Claude => "ANTHROPIC_REASONING_EFFORT",
        };
        std::env::var(variable)
            .ok()
            .map(|value| {
                value
                    .parse()
                    .map_err(|error: String| eyre!("{variable}: {error}"))
            })
            .transpose()
    }
}

/// The deliberately small standard-agent configuration accepted by eval
/// commands.
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[derive(Args)]
pub(crate) struct EvalAgentArgs {
    #[command(flatten)]
    auth: AuthArgs,

    #[command(flatten)]
    model_policy: ModelArgs,
}

#[derive(Args, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent CLI feature toggles are not one state machine"
)]
pub(crate) struct AgentArgs {
    /// Internal native resume identity; never accepted from arbitrary CLI flags.
    #[arg(skip)]
    pub(crate) claude_resume: Option<crate::native_sessions::ResumeSession>,

    /// Voice microphone shortcut, or none to use /voice mute only.
    #[arg(long, env = "NANOCODEX_VOICE_MUTE_KEY", default_value = "ctrl+x", value_parser = crate::tui::voice::validate_key)]
    pub(crate) voice_mute_key: String,

    /// Animate live voice captions; set false for reduced motion.
    #[arg(long, env = "NANOCODEX_VOICE_ANIMATIONS", default_value_t = true, action = clap::ArgAction::Set)]
    pub(crate) voice_animations: bool,

    /// How the TUI shows tool calls: expanded, folded, or hidden.
    ///
    /// Ctrl+O cycles through the modes. Hidden keeps only the conversation;
    /// the footer still shows the turn as Working until it ends.
    #[arg(long, env = "NANOCODEX_TOOL_CALLS", value_enum, default_value_t)]
    pub(crate) tool_calls: crate::tui::ToolCalls,

    #[command(flatten)]
    auth: AuthArgs,

    #[command(flatten)]
    pub(crate) claude_auth: crate::auth::ClaudeAuthArgs,

    /// Working directory exposed to the coding tools.
    #[arg(long)]
    cwd: Option<PathBuf>,

    #[command(flatten)]
    model_policy: ModelArgs,

    /// Select the native coding harness: codex or claude.
    #[arg(long, global = true, value_parser = ["codex", "claude"])]
    harness: Option<String>,

    /// Select the native Claude harness (shorthand for --harness claude).
    #[arg(long, global = true)]
    claude: bool,

    /// Model in the selected harness family. Defaults use OPENAI_MODEL or ANTHROPIC_MODEL.
    #[arg(long, global = true, value_parser = NonEmptyStringValueParser::new())]
    model: Option<String>,

    /// Explicit Anthropic Console API key override.
    #[arg(long, global = true, env = "ANTHROPIC_API_KEY", value_parser = NonEmptyStringValueParser::new(), hide_env_values = true)]
    claude_api_key: Option<String>,

    /// Native Anthropic Messages endpoint, including /v1/messages.
    #[arg(long, global = true, env = "ANTHROPIC_MESSAGES_URL", value_parser = NonEmptyStringValueParser::new())]
    claude_messages_url: Option<String>,

    /// Explicit JSON file enabling native Claude command hooks (Unix only).
    #[arg(long, global = true, value_name = "PATH")]
    claude_hooks: Option<PathBuf>,

    /// Enable bounded native multi-agent workflows for this Claude root session.
    #[arg(long, global = true)]
    claude_workflows: bool,

    /// Explicit private WebSocket origin allowed for native Monitor (repeatable).
    #[arg(long, global = true, value_name = "ORIGIN", value_parser = NonEmptyStringValueParser::new())]
    claude_monitor_ws_origin: Vec<String>,

    /// Explicit JSON file with Claude permissions allow/ask/deny rules.
    #[arg(long, global = true, value_name = "PATH")]
    claude_permissions: Option<PathBuf>,

    /// Native Claude admission mode (auto classifier mode is not implemented).
    #[arg(long, global = true, value_parser = ["full-access", "bypassPermissions", "default", "manual", "acceptEdits", "plan", "dontAsk"])]
    permission_mode: Option<String>,

    /// Optional namespace prepended to the model identifier on the wire.
    ///
    /// OpenAI routing gateways may use `openai`, producing identifiers such as
    /// `openai/gpt-6-astra` without changing Nanocodex's closed model policy.
    #[arg(long, env = "NANOCODEX_MODEL_ID_PREFIX")]
    model_id_prefix: Option<String>,

    /// Reasoning execution mode: standard or pro.
    #[arg(long, env = "OPENAI_REASONING_MODE", default_value_t)]
    reasoning_mode: ReasoningMode,

    /// Use priority processing for model requests.
    #[arg(
        long,
        env = "NANOCODEX_FAST_MODE",
        action = ArgAction::Set
    )]
    fast_mode: Option<bool>,

    /// Replace the standard system/developer instructions.
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    instructions: Option<String>,

    /// Whether image generation is exposed to the model.
    #[arg(
        long,
        env = "NANOCODEX_IMAGE_GENERATION",
        action = ArgAction::Set
    )]
    image_generation: Option<bool>,

    /// Whether the local command, patch, plan, and file tools are exposed.
    ///
    /// Set false when every workspace effect must go through MCP tools, for
    /// example when a remote sandbox is the workspace. Local computer-use
    /// tools are disabled with them.
    #[arg(
        long,
        env = "NANOCODEX_WORKSPACE_TOOLS",
        default_value_t = true,
        action = ArgAction::Set
    )]
    workspace_tools: bool,

    /// Whether clean, reusable Tact-style subagents are exposed in Code Mode.
    #[arg(
        long,
        env = "NANOCODEX_SUBAGENTS",
        default_value_t = true,
        action = ArgAction::Set
    )]
    subagents: bool,

    /// Maximum active subagent turns across one task tree (unlimited by default).
    #[arg(
        long,
        env = "NANOCODEX_MAX_SUBAGENTS",
        default_value_t = DEFAULT_MAX_SUBAGENTS
    )]
    max_subagents: usize,

    /// Record provider-native resumable sessions beneath `CODEX_HOME`.
    #[arg(
        long,
        env = "NANOCODEX_ROLLOUTS",
        default_value_t = true,
        action = ArgAction::Set
    )]
    rollouts: bool,

    /// Enable hosted Nanocodex session search and durable organization memory.
    #[arg(
        long,
        env = "NANOCODEX_MEMORY",
        default_value_t = false,
        action = ArgAction::Set
    )]
    memory: bool,

    /// Responses API WebSocket endpoint.
    #[arg(long, env = "OPENAI_RESPONSES_WEBSOCKET_URL")]
    websocket_url: Option<String>,

    /// Prime the Responses WebSocket before the first model request.
    #[arg(
        long,
        env = "NANOCODEX_WEBSOCKET_WARMUP",
        default_value_t = false,
        action = ArgAction::Set
    )]
    websocket_warmup: bool,

    /// Responses transport fixed for the complete agent session.
    ///
    /// Defaults to HTTPS for the Tempo provider and WebSocket for direct
    /// `OpenAI`.
    #[arg(long, env = "NANOCODEX_RESPONSES_TRANSPORT")]
    responses_transport: Option<ResponsesTransport>,

    /// Whether the Responses API retains server-side checkpoints.
    #[arg(long, env = "NANOCODEX_STORE_RESPONSES", action = ArgAction::Set)]
    store_responses: Option<bool>,

    /// `OpenAI` HTTP API base used by HTTPS Responses and in-process remote tools.
    #[arg(long, env = "OPENAI_API_BASE_URL")]
    api_base_url: Option<String>,

    #[command(flatten)]
    mcp: McpArgs,

    #[command(flatten)]
    mpp: MppArgs,

    #[command(flatten)]
    browser: BrowserArgs,
}

impl AgentArgs {
    /// A new, unused TUI session may choose a different native backend.
    pub(crate) fn select_tui_model(
        &mut self,
        model: HarnessModel,
        thinking: Thinking,
        fast_mode: bool,
    ) {
        let same_family = self.selected_harness().ok() == Some(model.family());
        self.harness = Some(model.family().to_string());
        self.claude = model.family() == HarnessFamily::Claude;
        self.model = Some(model.to_string());
        self.model_policy.thinking = Some(if same_family && model.supports_thinking(thinking) {
            thinking
        } else {
            model.default_thinking()
        });
        self.fast_mode =
            Some(model.family() == HarnessFamily::Codex && (!same_family || fast_mode));
    }

    pub(crate) fn resume_claude(
        mut self,
        session: crate::native_sessions::ResumeSession,
    ) -> Result<Self> {
        if !self.rollouts {
            return Err(eyre!(
                "Claude resume requires native persistence; remove --rollouts false"
            ));
        }
        let workspace = session.workspace.as_ref().or(self.cwd.as_ref())
            .ok_or_else(|| eyre!("legacy Claude session has no saved workspace; pass --cwd explicitly to resume it"))?
            .canonicalize().wrap_err("failed to resolve the resumed Claude workspace")?;
        if let Some(requested) = &self.cwd
            && requested
                .canonicalize()
                .wrap_err("failed to resolve --cwd")?
                != workspace
        {
            return Err(eyre!(
                "resumed Claude workspace is {}; --cwd requested {}",
                workspace.display(),
                requested.display()
            ));
        }
        // An environment default must never silently switch a resumed model.
        // An explicit --model remains a deliberate, family-validated override.
        if self.model.is_none() {
            self.model = Some(session.model.ok_or_else(|| eyre!("legacy Claude session has no saved model; pass --model explicitly to resume it"))?.to_string());
        }
        self.requested_model(HarnessFamily::Claude)?;
        self.cwd = Some(workspace);
        self.claude_resume = Some(session);
        Ok(self)
    }

    pub(crate) fn harness_model(&self) -> Result<HarnessModel> {
        let family = self.selected_harness()?;
        self.model_policy.requested_thinking(family)?;
        Ok(self
            .requested_model(family)?
            .unwrap_or_else(|| family.default_model()))
    }

    pub(crate) fn selected_harness(&self) -> Result<HarnessFamily> {
        match (self.claude, self.harness.as_deref()) {
            (true, Some(family)) if family != "claude" => {
                Err(eyre!("--claude conflicts with --harness {family}"))
            }
            (true, _) | (false, Some("claude")) => Ok(HarnessFamily::Claude),
            (false, Some(_)) => Ok(HarnessFamily::Codex),
            (false, None) => Ok(self.default_harness()),
        }
    }

    /// Claude Opus 5.5 (medium) is the preferred default. An explicit model or
    /// Responses-only configuration keeps its family, and Codex remains the
    /// fallback when no local Claude credential is configured.
    fn default_harness(&self) -> HarnessFamily {
        let explicit_model = self
            .model
            .as_deref()
            .and_then(|value| value.parse::<HarnessModel>().ok());
        if let Some(model) = explicit_model {
            return model.family();
        }
        if self.model.is_some() || std::env::var_os("OPENAI_MODEL").is_some() {
            return HarnessFamily::Codex;
        }
        if std::env::var_os("ANTHROPIC_MODEL").is_some() {
            return HarnessFamily::Claude;
        }
        let responses_only = self.memory
            || self.mpp.is_enabled()
            || self.model_id_prefix.is_some()
            || self.websocket_url.is_some()
            || self.api_base_url.is_some()
            || self.responses_transport.is_some()
            || self.store_responses.is_some()
            || self.reasoning_mode != ReasoningMode::Standard;
        if responses_only {
            return HarnessFamily::Codex;
        }
        if self.claude_api_key.is_some() || self.claude_auth.has_saved_credentials() {
            HarnessFamily::Claude
        } else {
            HarnessFamily::Codex
        }
    }

    /// Whether a harness family was chosen with --claude, --harness or a model.
    pub(crate) fn has_explicit_harness(&self) -> bool {
        self.claude
            || self.harness.is_some()
            || self.model.is_some()
            || std::env::var_os("OPENAI_MODEL").is_some()
            || std::env::var_os("ANTHROPIC_MODEL").is_some()
    }

    /// The workspace requested with `--cwd`, if any.
    pub(crate) fn requested_workspace(&self) -> Option<&std::path::Path> {
        self.cwd.as_deref()
    }

    /// Resume uses the store owning the thread unless the family was chosen explicitly.
    pub(crate) fn resume_with_harness(&mut self, family: HarnessFamily) {
        if !self.has_explicit_harness() {
            self.harness = Some(family.to_string());
        }
    }

    /// The local Claude harness has no VM support; keep a defaulted session on Codex.
    pub(crate) fn prefer_codex_for_vm(&mut self, vm: &VmArgs) {
        if vm.is_enabled() && !self.claude && self.harness.is_none() && self.model.is_none() {
            self.harness = Some(HarnessFamily::Codex.to_string());
        }
    }

    /// Resolve the model family before opening stores, acquiring credentials or starting tools.
    fn requested_model(&self, family: HarnessFamily) -> Result<Option<HarnessModel>> {
        let variable = match family {
            HarnessFamily::Codex => "OPENAI_MODEL",
            HarnessFamily::Claude => "ANTHROPIC_MODEL",
        };
        let environment = std::env::var(variable).ok();
        self.model
            .as_deref()
            .or(environment.as_deref())
            .map(|value| {
                let model: HarnessModel =
                    value.parse().map_err(|error: &'static str| eyre!(error))?;
                if model.family() != family {
                    return Err(eyre!(
                        "model {value:?} does not belong to the {family} harness"
                    ));
                }
                Ok(model)
            })
            .transpose()
    }

    pub(crate) fn restrict_to_host_control(&mut self, instructions: impl Into<String>) {
        self.browser.disable();
        self.mcp.disable();
        self.model_policy.web_search = Some(false);
        self.image_generation = Some(false);
        self.subagents = false;
        self.claude_workflows = false;
        self.claude_monitor_ws_origin.clear();
        self.rollouts = false;
        self.instructions = Some(instructions.into());
    }

    pub(crate) fn cwd(&self) -> &Path {
        self.cwd.as_deref().unwrap_or_else(|| Path::new("."))
    }

    #[cfg(test)]
    pub(crate) const fn uses_tempo(&self) -> bool {
        self.mpp.is_enabled()
    }

    #[cfg(test)]
    pub(crate) const fn browser_enabled(&self) -> bool {
        self.browser.is_enabled()
    }

    #[cfg(test)]
    pub(crate) const fn copies_all_browser_cookies(&self) -> bool {
        self.browser.copies_all_cookies()
    }

    #[cfg(test)]
    pub(crate) const fn uses_brave_browser(&self) -> bool {
        self.browser.uses_brave()
    }

    #[cfg(test)]
    pub(crate) const fn uses_interactive_browser_cookie_authorization(&self) -> bool {
        self.browser.uses_interactive_cookie_authorization()
    }

    #[cfg(test)]
    pub(crate) const fn uses_host_browser_passkeys(&self) -> bool {
        self.browser.uses_host_passkeys()
    }

    #[cfg(test)]
    pub(crate) const fn uses_persistent_browser_profile(&self) -> bool {
        self.browser.uses_persistent_profile()
    }

    pub(crate) fn thinking(&self) -> Thinking {
        self.selected_harness()
            .ok()
            .and_then(|family| self.model_policy.requested_thinking(family).ok().flatten())
            .unwrap_or_else(|| {
                self.harness_model()
                    .ok()
                    .filter(|model| model.family() != HarnessFamily::Codex)
                    .map_or(Thinking::Xhigh, HarnessModel::default_thinking)
            })
    }

    pub(crate) fn web_search(&self) -> bool {
        self.model_policy.web_search.unwrap_or(true)
    }

    pub(crate) fn fast_mode(&self) -> bool {
        self.fast_mode.unwrap_or(true)
            && self
                .selected_harness()
                .is_ok_and(|family| family == HarnessFamily::Codex)
    }

    pub(crate) fn responses_transport(&self) -> ResponsesTransport {
        self.responses_transport
            .unwrap_or(if self.mpp.is_enabled() {
                ResponsesTransport::Https
            } else {
                ResponsesTransport::WebSocket
            })
    }

    pub(crate) async fn build(
        self,
        vm: VmArgs,
        local_durability: Option<LocalDurability>,
    ) -> Result<ConfiguredAgent> {
        Box::pin(self.build_inner(None, vm, false, local_durability)).await
    }

    pub(crate) async fn build_tui(self, vm: VmArgs) -> Result<ConfiguredAgent> {
        Box::pin(self.build_inner(None, vm, true, None)).await
    }

    pub(crate) async fn build_resumed_tui(
        self,
        session: DurableSession,
        vm: VmArgs,
    ) -> Result<ConfiguredAgent> {
        Box::pin(self.build_inner(Some(session), vm, true, None)).await
    }

    async fn build_inner(
        mut self,
        durable: Option<DurableSession>,
        vm: VmArgs,
        tui: bool,
        local_durability: Option<LocalDurability>,
    ) -> Result<ConfiguredAgent> {
        self.prefer_codex_for_vm(&vm);
        let harness = self.selected_harness()?;
        if self.claude_workflows && (harness != HarnessFamily::Claude || !self.subagents) {
            return Err(eyre!(
                "--claude-workflows requires the Claude harness and enabled subagents"
            ));
        }
        if !self.claude_monitor_ws_origin.is_empty() && harness != HarnessFamily::Claude {
            return Err(eyre!(
                "--claude-monitor-ws-origin requires the Claude harness"
            ));
        }
        let requested_model = self.requested_model(harness)?;
        if harness == HarnessFamily::Claude {
            return self
                .build_claude(durable, vm, tui, local_durability, requested_model)
                .await;
        }
        let thinking = self
            .model_policy
            .requested_thinking(harness)?
            .unwrap_or(Thinking::Xhigh);
        let web_search = self.web_search();
        if local_durability.is_some() && self.rollouts {
            return Err(eyre!(
                "local durability testing requires `--rollouts false`; portable durability and Codex-compatible rollouts cannot both own restart state"
            ));
        }
        let codex_home = default_codex_home()?;
        let responses_transport = self.responses_transport();
        let mut session = prepare_session_build(self.cwd, self.rollouts, &codex_home, durable)?;
        if self.memory && session.session_id.is_none() {
            session.session_id = Some(SessionId::new());
        }
        let managed_memory = if self.memory {
            let _timing = crate::startup_timing::Stage::new("managed_memory");
            let root_session_id = session.session_id.ok_or_else(|| {
                eyre!("memory-enabled sessions require an explicit session identity")
            })?;
            Some(ConfiguredManagedMemory::connect(&codex_home, root_session_id).await?)
        } else {
            None
        };
        // Browser interaction is supplied by CUA, including for the direct CLI.
        let configured_browser = None;
        let mpp_enabled = self.mpp.is_enabled();
        if mpp_enabled && !matches!(responses_transport, ResponsesTransport::Https) {
            return Err(eyre!(
                "the Tempo provider currently supports HTTPS Responses with Charge only"
            ));
        }
        let auth = if mpp_enabled {
            OpenAiAuth::api_key("tempo-proxy")
        } else {
            self.auth.resolve()?.nanocodex()?
        };
        let model = match requested_model {
            Some(HarnessModel::Codex(model)) => model,
            Some(HarnessModel::Claude(_)) => {
                unreachable!("model family was validated")
            }
            None => connected_account_default_model(auth.mode()),
        };
        let direct_websocket_url = direct_websocket_url(self.websocket_url, auth.mode());
        let mpp_adapter = self.mpp.start().await?;
        let mut openai = OpenAi::builder(auth)
            .transport(responses_transport)
            .websocket_url(direct_websocket_url)
            .websocket_warmup(self.websocket_warmup);
        if let Some(prefix) = self.model_id_prefix.as_deref() {
            openai = openai.model_id_prefix(prefix);
        }
        if mpp_enabled {
            openai = openai.max_attempts(NonZeroU32::MIN);
        }
        if let Some(store) = self.store_responses {
            openai = openai.store(store);
        }
        let api_base_url = selected_api_base_url(
            self.api_base_url,
            mpp_adapter.as_ref().map(MppAdapter::api_base_url),
        );
        if let Some(api_base_url) = api_base_url {
            openai = openai.api_base_url(api_base_url);
        }
        if matches!(responses_transport, ResponsesTransport::Https)
            && let Some(mpp_adapter) = &mpp_adapter
        {
            openai = openai.http_client(mpp_adapter.responses_http_client()?);
        }
        let openai = openai.build()?;
        let realtime = (!mpp_enabled).then(|| openai.clone());
        let vm_egress = if vm.is_enabled() {
            mpp_adapter
                .as_ref()
                .map(MppAdapter::vm_egress_lease)
                .transpose()?
        } else {
            None
        };
        let configured_vm = vm.start(vm_egress).await?;
        let mut tools = match configured_vm.as_ref() {
            Some(vm) => vm.tools_builder().await?,
            None => Tools::builder().workspace(self.workspace_tools),
        }
        .web_search(web_search)
        .image_generation(self.image_generation.unwrap_or(true));
        let managed_mcp = if self.mcp.loads_managed() {
            let _timing = crate::startup_timing::Stage::new("managed_mcp_credentials");
            load_managed_mcp_credential(&codex_home).await?
        } else {
            None
        };
        let mcp = self
            .mcp
            .build(&codex_home, mpp_adapter.as_ref(), managed_mcp.as_ref())?;
        let mcp_handle = mcp.as_ref().map(|mcp| mcp.handle.clone());
        if let Some(ConfiguredMcp { provider, .. }) = mcp {
            tools = tools.provider(provider);
        }
        if let Some(mpp_adapter) = &mpp_adapter {
            if configured_vm.is_none() {
                tools = tools.process_environment(mpp_adapter.tool_environment());
            }
            tools = tools.remote_http_client(mpp_adapter.tool_http_client()?);
        }
        if configured_vm.is_none() && self.workspace_tools {
            let _timing = crate::startup_timing::Stage::new("computer_discovery");
            if let Some(computer) = crate::computer::connect_for_startup()
                .await
                .map_err(eyre::Report::msg)?
            {
                for tool in computer.tools() {
                    tools = tools.add(tool);
                }
            }
        }
        if let Some(managed_memory) = &managed_memory {
            tools = managed_memory.install(tools);
        }
        let tools = tools.build()?;
        let generic_subagents = self.subagents;
        let subagent_tools = selected_subagent_tools(generic_subagents, tui);
        let subagent_runtime = subagent_tools.map(|_| subagents::channel(self.max_subagents));
        let claude_tools = tools
            .clone()
            .into_builder()
            .workspace(false)
            .web_search(false)
            .image_generation(false)
            .build()?;
        let workspaces = Arc::new(claude::WorkspaceRegistry::new(
            session.workspace.clone(),
            codex_home.clone(),
        ));
        let root_workspace = session.workspace.clone();
        let codex_workspaces = Arc::clone(&workspaces);
        let codex_workspace = session.workspace.clone();
        let codex_registry = subagent_runtime
            .as_ref()
            .map(|(registry, _, _)| Arc::clone(registry));
        let codex_tools = tools.clone();
        let mut codex_recipe = Nanocodex::builder(openai.clone())
            .reasoning_mode(self.reasoning_mode)
            .fast_mode(self.fast_mode.unwrap_or(true))
            .workspace(session.workspace.clone())
            .codex_home(codex_home.clone());
        if let Some(instructions) = self.instructions.clone() {
            codex_recipe = codex_recipe.instructions(instructions);
        }
        if let Some(instructions) = session_instructions(
            self.instructions.as_deref(),
            generic_subagents,
            managed_memory.is_some(),
        ) {
            codex_recipe = codex_recipe.additional_instructions(instructions);
        }
        let harness_builder =
            nanocodex::Harness::builder().register(HarnessFamily::Codex, move |request| {
                let mut builder = codex_recipe.clone();
                let tools = codex_tools.clone();
                let registry = codex_registry.clone();
                let workspace = codex_workspace.clone();
                let workspaces = Arc::clone(&codex_workspaces);
                async move {
                    let HarnessModel::Codex(model) = request.model else {
                        return Err(nanocodex::NanocodexError::InvalidRequest(
                            "Codex recipe received a Claude model".into(),
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
                    builder = builder
                        .session_id(session_id)
                        .workspace(workspace)
                        .tools_factory(move |parent| {
                            workspaces
                                .seed(parent.session_id(), tool_workspace.clone())
                                .map_err(
                                    nanocodex::tools::runtime::ToolsBuildError::HostInitialization,
                                )?;
                            if let Some(registry) = &registry {
                                subagents::install_tools(
                                    tools.clone(),
                                    parent,
                                    Arc::clone(registry),
                                    subagent_tools.unwrap_or(SubagentToolSet::Generic),
                                )
                            } else {
                                Ok(tools.clone())
                            }
                        })
                        .model(model)
                        .thinking(request.thinking)
                        .host_context(request.host_context)
                        .spawn_factory(request.spawn_factory);
                    if let Some(snapshot) = request.snapshot {
                        builder = builder.restore_runtime(snapshot)?;
                    }
                    builder.build()
                }
            });
        let harness = claude::register_claude_recipe(
            harness_builder,
            claude::ClaudeConnection::new(
                self.claude_auth,
                self.claude_api_key,
                self.claude_messages_url,
            )
            .with_hooks(self.claude_hooks)
            .with_permission_config(
                self.claude_permissions.as_deref(),
                self.permission_mode.as_deref(),
            )?,
            session.workspace.clone(),
            self.instructions.clone(),
            claude_tools,
            web_search,
            subagent_runtime
                .as_ref()
                .map(|(registry, _, _)| Arc::clone(registry)),
            mcp_handle.clone(),
            Arc::clone(&workspaces),
        )
        .build();
        let mut builder = Nanocodex::builder(openai)
            .model(model)
            .reasoning_mode(self.reasoning_mode)
            .thinking(thinking)
            .fast_mode(self.fast_mode.unwrap_or(true))
            .spawn_factory(harness.spawn_factory())
            .workspace(session.workspace)
            .codex_home(codex_home);
        if let Some(session_id) = session.session_id {
            builder = builder.session_id(session_id);
        }
        if let Some(snapshot) = session.snapshot {
            builder = builder.resume(snapshot);
        }
        if let Some(rollout) = session.rollout {
            builder = builder.rollout(rollout);
        }
        let root_registry = subagent_runtime
            .as_ref()
            .map(|(registry, _, _)| Arc::clone(registry));
        let builder = builder.tools_factory(move |agent| {
            workspaces
                .seed(agent.session_id(), root_workspace.clone())
                .map_err(nanocodex::tools::runtime::ToolsBuildError::HostInitialization)?;
            if let (Some(registry), Some(subagent_tools)) = (&root_registry, subagent_tools) {
                subagents::install_tools(tools.clone(), agent, Arc::clone(registry), subagent_tools)
            } else {
                Ok(tools.clone())
            }
        });
        let additional_instructions = session_instructions(
            self.instructions.as_deref(),
            generic_subagents,
            managed_memory.is_some(),
        );
        let builder = if let Some(instructions) = self.instructions {
            builder.instructions(instructions)
        } else {
            builder
        };
        let builder = if let Some(instructions) = additional_instructions {
            builder.additional_instructions(instructions)
        } else {
            builder
        };
        let builder = if let Some(local_durability) = local_durability {
            let store = SqliteStore::open(&local_durability.path).wrap_err_with(|| {
                format!(
                    "failed to open local durability database {}",
                    local_durability.path.display()
                )
            })?;
            let state = PortableDurableSession::open(store, local_durability.state_id)
                .await
                .wrap_err("failed to open local durability state")?;
            builder
                .durability(state)
                .await
                .wrap_err("failed to attach local durability")?
        } else {
            builder
        };
        let (handle, events) = {
            let _timing = crate::startup_timing::Stage::new("native_agent");
            builder.build()?
        };
        let (child_agents, subagent_updates) =
            subagent_runtime.map_or((None, None), |(_, control, updates)| {
                let (drain_updates, subagent_updates) = if tui {
                    (None, Some(updates))
                } else {
                    (Some(updates), None)
                };
                (
                    Some(ChildAgents::new(
                        handle.session_id().to_string(),
                        control,
                        drain_updates,
                    )),
                    subagent_updates,
                )
            });
        Ok(ConfiguredAgent {
            claude_interactions: None,
            claude_scheduler: None,
            handle,
            events,
            realtime,
            child_agents,
            subagent_updates,
            mpp_adapter,
            mcp: mcp_handle,
            browser: configured_browser,
            vm: configured_vm,
            model: model.into(),
        })
    }
}

pub(crate) struct LocalDurability {
    pub(crate) path: PathBuf,
    pub(crate) state_id: String,
}

const fn selected_subagent_tools(
    generic_subagents: bool,
    simplify_workflow: bool,
) -> Option<SubagentToolSet> {
    match (generic_subagents, simplify_workflow) {
        (true, true) => Some(SubagentToolSet::GenericAndSimplify),
        (true, false) => Some(SubagentToolSet::Generic),
        (false, true) => Some(SubagentToolSet::Simplify),
        (false, false) => None,
    }
}

const SUBAGENT_INSTRUCTIONS: &str = concat!(
    "For larger tasks, delegate meaningful, separable work to subagents; handle trivial or tightly ",
    "coupled work directly. Use code mode to build multi-agent pipelines: map independent subtasks ",
    "across agents in parallel, await and reduce their results, then dispatch dependent stages. Do ",
    "not repeat delegated work yourself; wait for delegated work to finish, then use its results for ",
    "the next step. Double-check their results against the relevant evidence before relying on them. ",
    "Use schemas that expose the fields downstream stages need, and use loops to iterate until the ",
    "completion condition is met. Keep concurrent write scopes disjoint. You own final synthesis and ",
    "verification."
);

fn session_instructions(
    custom: Option<&str>,
    subagents_enabled: bool,
    memory_enabled: bool,
) -> Option<String> {
    let custom = custom.unwrap_or_default();
    let mut instructions = Vec::new();
    if subagents_enabled && !custom.contains(SUBAGENT_INSTRUCTIONS) {
        instructions.push(SUBAGENT_INSTRUCTIONS);
    }
    if memory_enabled && !custom.contains(MEMORY_INSTRUCTIONS) {
        instructions.push(MEMORY_INSTRUCTIONS);
    }
    (!instructions.is_empty()).then(|| instructions.join("\n\n"))
}

impl AuthArgs {
    fn resolve(self) -> Result<SharedAuth> {
        select_shared_auth(
            self.api_key,
            self.auth_file,
            self.access_token,
            environment_api_key()?,
        )
    }
}

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
impl EvalAgentArgs {
    pub(crate) fn shared_builder(
        self,
        model: Model,
        thinking: Thinking,
        web_search: bool,
    ) -> Result<(NanocodexBuilder, SharedAuth)> {
        self.model_policy.requested_thinking(HarnessFamily::Codex)?;
        let auth = self.auth.resolve()?;
        let builder = eval_builder_with_auth(auth.nanocodex()?, model, thinking, web_search)?;
        Ok((builder, auth))
    }

    pub(crate) fn thinking(&self) -> Option<Thinking> {
        self.model_policy
            .requested_thinking(HarnessFamily::Codex)
            .ok()
            .flatten()
    }

    pub(crate) const fn web_search(&self) -> Option<bool> {
        self.model_policy.web_search
    }
}

impl SharedAuth {
    fn nanocodex(&self) -> Result<OpenAiAuth> {
        match self {
            Self::ApiKey(api_key) => Ok(OpenAiAuth::api_key(Arc::clone(api_key))),
            Self::AccessToken(access_token) => {
                nanocodex::oai::auth::chatgpt_access_token(Arc::clone(access_token))
                    .map_err(Into::into)
            }
            Self::AuthFile(path) => load_subscription_auth(path),
        }
    }
}

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn eval_builder_with_auth(
    auth: OpenAiAuth,
    model: Model,
    thinking: Thinking,
    web_search: bool,
) -> Result<NanocodexBuilder> {
    let tools = Tools::builder().web_search(web_search).build()?;
    let openai = OpenAi::new(auth)?;
    Ok(Nanocodex::builder(openai)
        .model(model)
        .thinking(thinking)
        .tools(tools))
}

fn prepare_session_build(
    requested_workspace: Option<PathBuf>,
    rollouts: bool,
    codex_home: &Path,
    durable: Option<DurableSession>,
) -> Result<SessionBuild> {
    let Some(session) = durable else {
        return Ok(SessionBuild {
            workspace: requested_workspace.unwrap_or_else(|| PathBuf::from(".")),
            session_id: None,
            snapshot: None,
            rollout: rollouts.then(|| RolloutConfig::new(codex_home)),
        });
    };
    let restored = Path::new(session.workspace())
        .canonicalize()
        .wrap_err("failed to resolve the resumed workspace")?;
    if let Some(requested) = requested_workspace {
        let requested = requested
            .canonicalize()
            .wrap_err("failed to resolve the requested workspace")?;
        if requested != restored {
            return Err(eyre!(
                "resumed thread workspace is {}; --cwd requested {}",
                restored.display(),
                requested.display()
            ));
        }
    }
    let (session_id, snapshot, rollout) = session.into_parts();
    Ok(SessionBuild {
        workspace: restored,
        session_id: Some(
            session_id
                .parse()
                .wrap_err("resumed Codex thread ID is not UUIDv7")?,
        ),
        snapshot: Some(snapshot),
        rollout: rollouts.then_some(rollout),
    })
}

fn direct_websocket_url(explicit: Option<String>, auth_mode: OpenAiAuthMode) -> String {
    explicit.unwrap_or_else(|| auth_mode.default_websocket_url().to_owned())
}

const fn connected_account_default_model(auth_mode: OpenAiAuthMode) -> Model {
    match auth_mode {
        OpenAiAuthMode::ChatGpt => Model::Sol,
        OpenAiAuthMode::ApiKey => Model::Sol,
    }
}

fn selected_api_base_url(generic: Option<String>, tempo: Option<&str>) -> Option<String> {
    tempo.map(str::to_owned).or(generic)
}

#[cfg(test)]
fn select_auth(
    explicit_api_key: Option<String>,
    auth_file: Option<PathBuf>,
    access_token: Option<String>,
    environment_api_key: Option<String>,
) -> Result<OpenAiAuth> {
    select_shared_auth_with_default(
        explicit_api_key,
        auth_file,
        access_token,
        environment_api_key,
        default_auth_file,
    )
    .and_then(|auth| auth.nanocodex())
}

#[cfg(test)]
fn select_auth_with_default<F>(
    explicit_api_key: Option<String>,
    auth_file: Option<PathBuf>,
    access_token: Option<String>,
    environment_api_key: Option<String>,
    resolve_default_auth_file: F,
) -> Result<OpenAiAuth>
where
    F: FnOnce() -> Result<PathBuf>,
{
    select_shared_auth_with_default(
        explicit_api_key,
        auth_file,
        access_token,
        environment_api_key,
        resolve_default_auth_file,
    )
    .and_then(|auth| auth.nanocodex())
}

fn select_shared_auth(
    explicit_api_key: Option<String>,
    auth_file: Option<PathBuf>,
    access_token: Option<String>,
    environment_api_key: Option<String>,
) -> Result<SharedAuth> {
    select_shared_auth_with_default(
        explicit_api_key,
        auth_file,
        access_token,
        environment_api_key,
        default_auth_file,
    )
}

fn select_shared_auth_with_default<F>(
    explicit_api_key: Option<String>,
    auth_file: Option<PathBuf>,
    access_token: Option<String>,
    environment_api_key: Option<String>,
    resolve_default_auth_file: F,
) -> Result<SharedAuth>
where
    F: FnOnce() -> Result<PathBuf>,
{
    if let Some(api_key) = explicit_api_key {
        return Ok(SharedAuth::ApiKey(api_key.into()));
    }
    if let Some(auth_file) = auth_file {
        return Ok(SharedAuth::AuthFile(auth_file));
    }
    if let Some(access_token) = access_token {
        return Ok(SharedAuth::AccessToken(
            access_token.trim().to_owned().into(),
        ));
    }
    let auth_file = resolve_default_auth_file()?;
    if auth_file
        .try_exists()
        .wrap_err_with(|| format!("failed to inspect {}", auth_file.display()))?
    {
        return Ok(SharedAuth::AuthFile(auth_file));
    }
    if let Some(api_key) = environment_api_key {
        return Ok(SharedAuth::ApiKey(api_key.into()));
    }
    Ok(SharedAuth::AuthFile(auth_file))
}

fn environment_api_key() -> Result<Option<String>> {
    match std::env::var("OPENAI_API_KEY") {
        Ok(api_key) if api_key.trim().is_empty() => Ok(None),
        Ok(api_key) => Ok(Some(api_key)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error @ std::env::VarError::NotUnicode(_)) => {
            Err(error).wrap_err("OPENAI_API_KEY is not valid Unicode")
        }
    }
}

fn load_subscription_auth(auth_file: &Path) -> Result<OpenAiAuth> {
    nanocodex::oai::auth::load_chatgpt_auth(auth_file).map_err(|error| {
        eyre!(
            "ChatGPT authorization could not be loaded from {}: {error}. Run `nanocodex auth login`",
            auth_file.display()
        )
    })
}

pub(crate) fn default_auth_file() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("NANOCODEX_AUTH_FILE") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("CODEX_HOME").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path).join("auth.json"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| {
            eyre!("home directory is unavailable; pass --auth-file or NANOCODEX_AUTH_FILE")
        })?;
    Ok(PathBuf::from(home).join(".codex/auth.json"))
}

pub(crate) fn default_codex_home() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CODEX_HOME").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| {
            eyre!("home directory is unavailable; set CODEX_HOME or pass --rollouts false")
        })?;
    Ok(PathBuf::from(home).join(".codex"))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use nanocodex::oai::auth::OpenAiAuthMode;

    use super::{
        direct_websocket_url, select_auth, select_auth_with_default, selected_api_base_url,
    };

    #[test]
    fn default_websocket_url_follows_the_selected_auth_mode() {
        assert_eq!(
            direct_websocket_url(None, OpenAiAuthMode::ApiKey),
            "wss://api.openai.com/v1/responses"
        );
        assert_eq!(
            direct_websocket_url(None, OpenAiAuthMode::ChatGpt),
            "wss://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            direct_websocket_url(
                Some("ws://127.0.0.1:1234/responses".to_owned()),
                OpenAiAuthMode::ChatGpt,
            ),
            "ws://127.0.0.1:1234/responses"
        );
    }

    #[test]
    fn tempo_api_base_overrides_the_generic_openai_base() {
        assert_eq!(
            selected_api_base_url(
                Some("https://generic.example/v1".to_owned()),
                Some("https://tempo.example/v1"),
            ),
            Some("https://tempo.example/v1".to_owned())
        );
        assert_eq!(
            selected_api_base_url(Some("https://generic.example/v1".to_owned()), None),
            Some("https://generic.example/v1".to_owned())
        );
    }

    static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

    fn auth_file() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "nanocodex-cli-auth-selection-{}-{}.json",
            std::process::id(),
            NEXT_PATH.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn write_chatgpt_auth(path: &std::path::Path) {
        std::fs::write(
            path,
            br#"{
                "auth_mode": "chatgpt",
                "tokens": {
                    "id_token": "header.e30.signature",
                    "access_token": "access-token",
                    "refresh_token": "refresh-token",
                    "account_id": "account-1"
                }
            }"#,
        )
        .unwrap();
    }

    #[test]
    fn explicit_api_key_overrides_automatic_auth_selection() {
        let auth = select_auth(
            Some("explicit-key".into()),
            Some(auth_file()),
            Some("at-access-token".into()),
            Some("environment-key".into()),
        )
        .unwrap();

        assert_eq!(auth.mode(), OpenAiAuthMode::ApiKey);
    }

    #[test]
    fn default_chatgpt_auth_precedes_the_environment_key() {
        let auth_file = auth_file();
        write_chatgpt_auth(&auth_file);

        let auth =
            select_auth_with_default(None, None, None, Some("environment-key".into()), || {
                Ok(auth_file.clone())
            })
            .unwrap();

        assert_eq!(auth.mode(), OpenAiAuthMode::ChatGpt);
        std::fs::remove_file(auth_file).unwrap();
    }

    #[test]
    fn environment_key_is_used_when_the_default_auth_file_is_missing() {
        let auth_file = auth_file();
        let auth =
            select_auth_with_default(None, None, None, Some("environment-key".into()), || {
                Ok(auth_file)
            })
            .unwrap();

        assert_eq!(auth.mode(), OpenAiAuthMode::ApiKey);
    }

    #[test]
    fn invalid_default_auth_does_not_silently_fall_back_to_a_key() {
        let auth_file = auth_file();
        std::fs::write(&auth_file, b"{}").unwrap();

        let error =
            select_auth_with_default(None, None, None, Some("environment-key".into()), || {
                Ok(auth_file.clone())
            })
            .unwrap_err();

        assert!(error.to_string().contains("no ChatGPT tokens"));
        std::fs::remove_file(auth_file).unwrap();
    }

    #[test]
    fn explicit_auth_file_precedes_the_environment_key() {
        let auth_file = auth_file();
        std::fs::write(&auth_file, b"{}").unwrap();

        let error = select_auth(
            None,
            Some(auth_file.clone()),
            None,
            Some("environment-key".into()),
        )
        .unwrap_err();

        assert!(error.to_string().contains("no ChatGPT tokens"));
        std::fs::remove_file(auth_file).unwrap();
    }

    #[test]
    fn access_token_precedes_the_default_auth_file_and_environment_api_key() {
        let auth_file = auth_file();
        write_chatgpt_auth(&auth_file);

        let auth = select_auth_with_default(
            None,
            None,
            Some("at-persistent".into()),
            Some("environment-key".into()),
            || Ok(auth_file.clone()),
        )
        .unwrap();

        assert_eq!(auth.mode(), OpenAiAuthMode::ChatGpt);
        std::fs::remove_file(auth_file).unwrap();
    }
}
