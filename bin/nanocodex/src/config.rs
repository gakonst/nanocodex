use std::path::{Path, PathBuf};

use clap::{ArgAction, Args, builder::NonEmptyStringValueParser};
use eyre::Result;
use nanocodex::{AgentEvents, Nanocodex, Responses, Thinking, Tools};

use crate::mcp::McpArgs;

#[derive(Args)]
pub(crate) struct AgentArgs {
    /// `OpenAI` API key. Prefer `OPENAI_API_KEY` or the repository `.env` file.
    #[arg(
        long,
        env = "OPENAI_API_KEY",
        hide_env_values = true,
        value_parser = NonEmptyStringValueParser::new()
    )]
    api_key: String,

    /// Working directory exposed to the coding tools.
    #[arg(long, global = true, default_value = ".")]
    cwd: PathBuf,

    /// Reasoning effort used by the model.
    #[arg(long, global = true, env = "OPENAI_REASONING_EFFORT", default_value_t)]
    thinking: Thinking,

    /// Replace the standard system/developer prompt.
    #[arg(long, global = true, value_parser = NonEmptyStringValueParser::new())]
    system_prompt: Option<String>,

    /// Whether standalone web search is exposed to the model.
    #[arg(
        long,
        global = true,
        env = "NANOCODEX_WEB_SEARCH",
        default_value_t = true,
        action = ArgAction::Set
    )]
    web_search: bool,

    /// Whether image generation is exposed to the model.
    #[arg(
        long,
        global = true,
        env = "NANOCODEX_IMAGE_GENERATION",
        default_value_t = true,
        action = ArgAction::Set
    )]
    image_generation: bool,

    /// Responses API WebSocket endpoint.
    #[arg(
        long,
        global = true,
        env = "OPENAI_RESPONSES_WEBSOCKET_URL",
        default_value = "wss://api.openai.com/v1/responses"
    )]
    websocket_url: String,

    /// `OpenAI` HTTP API base used by standalone web search.
    #[arg(
        long,
        global = true,
        env = "OPENAI_API_BASE_URL",
        default_value = "https://api.openai.com/v1"
    )]
    api_base_url: String,

    #[command(flatten)]
    mcp: McpArgs,
}

impl AgentArgs {
    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn build(self) -> Result<(Nanocodex, AgentEvents)> {
        let responses = Responses::builder()
            .websocket_url(self.websocket_url)
            .api_base_url(self.api_base_url)
            .build();
        let mut tools = Tools::builder()
            .web_search(self.web_search)
            .image_generation(self.image_generation);
        if let Some(mcp) = self.mcp.build()? {
            tools = tools.provider(mcp);
        }
        let tools = tools.build()?;
        let builder = Nanocodex::builder(self.api_key)
            .thinking(self.thinking)
            .tools(tools)
            .workspace(self.cwd)
            .responses(responses);
        let builder = if let Some(prompt) = self.system_prompt {
            builder.prompt(prompt)
        } else {
            builder
        };
        Ok(builder.build()?)
    }
}
