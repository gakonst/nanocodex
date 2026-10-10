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
    agent::{rollout::RolloutConfig, session::SessionId},
    oai::{
        auth::{OpenAiAuth, OpenAiAuthMode},
        transport::ResponsesTransport,
    },
    tools::mcp::McpHandle,
};

use crate::browser::BrowserArgs;
use crate::login::load_managed_mcp_credential;
use crate::managed_memory::{ConfiguredManagedMemory, MEMORY_INSTRUCTIONS};
use crate::mcp::{ConfiguredMcp, McpArgs};
use crate::mpp::{MppAdapter, MppArgs};
use crate::sessions::{Persistence, ResumedSession};
use crate::subagents::{self, ChildAgents, DEFAULT_MAX_SUBAGENTS};
use crate::vm::{ConfiguredVm, VmArgs};

mod claude;
mod codex;
pub(crate) use claude::frontend as claude_frontend;
pub(crate) use claude::interaction::{
    InteractionReceiver, PendingInteraction, serve_terminal as serve_claude_terminal,
};
pub(crate) use claude::scheduler::SessionScheduler;
pub(crate) use claude::{prepare_rewind_branch, rewind_files};
mod instructions;
pub(crate) use instructions::expand_session_user_skill;

/// Host-serviced channels shared by every harness family: user questions,
/// plan-mode and permission prompts, and scheduled or monitored prompts.
#[derive(Default)]
pub(crate) struct HostChannels {
    pub(crate) interactions: Option<InteractionReceiver>,
    pub(crate) scheduler: Option<Arc<SessionScheduler>>,
}

pub(crate) struct ConfiguredAgent {
    pub(crate) host: HostChannels,
    pub(crate) handle: Nanocodex,
    pub(crate) events: AgentEvents,
    pub(crate) realtime: Option<OpenAi>,
    pub(crate) child_agents: Option<Arc<ChildAgents>>,
    pub(crate) subagent_updates:
        Option<tokio::sync::mpsc::UnboundedReceiver<nanocodex_subagents::ScopedAgentUpdate>>,
    pub(crate) mpp_adapter: Option<MppAdapter>,
    pub(crate) mcp: Option<McpHandle>,
    pub(crate) vm: Option<ConfiguredVm>,
    pub(crate) model: HarnessModel,
}

/// Identity, workspace and persistence of the root session, resolved once for
/// either family before credentials are acquired or tools start.
struct RootSession {
    workspace: PathBuf,
    session_id: String,
    resumed: Option<ResumedSession>,
    persistence: Option<Persistence>,
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
    /// Stored session to continue, of either family; set only by `nanocodex
    /// resume`, never from arbitrary CLI flags.
    #[arg(skip)]
    resume: Option<ResumedSession>,

    /// Voice microphone shortcut, or none to use /voice mute only.
    #[arg(long, env = "NANOCODEX_VOICE_MUTE_KEY", default_value = "ctrl+x", value_parser = crate::nanocodex2::tui::voice_keys::validate_key)]
    pub(crate) voice_mute_key: String,

    /// Animate live voice captions; set false for reduced motion.
    #[arg(long, env = "NANOCODEX_VOICE_ANIMATIONS", default_value_t = true, action = clap::ArgAction::Set)]
    pub(crate) voice_animations: bool,

    /// How the TUI shows tool calls: expanded, folded, or hidden.
    ///
    /// Ctrl+O cycles through the modes. Hidden keeps only the conversation;
    /// the footer still shows the turn as Working until it ends.
    #[arg(long, env = "NANOCODEX_TOOL_CALLS", value_enum, default_value_t)]
    pub(crate) tool_calls: crate::nanocodex2::tui::tool_calls::ToolCalls,

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

    /// Persist resumable sessions beneath `CODEX_HOME` for every harness: the
    /// durable session store plus a Codex-compatible JSONL rollout mirror.
    #[arg(
        long,
        env = "NANOCODEX_ROLLOUTS",
        default_value_t = true,
        action = ArgAction::Set
    )]
    rollouts: bool,

    /// Link Claude's natural instruction and skill paths to the canonical
    /// `CODEX_HOME` files before a session starts.
    #[arg(
        long,
        env = "NANOCODEX_LINK_HOMES",
        default_value_t = true,
        action = ArgAction::Set
    )]
    link_homes: bool,

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
        // A deliberate switch keeps retained settings the new model accepts
        // and resets the rest to its defaults, so the next request is valid.
        let capabilities = model.capabilities(nanocodex::ModelTransport::Native);
        self.harness = Some(model.family().to_string());
        self.claude = model.family() == HarnessFamily::Claude;
        self.model = Some(model.to_string());
        self.model_policy.thinking = Some(capabilities.normalize_thinking(thinking));
        self.fast_mode = Some(capabilities.fast_mode() && fast_mode);
        self.reasoning_mode = capabilities.normalize_reasoning_mode(self.reasoning_mode);
    }

    /// Validates the selected model's explicit settings before a terminal
    /// session starts, so an invalid launch exits with an actionable error.
    pub(crate) fn validate_model_settings(&self) -> Result<()> {
        self.check_model_settings(self.harness_model()?)
    }

    /// Rejects explicit thinking, fast-mode or reasoning-mode selections the
    /// model does not accept, before any credential or network use.
    pub(crate) fn check_model_settings(&self, model: HarnessModel) -> Result<()> {
        let capabilities = model.capabilities(nanocodex::ModelTransport::Native);
        if let Some(thinking) = self.model_policy.requested_thinking(model.family())? {
            capabilities.check_thinking(thinking).map_err(|error| eyre!(error))?;
        }
        if self.fast_mode == Some(true) {
            capabilities
                .check_fast_mode(true)
                .map_err(|error| eyre!("{error} (--fast-mode / NANOCODEX_FAST_MODE)"))?;
        }
        capabilities
            .check_reasoning_mode(self.reasoning_mode)
            .map_err(|error| eyre!("{error} (--reasoning-mode / OPENAI_REASONING_MODE)"))?;
        Ok(())
    }

    /// Arguments for switching a running TUI to another saved session (/attach):
    /// the resumed session supplies its own workspace and model.
    pub(crate) fn for_session_switch(mut self) -> Self {
        self.cwd = None;
        self.model = None;
        self.resume = None;
        self
    }

    /// The workspace requested with `--cwd`, if any.
    pub(crate) fn requested_workspace(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Arguments for a fresh session (/clear): keeps the selected harness,
    /// model and workspace but continues no stored session.
    pub(crate) fn fresh_session(mut self) -> Self {
        self.resume = None;
        self
    }

    /// Continues a stored session in the harness family that recorded it.
    pub(crate) fn resume(mut self, session: ResumedSession) -> Result<Self> {
        if !self.rollouts {
            return Err(eyre!(
                "resume requires session persistence; remove --rollouts false"
            ));
        }
        let family = session.family();
        let explicit = match (self.claude, self.harness.as_deref()) {
            (true, _) | (false, Some("claude")) => Some(HarnessFamily::Claude),
            (false, Some(_)) => Some(HarnessFamily::Codex),
            (false, None) => None,
        };
        if let Some(explicit) = explicit.filter(|explicit| *explicit != family) {
            return Err(eyre!(
                "session {} was recorded by the {family} harness; --harness {explicit} cannot resume it",
                session.id()
            ));
        }
        let saved = session.workspace().or(self.cwd.as_deref()).ok_or_else(|| {
            eyre!("session has no saved workspace; pass --cwd explicitly to resume it")
        })?;
        // Listing and reading history never need the workspace; continuing does.
        let workspace = saved.canonicalize().wrap_err_with(|| {
            format!(
                "failed to resolve the resumed workspace {}; restore that directory to resume session {}. Its history stays readable with `nanocodex rewind {}`",
                saved.display(),
                session.id(),
                session.id()
            )
        })?;
        if let Some(requested) = &self.cwd
            && requested
                .canonicalize()
                .wrap_err("failed to resolve --cwd")?
                != workspace
        {
            return Err(eyre!(
                "resumed session workspace is {}; --cwd requested {}",
                workspace.display(),
                requested.display()
            ));
        }
        self.harness = Some(family.to_string());
        self.claude = family == HarnessFamily::Claude;
        // An environment default must never silently switch a resumed model.
        // An explicit --model remains a deliberate, family-validated override.
        if self.model.is_none() {
            self.model = Some(session.model().to_string());
        }
        self.requested_model(family)?;
        self.cwd = Some(workspace);
        self.resume = Some(session);
        Ok(self)
    }

    /// The stored session this configuration continues, if any.
    pub(crate) const fn resumed(&self) -> Option<&ResumedSession> {
        self.resume.as_ref()
    }

    pub(crate) fn local_claude_available(&self) -> bool {
        self.claude_api_key.is_some()
            || self.claude_auth.has_saved_credentials()
            || self.selected_harness().ok() == Some(HarnessFamily::Claude)
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
        let responses_only = self.mpp.is_enabled()
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
        self.link_homes = false;
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

    pub(crate) const fn tui_reasoning_mode(&self) -> ReasoningMode {
        self.reasoning_mode
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

    /// Effective fast processing: an explicit choice where the model offers
    /// it; by default priority processing on Responses models, and standard
    /// speed on Claude, whose fast mode is a premium opt-in.
    pub(crate) fn fast_mode(&self) -> bool {
        self.harness_model().is_ok_and(|model| {
            model.supports_fast_mode()
                && self
                    .fast_mode
                    .unwrap_or(model.family() == HarnessFamily::Codex)
        })
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
        Box::pin(self.build_inner(vm, false, local_durability)).await
    }

    pub(crate) async fn build_tui(self, vm: VmArgs) -> Result<ConfiguredAgent> {
        Box::pin(self.build_inner(vm, true, None)).await
    }

    async fn build_inner(
        mut self,
        vm: VmArgs,
        tui: bool,
        local_durability: Option<LocalDurability>,
    ) -> Result<ConfiguredAgent> {
        self.prefer_codex_for_vm(&vm);
        let harness = self.selected_harness()?;
        if self.link_homes {
            crate::homes::link_at_startup();
        }
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
        self.check_model_settings(requested_model.unwrap_or_else(|| harness.default_model()))?;
        let codex_home = default_codex_home()?;
        let root = self.root_session(harness, &codex_home, local_durability)?;
        if harness == HarnessFamily::Claude {
            return self
                .build_claude(root, codex_home, vm, tui, requested_model)
                .await;
        }
        let thinking = self
            .model_policy
            .requested_thinking(harness)?
            .unwrap_or(Thinking::Xhigh);
        let responses_transport = self.responses_transport();
        let managed_memory = self.managed_memory(&codex_home, &root.session_id).await?;
        let mpp_enabled = self.mpp.is_enabled();
        if mpp_enabled && !matches!(responses_transport, ResponsesTransport::Https) {
            return Err(eyre!(
                "the Tempo provider currently supports HTTPS Responses with Charge only"
            ));
        }
        let auth = if mpp_enabled {
            OpenAiAuth::api_key("tempo-proxy")
        } else {
            self.auth.clone().resolve()?.nanocodex()?
        };
        let model = match requested_model {
            Some(HarnessModel::Codex(model)) => model,
            Some(HarnessModel::Claude(_)) => {
                unreachable!("model family was validated")
            }
            None => connected_account_default_model(auth.mode()),
        };
        let mpp_adapter = self.mpp.clone().start().await?;
        let openai = self
            .responses_settings()
            .client(auth, mpp_adapter.as_ref())?;
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
        let (tools, mcp_handle) = self
            .host_tools(
                &codex_home,
                configured_vm.as_ref(),
                mpp_adapter.as_ref(),
                managed_memory.as_ref(),
            )
            .await?;
        let subagent_runtime = self.subagents.then(|| subagents::channel(self.max_subagents));
        let registry = subagent_runtime
            .as_ref()
            .map(|(registry, _, _)| Arc::clone(registry));
        let workspaces = Arc::new(claude::WorkspaceRegistry::new(
            root.workspace.clone(),
            codex_home.clone(),
        ));
        let recipe = self.codex_recipe(
            HarnessFamily::Codex,
            codex::CodexConnection::ready(openai.clone()),
            tools.clone(),
            &codex_home,
            &root.workspace,
            &workspaces,
            registry.clone(),
            managed_memory.is_some(),
        );
        let harness = self
            .harness(
                recipe.clone(),
                self.claude_connection()?,
                &tools,
                &root.workspace,
                registry,
                mcp_handle.clone(),
                &workspaces,
            )?
            .build();
        let session_id = root
            .session_id
            .parse::<SessionId>()
            .wrap_err("Codex session IDs must be UUIDv7")?;
        let mut builder = recipe
            .builder(openai, session_id, root.workspace.clone())
            .model(model)
            // Explicit unsupported fast mode was rejected by check_model_settings.
            .fast_mode(recipe.fast_mode && HarnessModel::Codex(model).supports_fast_mode())
            .thinking(thinking)
            .spawn_factory(harness.spawn_factory());
        // A rollout-only thread has no durable state yet; its rollout boundary
        // seeds the new durable state once.
        let fallback = root.resumed.as_ref().and_then(ResumedSession::fallback);
        if let Some(persistence) = &root.persistence {
            if let Some(mirror) = persistence.mirror() {
                builder = builder.rollout(mirror);
            }
            let state = persistence.open(model.into(), &root.workspace).await?;
            if let Some(snapshot) = fallback
                && state
                    .latest_checkpoint()
                    .await
                    .wrap_err("failed to inspect the durable session")?
                    .is_none()
            {
                // The resolved root model and effort stay authoritative.
                builder = builder.resume(snapshot.clone())?.model(model).thinking(thinking);
            }
            builder = builder
                .durability(state)
                .await
                .wrap_err("failed to attach session durability")?;
        } else if let Some(snapshot) = fallback {
            builder = builder.resume(snapshot.clone())?.model(model).thinking(thinking);
        }
        let (handle, events) = {
            let _timing = crate::startup_timing::Stage::new("native_agent");
            builder.build()?
        };
        let (child_agents, subagent_updates) = child_agents(&handle, subagent_runtime, tui);
        Ok(ConfiguredAgent {
            host: HostChannels::default(),
            handle,
            events,
            realtime,
            child_agents,
            subagent_updates,
            mpp_adapter,
            mcp: mcp_handle,
            vm: configured_vm,
            model: model.into(),
        })
    }

    /// Resolves the root identity, workspace and persistence for either family.
    fn root_session(
        &mut self,
        family: HarnessFamily,
        codex_home: &Path,
        local_durability: Option<LocalDurability>,
    ) -> Result<RootSession> {
        let resumed = self.resume.take();
        let workspace = self
            .cwd
            .clone()
            .unwrap_or_else(|| PathBuf::from("."))
            .canonicalize()
            .wrap_err("failed to resolve the workspace")?;
        let mirror = self.rollouts.then(|| RolloutConfig::new(codex_home));
        let session_id = match (&resumed, &local_durability) {
            (Some(session), _) => session.id().to_owned(),
            // A test store keeps its explicit state ID. Claude sessions use it
            // as their identity; Codex identities must remain UUIDv7.
            (None, Some(local))
                if family == HarnessFamily::Claude
                    || local.state_id.parse::<SessionId>().is_ok() =>
            {
                local.state_id.clone()
            }
            (None, _) => SessionId::new().to_string(),
        };
        let persistence = match (local_durability, &resumed) {
            (Some(local), _) => Some(Persistence::new(local.path, local.state_id, mirror)),
            (None, Some(session)) => self
                .rollouts
                .then(|| Persistence::resumed(session, codex_home, true)),
            (None, None) => self
                .rollouts
                .then(|| Persistence::shared(codex_home, &session_id, mirror)),
        };
        Ok(RootSession {
            workspace,
            session_id,
            resumed,
            persistence,
        })
    }

    /// Managed memory is a host capability installed for either family.
    async fn managed_memory(
        &self,
        codex_home: &Path,
        root_session_id: &str,
    ) -> Result<Option<ConfiguredManagedMemory>> {
        if !self.memory {
            return Ok(None);
        }
        let _timing = crate::startup_timing::Stage::new("managed_memory");
        Ok(Some(
            ConfiguredManagedMemory::connect(codex_home, root_session_id).await?,
        ))
    }

    /// One host tool catalog for every family: MCP, Computer Use, managed
    /// memory, Tempo routing and the VM workspace, plus Codex's built-ins.
    async fn host_tools(
        &self,
        codex_home: &Path,
        configured_vm: Option<&ConfiguredVm>,
        mpp_adapter: Option<&MppAdapter>,
        managed_memory: Option<&ConfiguredManagedMemory>,
    ) -> Result<(Tools, Option<McpHandle>)> {
        let mut tools = match configured_vm {
            Some(vm) => vm.tools_builder().await?,
            None => Tools::builder().workspace(self.workspace_tools),
        }
        .exposure(nanocodex::tools::ToolExposure::CodeModeOnly)
        .web_search(self.web_search())
        .image_generation(self.image_generation.unwrap_or(true));
        let managed_mcp = if self.mcp.loads_managed() {
            let _timing = crate::startup_timing::Stage::new("managed_mcp_credentials");
            load_managed_mcp_credential(codex_home).await?
        } else {
            None
        };
        let mcp = self
            .mcp
            .clone()
            .build(codex_home, mpp_adapter, managed_mcp.as_ref())?;
        let mcp_handle = mcp.as_ref().map(|mcp| mcp.handle.clone());
        if let Some(ConfiguredMcp { provider, .. }) = mcp {
            tools = tools.provider(provider);
        }
        if let Some(mpp_adapter) = mpp_adapter {
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
        if let Some(managed_memory) = managed_memory {
            tools = managed_memory.install(tools);
        }
        Ok((tools.build()?, mcp_handle))
    }

    /// Responses client settings shared by Codex roots and children.
    fn responses_settings(&self) -> ResponsesSettings {
        ResponsesSettings {
            transport: self.responses_transport(),
            websocket_url: self.websocket_url.clone(),
            websocket_warmup: self.websocket_warmup,
            model_id_prefix: self.model_id_prefix.clone(),
            store_responses: self.store_responses,
            api_base_url: self.api_base_url.clone(),
        }
    }

    /// The single Codex recipe input set shared by either root family.
    ///
    /// A Codex root keeps its host guidance beside an explicit instruction, so
    /// its Codex children inherit that same effective instruction. A Claude
    /// root's explicit instruction is a complete replacement, which its Codex
    /// children receive exactly.
    #[allow(clippy::too_many_arguments)]
    fn codex_recipe(
        &self,
        root: HarnessFamily,
        connection: codex::CodexConnection,
        tools: Tools,
        codex_home: &Path,
        workspace: &Path,
        workspaces: &Arc<claude::WorkspaceRegistry>,
        registry: Option<Arc<nanocodex_subagents::Registry>>,
        memory: bool,
    ) -> codex::CodexRecipe {
        let exact = root == HarnessFamily::Claude && self.instructions.is_some();
        codex::CodexRecipe {
            connection,
            tools,
            instructions: self.instructions.clone(),
            additional_instructions: (!exact)
                .then(|| {
                    session_instructions(
                        self.instructions.as_deref(),
                        registry.is_some(),
                        memory,
                    )
                })
                .flatten(),
            reasoning_mode: self.reasoning_mode,
            fast_mode: self.fast_mode.unwrap_or(true),
            codex_home: codex_home.to_path_buf(),
            workspace: workspace.to_path_buf(),
            workspaces: Arc::clone(workspaces),
            registry,
        }
    }

    /// Registers every family's recipe over the same host inputs, so children of
    /// either family are built identically under either root.
    #[allow(clippy::too_many_arguments)]
    fn harness(
        &self,
        codex: codex::CodexRecipe,
        claude: claude::ClaudeConnection,
        tools: &Tools,
        workspace: &Path,
        registry: Option<Arc<nanocodex_subagents::Registry>>,
        mcp_handle: Option<McpHandle>,
        workspaces: &Arc<claude::WorkspaceRegistry>,
    ) -> Result<nanocodex::HarnessBuilder> {
        let harness = codex::register_codex_recipe(nanocodex::Harness::builder(), codex);
        Ok(claude::register_claude_recipe(
            harness,
            claude,
            workspace.to_path_buf(),
            self.instructions.clone(),
            claude::host_tools(tools)?,
            self.web_search(),
            registry,
            mcp_handle,
            Arc::clone(workspaces),
            self.fast_mode.unwrap_or(false),
        ))
    }

    fn claude_connection(&self) -> Result<claude::ClaudeConnection> {
        claude::ClaudeConnection::new(
            self.claude_auth.clone(),
            self.claude_api_key.clone(),
            self.claude_messages_url.clone(),
        )
        .with_hooks(self.claude_hooks.clone())
        .with_permission_config(
            self.claude_permissions.as_deref(),
            self.permission_mode.as_deref(),
        )
    }
}

/// Responses transport policy for every Codex session in one task tree.
#[derive(Clone)]
struct ResponsesSettings {
    transport: ResponsesTransport,
    websocket_url: Option<String>,
    websocket_warmup: bool,
    model_id_prefix: Option<String>,
    store_responses: Option<bool>,
    api_base_url: Option<String>,
}

impl ResponsesSettings {
    fn client(&self, auth: OpenAiAuth, mpp_adapter: Option<&MppAdapter>) -> Result<OpenAi> {
        let websocket_url = direct_websocket_url(self.websocket_url.clone(), auth.mode());
        let mut openai = OpenAi::builder(auth)
            .transport(self.transport)
            .websocket_url(websocket_url)
            .websocket_warmup(self.websocket_warmup);
        if let Some(prefix) = self.model_id_prefix.as_deref() {
            openai = openai.model_id_prefix(prefix);
        }
        if mpp_adapter.is_some() {
            openai = openai.max_attempts(NonZeroU32::MIN);
        }
        if let Some(store) = self.store_responses {
            openai = openai.store(store);
        }
        if let Some(api_base_url) = selected_api_base_url(
            self.api_base_url.clone(),
            mpp_adapter.map(MppAdapter::api_base_url),
        ) {
            openai = openai.api_base_url(api_base_url);
        }
        if matches!(self.transport, ResponsesTransport::Https)
            && let Some(mpp_adapter) = mpp_adapter
        {
            openai = openai.http_client(mpp_adapter.responses_http_client()?);
        }
        Ok(openai.build()?)
    }
}

type SubagentRuntime = (
    Arc<nanocodex_subagents::Registry>,
    nanocodex_subagents::SubagentControl,
    tokio::sync::mpsc::UnboundedReceiver<nanocodex_subagents::ScopedAgentUpdate>,
);

type SubagentHandles = (
    Option<Arc<ChildAgents>>,
    Option<tokio::sync::mpsc::UnboundedReceiver<nanocodex_subagents::ScopedAgentUpdate>>,
);

/// Subagent control for a built root; the TUI services updates itself.
fn child_agents(handle: &Nanocodex, runtime: Option<SubagentRuntime>, tui: bool) -> SubagentHandles {
    runtime.map_or((None, None), |(_, control, updates)| {
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
    })
}

pub(crate) struct LocalDurability {
    pub(crate) path: PathBuf,
    pub(crate) state_id: String,
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
