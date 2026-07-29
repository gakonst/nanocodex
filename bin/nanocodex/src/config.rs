use std::{
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::Arc,
};

use clap::{ArgAction, Args, ValueEnum, builder::NonEmptyStringValueParser};
use eyre::{Result, WrapErr, eyre};
use nanocodex::{
    AgentEvents, Anthropic, Nanocodex, OpenAi, ReasoningMode, Thinking, Tools,
    agent::{
        rollout::{DurableSession, RolloutConfig},
        session::{SessionId, SessionSnapshot},
    },
    oai::{
        __private::ResponsesServiceFactory,
        ResponseError,
        anthropic::{ANTHROPIC_MODEL, load_anthropic_auth},
        auth::{OpenAiAuth, OpenAiAuthMode},
        tower::{ResponsesAttempt, ResponsesServiceResponse},
        transport::ResponsesTransport,
    },
    tools::mcp::McpHandle,
};
use tower::Service;

use crate::mcp::{ConfiguredMcp, McpArgs};
use crate::mpp::{MppAdapter, MppArgs};
use crate::subagents::{self, ChildAgents};
use crate::vm::{ConfiguredVm, VmArgs};

pub(crate) struct ConfiguredAgent {
    pub(crate) handle: Nanocodex,
    pub(crate) events: AgentEvents,
    pub(crate) model: Arc<str>,
    pub(crate) child_agents: Option<Arc<ChildAgents>>,
    pub(crate) mpp_adapter: Option<MppAdapter>,
    pub(crate) mcp: Option<McpHandle>,
    pub(crate) vm: Option<ConfiguredVm>,
}

struct SessionBuild {
    workspace: PathBuf,
    session_id: Option<SessionId>,
    snapshot: Option<SessionSnapshot>,
    rollout: Option<RolloutConfig>,
}

#[derive(Clone, Copy, Default, Eq, PartialEq, ValueEnum)]
enum Provider {
    #[default]
    #[value(name = "openai")]
    OpenAi,
    Anthropic,
}

#[derive(Args)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent CLI feature toggles are not one state machine"
)]
pub(crate) struct AgentArgs {
    /// Explicit `OpenAI` API key override.
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    api_key: Option<String>,

    /// Explicitly use `ChatGPT` authorization from this credential file.
    #[arg(long, env = "NANOCODEX_AUTH_FILE")]
    auth_file: Option<PathBuf>,

    /// Working directory exposed to the coding tools.
    #[arg(long)]
    cwd: Option<PathBuf>,

    /// Reasoning effort: none, low, medium, high, xhigh, or max.
    #[arg(long, env = "OPENAI_REASONING_EFFORT", default_value_t)]
    thinking: Thinking,

    /// Reasoning execution mode: standard or pro.
    #[arg(long, env = "OPENAI_REASONING_MODE", default_value_t)]
    reasoning_mode: ReasoningMode,

    /// Replace the standard system/developer instructions.
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    instructions: Option<String>,

    /// Whether standalone web search is exposed to the model.
    #[arg(
        long,
        env = "NANOCODEX_WEB_SEARCH",
        default_value_t = true,
        action = ArgAction::Set
    )]
    web_search: bool,

    /// Whether image generation is exposed to the model.
    #[arg(
        long,
        env = "NANOCODEX_IMAGE_GENERATION",
        default_value_t = true,
        action = ArgAction::Set
    )]
    image_generation: bool,

    /// Expose reusable clean, forked, and follow-up child agents in Code Mode.
    #[arg(
        long,
        env = "NANOCODEX_SUBAGENTS",
        default_value_t = false,
        action = ArgAction::Set
    )]
    subagents: bool,

    /// Write Codex-compatible resumable threads beneath `CODEX_HOME`.
    #[arg(
        long,
        env = "NANOCODEX_ROLLOUTS",
        default_value_t = true,
        action = ArgAction::Set
    )]
    rollouts: bool,

    /// Responses API WebSocket endpoint.
    #[arg(long, env = "OPENAI_RESPONSES_WEBSOCKET_URL")]
    websocket_url: Option<String>,

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

    /// Model service used for agent inference.
    #[arg(
        long,
        env = "NANOCODEX_PROVIDER",
        default_value = "openai",
        conflicts_with = "provider_openai"
    )]
    provider: Provider,

    /// Anthropic Messages model override.
    #[arg(long, env = "ANTHROPIC_MODEL")]
    anthropic_model: Option<String>,

    /// Anthropic Messages API base override.
    #[arg(long, env = "ANTHROPIC_API_BASE_URL")]
    anthropic_api_base_url: Option<String>,

    #[command(flatten)]
    mcp: McpArgs,

    #[command(flatten)]
    mpp: MppArgs,
}

impl AgentArgs {
    pub(crate) fn cwd(&self) -> &Path {
        self.cwd.as_deref().unwrap_or_else(|| Path::new("."))
    }

    #[cfg(test)]
    pub(crate) const fn uses_tempo(&self) -> bool {
        self.mpp.is_enabled()
    }

    #[cfg(test)]
    pub(crate) const fn uses_anthropic(&self) -> bool {
        matches!(self.provider, Provider::Anthropic)
    }

    pub(crate) const fn thinking(&self) -> Thinking {
        self.thinking
    }

    pub(crate) fn responses_transport(&self) -> ResponsesTransport {
        self.responses_transport
            .unwrap_or(if self.mpp.is_enabled() {
                ResponsesTransport::Https
            } else {
                ResponsesTransport::WebSocket
            })
    }

    pub(crate) async fn build(self, vm: VmArgs) -> Result<ConfiguredAgent> {
        self.build_inner(None, vm).await
    }

    pub(crate) async fn build_resumed(
        self,
        session: DurableSession,
        vm: VmArgs,
    ) -> Result<ConfiguredAgent> {
        self.build_inner(Some(session), vm).await
    }

    async fn build_inner(
        self,
        durable: Option<DurableSession>,
        vm: VmArgs,
    ) -> Result<ConfiguredAgent> {
        let codex_home = default_codex_home()?;
        let responses_transport = self.responses_transport();
        let session = prepare_session_build(self.cwd, self.rollouts, &codex_home, durable)?;
        let mpp_enabled = self.mpp.is_enabled();
        if mpp_enabled && matches!(self.provider, Provider::Anthropic) {
            return Err(eyre!(
                "the Tempo provider cannot be combined with --provider anthropic"
            ));
        }
        if mpp_enabled && !matches!(responses_transport, ResponsesTransport::Https) {
            return Err(eyre!(
                "the Tempo provider currently supports HTTPS Responses with Charge only"
            ));
        }
        let auth = if matches!(self.provider, Provider::OpenAi) {
            Some(if mpp_enabled {
                OpenAiAuth::api_key("tempo-proxy")
            } else {
                select_auth(self.api_key, self.auth_file, environment_api_key()?)?
            })
        } else {
            None
        };
        let anthropic_auth = if matches!(self.provider, Provider::Anthropic) {
            Some(
                load_anthropic_auth()
                    .await
                    .wrap_err("failed to resolve Anthropic credentials")?,
            )
        } else {
            None
        };
        let mpp_adapter = self.mpp.start().await?;
        let configured_vm = vm.start().await?;
        let mut tools = configured_vm
            .as_ref()
            .map_or_else(Tools::builder, ConfiguredVm::tools_builder)
            .web_search(self.web_search)
            .image_generation(self.image_generation);
        let mcp = self.mcp.build(&codex_home)?;
        let mcp_handle = mcp.as_ref().map(|mcp| mcp.handle.clone());
        if let Some(ConfiguredMcp { provider, .. }) = mcp {
            tools = tools.provider(provider);
        }
        if let Some(mpp_adapter) = &mpp_adapter {
            tools = tools
                .process_environment(mpp_adapter.tool_environment())
                .remote_http_client(mpp_adapter.tool_http_client()?);
        }
        let tools = tools.build()?;
        let child_agents = self.subagents.then(|| Arc::new(ChildAgents::default()));
        let common = CommonAgentBuild {
            session,
            codex_home,
            tools,
            child_agents,
            mpp_adapter,
            mcp: mcp_handle,
            vm: configured_vm,
            reasoning_mode: self.reasoning_mode,
            thinking: self.thinking,
            instructions: self.instructions,
        };
        if let Some(anthropic_auth) = anthropic_auth {
            let model: Arc<str> = self
                .anthropic_model
                .unwrap_or_else(|| ANTHROPIC_MODEL.to_owned())
                .into();
            let mut anthropic = Anthropic::builder(anthropic_auth).model(Arc::clone(&model));
            if let Some(api_base_url) = self.anthropic_api_base_url {
                anthropic = anthropic.api_base_url(api_base_url);
            }
            configure_agent(anthropic.build()?, model, common)
        } else {
            let auth = auth.ok_or_else(|| eyre!("OpenAI authorization was not resolved"))?;
            let direct_websocket_url = direct_websocket_url(self.websocket_url, auth.mode());
            let mut openai = OpenAi::builder(auth)
                .transport(responses_transport)
                .websocket_url(direct_websocket_url);
            if mpp_enabled {
                openai = openai.max_attempts(NonZeroU32::MIN);
            }
            if let Some(store) = self.store_responses {
                openai = openai.store(store);
            }
            let api_base_url = selected_api_base_url(
                self.api_base_url,
                common.mpp_adapter.as_ref().map(MppAdapter::api_base_url),
            );
            if let Some(api_base_url) = api_base_url {
                openai = openai.api_base_url(api_base_url);
            }
            if matches!(responses_transport, ResponsesTransport::Https)
                && let Some(mpp_adapter) = &common.mpp_adapter
            {
                openai = openai.http_client(mpp_adapter.responses_http_client()?);
            }
            configure_agent(openai.build()?, nanocodex::oai::MODEL.into(), common)
        }
    }
}

struct CommonAgentBuild {
    session: SessionBuild,
    codex_home: PathBuf,
    tools: Tools,
    child_agents: Option<Arc<ChildAgents>>,
    mpp_adapter: Option<MppAdapter>,
    mcp: Option<McpHandle>,
    vm: Option<ConfiguredVm>,
    reasoning_mode: ReasoningMode,
    thinking: Thinking,
    instructions: Option<String>,
}

fn configure_agent<F>(
    client: OpenAi<F>,
    model: Arc<str>,
    common: CommonAgentBuild,
) -> Result<ConfiguredAgent>
where
    F: ResponsesServiceFactory + Send + Sync + 'static,
    F::Service: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    <F::Service as Service<ResponsesAttempt>>::Error: Into<ResponseError> + Send + 'static,
    <F::Service as Service<ResponsesAttempt>>::Future: Send,
{
    let CommonAgentBuild {
        session,
        codex_home,
        tools,
        child_agents,
        mpp_adapter,
        mcp,
        vm,
        reasoning_mode,
        thinking,
        instructions,
    } = common;
    let mut builder = Nanocodex::builder(client)
        .reasoning_mode(reasoning_mode)
        .thinking(thinking)
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
    let builder = if let Some(child_agents) = &child_agents {
        let tools = tools;
        let child_agents = Arc::downgrade(child_agents);
        builder.tools_factory(move |agent| {
            subagents::with_subagents(tools.clone(), agent, child_agents.clone())
        })
    } else {
        builder.tools(tools)
    };
    let builder = if let Some(instructions) = instructions {
        builder.instructions(instructions)
    } else {
        builder
    };
    let (handle, events) = builder.build()?;
    Ok(ConfiguredAgent {
        handle,
        events,
        model,
        child_agents,
        mpp_adapter,
        mcp,
        vm,
    })
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

fn selected_api_base_url(generic: Option<String>, tempo: Option<&str>) -> Option<String> {
    tempo.map(str::to_owned).or(generic)
}

fn select_auth(
    explicit_api_key: Option<String>,
    auth_file: Option<PathBuf>,
    environment_api_key: Option<String>,
) -> Result<OpenAiAuth> {
    select_auth_with_default(
        explicit_api_key,
        auth_file,
        environment_api_key,
        default_auth_file,
    )
}

fn select_auth_with_default<F>(
    explicit_api_key: Option<String>,
    auth_file: Option<PathBuf>,
    environment_api_key: Option<String>,
    resolve_default_auth_file: F,
) -> Result<OpenAiAuth>
where
    F: FnOnce() -> Result<PathBuf>,
{
    if let Some(api_key) = explicit_api_key {
        return Ok(OpenAiAuth::api_key(api_key));
    }
    if let Some(auth_file) = auth_file {
        return load_subscription_auth(&auth_file);
    }
    let auth_file = resolve_default_auth_file()?;
    if auth_file
        .try_exists()
        .wrap_err_with(|| format!("failed to inspect {}", auth_file.display()))?
    {
        return load_subscription_auth(&auth_file);
    }
    if let Some(api_key) = environment_api_key {
        return Ok(OpenAiAuth::api_key(api_key));
    }
    load_subscription_auth(&auth_file)
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

    use clap::CommandFactory;
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
    fn subagents_are_opt_in() {
        let command = crate::Cli::command();
        let subagents = command
            .get_arguments()
            .find(|argument| argument.get_id() == "subagents")
            .expect("the CLI should expose the subagents argument");

        assert_eq!(subagents.get_default_values(), ["false"]);
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
    fn standard_and_codex_config_mcp_servers_are_enabled_by_default() {
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
    }

    #[test]
    fn explicit_api_key_overrides_automatic_auth_selection() {
        let auth = select_auth(
            Some("explicit-key".into()),
            Some(auth_file()),
            Some("environment-key".into()),
        )
        .unwrap();

        assert_eq!(auth.mode(), OpenAiAuthMode::ApiKey);
    }

    #[test]
    fn default_chatgpt_auth_precedes_the_environment_key() {
        let auth_file = auth_file();
        write_chatgpt_auth(&auth_file);

        let auth = select_auth_with_default(None, None, Some("environment-key".into()), || {
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
            select_auth_with_default(None, None, Some("environment-key".into()), || Ok(auth_file))
                .unwrap();

        assert_eq!(auth.mode(), OpenAiAuthMode::ApiKey);
    }

    #[test]
    fn invalid_default_auth_does_not_silently_fall_back_to_a_key() {
        let auth_file = auth_file();
        std::fs::write(&auth_file, b"{}").unwrap();

        let error = select_auth_with_default(None, None, Some("environment-key".into()), || {
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
            Some("environment-key".into()),
        )
        .unwrap_err();

        assert!(error.to_string().contains("no ChatGPT tokens"));
        std::fs::remove_file(auth_file).unwrap();
    }
}
