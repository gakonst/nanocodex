use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use clap::{ArgAction, Args, builder::NonEmptyStringValueParser};
use eyre::{Result, eyre};
use nanocodex::{AgentEvents, Nanocodex, OpenAiAuth, Responses, Thinking, Tools};

use crate::mcp::McpArgs;
use crate::subagents::{self, ChildAgents};

pub(crate) struct ConfiguredAgent {
    pub(crate) handle: Nanocodex,
    pub(crate) events: AgentEvents,
    pub(crate) child_agents: Option<Arc<ChildAgents>>,
    pub(crate) thinking: Thinking,
}

#[derive(Args)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent CLI feature toggles are not one state machine"
)]
pub(crate) struct AgentArgs {
    /// `OpenAI` API key. Prefer `OPENAI_API_KEY` or the repository `.env` file.
    #[arg(
        long,
        env = "OPENAI_API_KEY",
        hide_env_values = true,
        value_parser = NonEmptyStringValueParser::new()
    )]
    api_key: Option<String>,

    /// Force the stored `ChatGPT` OAuth session even when an API key is available.
    #[arg(long, env = "NANOCODEX_CHATGPT", default_value_t = false)]
    chatgpt: bool,

    /// Override the shared Codex `auth.json` credential file.
    #[arg(long, env = "NANOCODEX_AUTH_FILE")]
    auth_file: Option<PathBuf>,

    /// Working directory exposed to the coding tools.
    #[arg(long, default_value = ".")]
    cwd: PathBuf,

    /// Reasoning effort used by the model.
    #[arg(long, env = "OPENAI_REASONING_EFFORT", default_value_t)]
    thinking: Thinking,

    /// Responses service tier. `fast` maps to the API's `priority` tier.
    #[arg(
        long,
        env = "OPENAI_SERVICE_TIER",
        value_parser = NonEmptyStringValueParser::new()
    )]
    service_tier: Option<String>,

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
        let thinking = self.thinking;
        let auth = if self.chatgpt {
            load_subscription_auth(self.auth_file)?
        } else if let Some(api_key) = self.api_key {
            OpenAiAuth::api_key(api_key)
        } else {
            load_subscription_auth(self.auth_file)?
        };
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
            .thinking(thinking)
            .workspace(self.cwd)
            .responses(responses);
        let builder = if let Some(service_tier) = self.service_tier {
            builder.service_tier(service_tier)
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
            thinking,
        })
    }
}

fn load_subscription_auth(auth_file: Option<PathBuf>) -> Result<OpenAiAuth> {
    let auth_file = auth_file.unwrap_or(default_auth_file()?);
    nanocodex::load_chatgpt_auth(&auth_file).map_err(|error| {
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::AgentArgs;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        agent: AgentArgs,
    }

    #[test]
    fn parses_service_tier_flag() {
        let cli = TestCli::parse_from(["nanocodex", "--service-tier", "fast"]);

        assert_eq!(cli.agent.service_tier.as_deref(), Some("fast"));
    }
}
