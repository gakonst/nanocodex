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
    AgentEvents, DurableAgentExt as _, Model, Nanocodex, OpenAi, ReasoningMode, Thinking, Tools,
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

pub(crate) struct ConfiguredAgent {
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
    pub(crate) model: Model,
}

struct SessionBuild {
    workspace: PathBuf,
    session_id: Option<SessionId>,
    snapshot: Option<SessionSnapshot>,
    rollout: Option<RolloutConfig>,
}

/// Authentication flags shared by every direct-OpenAI CLI consumer.
#[derive(Args)]
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
#[derive(Args)]
pub(crate) struct ModelArgs {
    /// Reasoning effort: none, low, medium, high, xhigh, or max.
    #[arg(long, env = "OPENAI_REASONING_EFFORT")]
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

#[derive(Args)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent CLI feature toggles are not one state machine"
)]
pub(crate) struct AgentArgs {
    #[command(flatten)]
    auth: AuthArgs,

    /// Working directory exposed to the coding tools.
    #[arg(long)]
    cwd: Option<PathBuf>,

    #[command(flatten)]
    model_policy: ModelArgs,

    /// Coding model: gpt-5.6-sol, gpt-5.6-terra, gpt-5.6-luna, or gpt-6-astra.
    #[arg(long, env = "OPENAI_MODEL")]
    model: Option<Model>,

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
        default_value_t = false,
        action = ArgAction::Set
    )]
    fast_mode: bool,

    /// Replace the standard system/developer instructions.
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    instructions: Option<String>,

    /// Whether image generation is exposed to the model.
    #[arg(
        long,
        env = "NANOCODEX_IMAGE_GENERATION",
        default_value_t = true,
        action = ArgAction::Set
    )]
    image_generation: bool,

    /// Whether clean, reusable Tact-style subagents are exposed in Code Mode.
    #[arg(
        long,
        env = "NANOCODEX_SUBAGENTS",
        default_value_t = true,
        action = ArgAction::Set
    )]
    subagents: bool,

    /// Maximum number of active subagent turns across one task tree.
    #[arg(
        long,
        env = "NANOCODEX_MAX_SUBAGENTS",
        default_value_t = DEFAULT_MAX_SUBAGENTS
    )]
    max_subagents: usize,

    /// Write Codex-compatible resumable threads beneath `CODEX_HOME`.
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
    pub(crate) fn restrict_to_host_control(&mut self, instructions: impl Into<String>) {
        self.browser.disable();
        self.mcp.disable();
        self.model_policy.web_search = Some(false);
        self.image_generation = false;
        self.subagents = false;
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
        self.model_policy.thinking.unwrap_or_default()
    }

    pub(crate) fn web_search(&self) -> bool {
        self.model_policy.web_search.unwrap_or(true)
    }

    pub(crate) const fn fast_mode(&self) -> bool {
        self.fast_mode
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
        self.build_inner(None, vm, false, local_durability).await
    }

    pub(crate) async fn build_tui(self, vm: VmArgs) -> Result<ConfiguredAgent> {
        self.build_inner(None, vm, true, None).await
    }

    pub(crate) async fn build_resumed_tui(
        self,
        session: DurableSession,
        vm: VmArgs,
    ) -> Result<ConfiguredAgent> {
        self.build_inner(Some(session), vm, true, None).await
    }

    async fn build_inner(
        self,
        durable: Option<DurableSession>,
        vm: VmArgs,
        tui: bool,
        local_durability: Option<LocalDurability>,
    ) -> Result<ConfiguredAgent> {
        let thinking = self.thinking();
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
            let root_session_id = session.session_id.ok_or_else(|| {
                eyre!("memory-enabled sessions require an explicit session identity")
            })?;
            Some(ConfiguredManagedMemory::connect(&codex_home, root_session_id).await?)
        } else {
            None
        };
        let configured_browser = self.browser.configure(&session.workspace)?;
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
        let model = match self.model {
            Some(model) => model,
            None => connected_account_default_model(&auth).await,
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
        let mut tools = configured_vm
            .as_ref()
            .map_or_else(Tools::builder, ConfiguredVm::tools_builder)
            .web_search(web_search)
            .image_generation(self.image_generation);
        let managed_mcp = if self.mcp.loads_managed() {
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
        if let Some(browser) = &configured_browser {
            tools = tools.provider(browser.tool());
        }
        if let Some(managed_memory) = &managed_memory {
            tools = managed_memory.install(tools);
        }
        let tools = tools.build()?;
        let generic_subagents = self.subagents;
        let subagent_tools = selected_subagent_tools(generic_subagents, tui);
        let subagent_runtime = subagent_tools.map(|_| subagents::channel(self.max_subagents));
        let mut builder = Nanocodex::builder(openai)
            .model(model)
            .reasoning_mode(self.reasoning_mode)
            .thinking(thinking)
            .fast_mode(self.fast_mode)
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
        let builder = if let (Some((registry, _, _)), Some(subagent_tools)) =
            (&subagent_runtime, subagent_tools)
        {
            let tools = tools;
            let registry = Arc::clone(registry);
            builder.tools_factory(move |agent| {
                subagents::install_tools(
                    tools.clone(),
                    agent,
                    Arc::clone(&registry),
                    subagent_tools,
                )
            })
        } else {
            builder.tools(tools)
        };
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
        let (handle, events) = builder.build()?;
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
            handle,
            events,
            realtime,
            child_agents,
            subagent_updates,
            mpp_adapter,
            mcp: mcp_handle,
            browser: configured_browser,
            vm: configured_vm,
            model,
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
        let auth = self.auth.resolve()?;
        let builder = eval_builder_with_auth(auth.nanocodex()?, model, thinking, web_search)?;
        Ok((builder, auth))
    }

    pub(crate) const fn thinking(&self) -> Option<Thinking> {
        self.model_policy.thinking
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

async fn connected_account_default_model(auth: &OpenAiAuth) -> Model {
    const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;
    if auth.mode() != OpenAiAuthMode::ChatGpt {
        return Model::Sol;
    }
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(_) => return Model::Sol,
    };
    let mut snapshot = match auth.snapshot().await {
        Ok(snapshot) => snapshot,
        Err(_) => return Model::Sol,
    };
    for attempt in 0..2 {
        let mut request = client
            .get("https://chatgpt.com/backend-api/codex/models?client_version=0.5.0")
            .bearer_auth(snapshot.bearer())
            .header(
                "chatgpt-account-id",
                snapshot.account_id().unwrap_or_default(),
            )
            .header("originator", "codex_cli_rs")
            .header("user-agent", "nanocodex/0.5.0");
        if snapshot.is_fedramp() {
            request = request.header("x-openai-fedramp", "true");
        }
        let Ok(mut response) = request.send().await else {
            return Model::Sol;
        };
        if response.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
            if auth.recover_unauthorized(&snapshot).await.is_err() {
                return Model::Sol;
            }
            let Ok(refreshed) = auth.snapshot().await else {
                return Model::Sol;
            };
            snapshot = refreshed;
            continue;
        }
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|length| length > u64::try_from(MAX_CATALOG_BYTES).unwrap_or(u64::MAX))
        {
            return Model::Sol;
        }
        let mut encoded = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk))
                    if encoded.len().saturating_add(chunk.len()) <= MAX_CATALOG_BYTES =>
                {
                    encoded.extend_from_slice(&chunk);
                }
                Ok(Some(_)) | Err(_) => return Model::Sol,
                Ok(None) => break,
            }
        }
        return account_catalog_default_model(&encoded);
    }
    Model::Sol
}

#[derive(serde::Deserialize)]
struct AccountModelCatalog {
    models: Vec<AccountModelAvailability>,
}

#[derive(serde::Deserialize)]
struct AccountModelAvailability {
    slug: String,
    visibility: String,
}

fn account_catalog_default_model(encoded: &[u8]) -> Model {
    let Ok(catalog) = serde_json::from_slice::<AccountModelCatalog>(encoded) else {
        return Model::Sol;
    };
    if catalog
        .models
        .iter()
        .any(|model| model.slug == Model::Astra.as_str() && model.visibility == "list")
    {
        Model::Astra
    } else {
        Model::Sol
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

    use clap::{CommandFactory, Parser};
    use nanocodex::{Model, oai::auth::OpenAiAuthMode};

    #[test]
    fn account_catalog_defaults_to_only_picker_visible_astra() {
        assert_eq!(
            account_catalog_default_model(
                br#"{"models":[{"slug":"gpt-6-astra","visibility":"list"}]}"#,
            ),
            Model::Astra
        );
        assert_eq!(
            account_catalog_default_model(
                br#"{"models":[{"slug":"gpt-6-astra","visibility":"hide"}]}"#,
            ),
            Model::Sol
        );
        assert_eq!(account_catalog_default_model(b"not-json"), Model::Sol);
    }

    use super::{
        SUBAGENT_INSTRUCTIONS, account_catalog_default_model, direct_websocket_url, select_auth,
        select_auth_with_default, selected_api_base_url, selected_subagent_tools,
        session_instructions,
    };
    use crate::{managed_memory::MEMORY_INSTRUCTIONS, subagents::SubagentToolSet};

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
    fn subagents_are_enabled_by_default() {
        let command = crate::Cli::command();
        let subagents = command
            .get_arguments()
            .find(|argument| argument.get_id() == "subagents")
            .expect("the CLI should expose the subagents argument");

        assert_eq!(subagents.get_default_values(), ["true"]);
    }

    #[test]
    fn subagents_can_be_disabled_explicitly() {
        let cli = crate::Cli::try_parse_from(["nanocodex", "--subagents", "false"]).unwrap();

        assert!(!cli.agent.subagents);
    }

    #[test]
    fn subagent_concurrency_defaults_to_tacts_limit() {
        let command = crate::Cli::command();
        let max_subagents = command
            .get_arguments()
            .find(|argument| argument.get_id() == "max_subagents")
            .expect("the CLI should expose the max-subagents argument");

        assert_eq!(max_subagents.get_default_values(), ["32"]);
    }

    #[test]
    fn subagent_instructions_follow_the_enable_switch() {
        assert_eq!(session_instructions(None, false, false), None);
        assert_eq!(
            session_instructions(Some(SUBAGENT_INSTRUCTIONS), true, false),
            None
        );
        let enabled = session_instructions(None, true, false).unwrap();
        assert!(enabled.ends_with(SUBAGENT_INSTRUCTIONS));
        assert_eq!(enabled.matches(SUBAGENT_INSTRUCTIONS).count(), 1);
    }

    #[test]
    fn memory_instructions_follow_the_enable_switch() {
        assert_eq!(session_instructions(None, false, false), None);
        assert_eq!(
            session_instructions(Some(MEMORY_INSTRUCTIONS), false, true),
            None
        );
        let enabled = session_instructions(None, false, true).unwrap();
        assert!(enabled.ends_with(MEMORY_INSTRUCTIONS));
        assert_eq!(enabled.matches(MEMORY_INSTRUCTIONS).count(), 1);
    }

    #[test]
    fn simplify_reuses_the_runtime_without_exposing_generic_subagents() {
        assert_eq!(
            selected_subagent_tools(false, true),
            Some(SubagentToolSet::Simplify)
        );
        assert_eq!(selected_subagent_tools(false, false), None);
        assert_eq!(
            selected_subagent_tools(true, false),
            Some(SubagentToolSet::Generic)
        );
        assert_eq!(
            selected_subagent_tools(true, true),
            Some(SubagentToolSet::GenericAndSimplify)
        );
    }

    #[test]
    fn fast_mode_is_opt_in() {
        let command = crate::Cli::command();
        let fast_mode = command
            .get_arguments()
            .find(|argument| argument.get_id() == "fast_mode")
            .expect("the CLI should expose the fast-mode argument");

        assert_eq!(fast_mode.get_default_values(), ["false"]);
    }

    #[test]
    fn rollouts_are_enabled_by_default() {
        let command = crate::Cli::command();
        let rollouts = command
            .get_arguments()
            .find(|argument| argument.get_id() == "rollouts")
            .expect("the CLI should expose the rollouts argument");

        assert_eq!(rollouts.get_default_values(), ["true"]);
    }

    #[test]
    fn standard_mcp_servers_and_codex_config_are_enabled_by_default() {
        let command = crate::Cli::command();
        let mcp_defaults = command
            .get_arguments()
            .find(|argument| argument.get_id() == "mcp_defaults")
            .expect("the CLI should expose the MCP defaults argument");

        assert_eq!(mcp_defaults.get_default_values(), ["true"]);

        let codex_config = command
            .get_arguments()
            .find(|argument| argument.get_id() == "mcp_codex_config")
            .expect("the CLI should expose the Codex MCP config argument");
        assert_eq!(codex_config.get_default_values(), ["true"]);
    }

    #[test]
    fn responses_transport_and_storage_are_selected_once_at_startup() {
        let command = crate::Cli::command();
        let transport = command
            .get_arguments()
            .find(|argument| argument.get_id() == "responses_transport")
            .expect("the CLI should expose the Responses transport argument");
        assert!(transport.get_default_values().is_empty());

        assert!(
            command
                .get_arguments()
                .all(|argument| argument.get_id() != "responses_history"),
            "history replay policy is internal and must not be a CLI argument"
        );

        let store = command
            .get_arguments()
            .find(|argument| argument.get_id() == "store_responses")
            .expect("the CLI should expose the Responses storage argument");
        assert!(store.get_default_values().is_empty());

        let warmup = command
            .get_arguments()
            .find(|argument| argument.get_id() == "websocket_warmup")
            .expect("the CLI should expose the WebSocket warmup argument");
        assert_eq!(warmup.get_default_values(), ["false"]);
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
