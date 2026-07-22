use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use clap::{ArgAction, Args, builder::NonEmptyStringValueParser};
use eyre::{Result, WrapErr, eyre};
use nanocodex::{
    AgentEvents, Nanocodex, OpenAiAuth, ReasoningMode, Responses, RolloutConfig, Thinking, Tools,
};

use crate::mcp::McpArgs;
use crate::subagents::{self, ChildAgents};

pub(crate) struct ConfiguredAgent {
    pub(crate) handle: Nanocodex,
    pub(crate) events: AgentEvents,
    pub(crate) child_agents: Option<Arc<ChildAgents>>,
}

#[derive(Args)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent CLI feature toggles are not one state machine"
)]
pub(crate) struct AgentArgs {
    /// Explicit `OpenAI` API key override. Otherwise `OPENAI_API_KEY` is preferred.
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    api_key: Option<String>,

    /// Explicitly use `ChatGPT` authorization from this credential file.
    #[arg(long, env = "NANOCODEX_AUTH_FILE")]
    auth_file: Option<PathBuf>,

    /// Working directory exposed to the coding tools.
    #[arg(long, default_value = ".")]
    cwd: PathBuf,

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

    /// `OpenAI` HTTP API base used by standalone web search.
    #[arg(long, env = "OPENAI_API_BASE_URL")]
    api_base_url: Option<String>,

    #[command(flatten)]
    mcp: McpArgs,
}

impl AgentArgs {
    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn build(self) -> Result<ConfiguredAgent> {
        let rollout = self.rollouts.then(default_codex_home).transpose()?;
        let auth = select_auth(self.api_key, self.auth_file, environment_api_key()?)?;
        let mut responses = Responses::builder();
        if let Some(websocket_url) = self.websocket_url {
            responses = responses.websocket_url(websocket_url);
        }
        if let Some(api_base_url) = self.api_base_url {
            responses = responses.api_base_url(api_base_url);
        }
        let responses = responses.build();
        let mut tools = Tools::builder()
            .web_search(self.web_search)
            .image_generation(self.image_generation);
        if let Some(mcp) = self.mcp.build()? {
            tools = tools.provider(mcp);
        }
        let tools = tools.build()?;
        let child_agents = self.subagents.then(|| Arc::new(ChildAgents::default()));
        let builder = Nanocodex::builder(auth)
            .reasoning_mode(self.reasoning_mode)
            .thinking(self.thinking)
            .workspace(self.cwd)
            .responses(responses);
        let builder = if let Some(codex_home) = rollout {
            builder.rollout(RolloutConfig::new(codex_home))
        } else {
            builder
        };
        let builder = if let Some(child_agents) = &child_agents {
            let tools = tools.clone();
            let child_agents = Arc::downgrade(child_agents);
            builder.tools_factory(move |agent| {
                subagents::with_subagents(tools.clone(), agent, child_agents.clone())
            })
        } else {
            builder.tools(tools)
        };
        let builder = if let Some(instructions) = self.instructions {
            builder.instructions(instructions)
        } else {
            builder
        };
        let (handle, events) = builder.build()?;
        Ok(ConfiguredAgent {
            handle,
            events,
            child_agents,
        })
    }
}

fn select_auth(
    explicit_api_key: Option<String>,
    auth_file: Option<PathBuf>,
    environment_api_key: Option<String>,
) -> Result<OpenAiAuth> {
    if let Some(api_key) = explicit_api_key {
        return Ok(OpenAiAuth::api_key(api_key));
    }
    if let Some(auth_file) = auth_file {
        return load_subscription_auth(&auth_file);
    }
    if let Some(api_key) = environment_api_key {
        return Ok(OpenAiAuth::api_key(api_key));
    }
    load_subscription_auth(&default_auth_file()?)
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
    nanocodex::load_chatgpt_auth(auth_file).map_err(|error| {
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
    use nanocodex::OpenAiAuthMode;

    use super::select_auth;

    static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

    fn auth_file() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "nanocodex-cli-auth-selection-{}-{}.json",
            std::process::id(),
            NEXT_PATH.fetch_add(1, Ordering::Relaxed)
        ))
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
    fn environment_key_is_the_automatic_default() {
        let auth = select_auth(None, None, Some("environment-key".into())).unwrap();

        assert_eq!(auth.mode(), OpenAiAuthMode::ApiKey);
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
