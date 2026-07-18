use std::io;

use clap::{ArgAction, Args, builder::NonEmptyStringValueParser};
use eyre::Result;
use harness_agent::{Agent, Responses, Thinking, Tools};

#[derive(Args)]
pub(crate) struct Run {
    /// Prompt submitted to the agent.
    #[arg(value_parser = NonEmptyStringValueParser::new())]
    prompt: String,

    /// `OpenAI` API key. Prefer `OPENAI_API_KEY` or the repository `.env` file.
    #[arg(
        long,
        env = "OPENAI_API_KEY",
        hide_env_values = true,
        value_parser = NonEmptyStringValueParser::new()
    )]
    api_key: String,

    /// Reasoning effort used by the model.
    #[arg(long, env = "OPENAI_REASONING_EFFORT", default_value_t)]
    thinking: Thinking,

    /// Whether standalone web search is exposed to the model.
    #[arg(
        long,
        env = "HARNESS_WEB_SEARCH",
        default_value_t = true,
        action = ArgAction::Set
    )]
    web_search: bool,

    /// Responses API WebSocket endpoint.
    #[arg(
        long,
        env = "OPENAI_RESPONSES_WEBSOCKET_URL",
        default_value = "wss://api.openai.com/v1/responses"
    )]
    websocket_url: String,

    /// `OpenAI` HTTP API base used by standalone web search.
    #[arg(
        long,
        env = "OPENAI_API_BASE_URL",
        default_value = "https://api.openai.com/v1"
    )]
    api_base_url: String,
}

impl Run {
    pub(crate) async fn run(self) -> Result<()> {
        let responses = Responses::builder()
            .websocket_url(self.websocket_url)
            .api_base_url(self.api_base_url)
            .build();
        let tools = Tools::builder().web_search(self.web_search).build()?;
        let (handle, mut events) = Agent::builder(self.api_key)
            .thinking(self.thinking)
            .tools(tools)
            .responses(responses)
            .build()?;
        let turn = handle.prompt(self.prompt).await?;
        events.write_turn_jsonl(io::stdout()).await?;
        turn.result().await?;
        Ok(())
    }
}
