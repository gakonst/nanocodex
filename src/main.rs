use std::io;

use clap::{ArgAction, Parser, Subcommand};
use eyre::{Result, ensure, eyre};
use harness::{ModelConfig, ReasoningEffort};

#[derive(Parser)]
#[command(version, about = "A Harbor-first OpenAI coding harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read one task request from stdin and stream JSONL events to stdout.
    Run {
        /// `OpenAI` model used for the run.
        #[arg(long, env = "OPENAI_MODEL", default_value = "gpt-5.6-sol")]
        model: String,

        /// `OpenAI` API key. Prefer `OPENAI_API_KEY` or the repository `.env` file.
        #[arg(long, env = "OPENAI_API_KEY", hide_env_values = true)]
        api_key: Option<String>,

        /// Reasoning effort used by the model.
        #[arg(long, value_enum, env = "OPENAI_REASONING_EFFORT", default_value_t)]
        effort: ReasoningEffort,

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
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Keep direct `cargo run` behavior consistent with the Justfile without
    // requiring shell-specific syntax to load the repository's `.env` file.
    let _ = dotenvy::dotenv();

    match Cli::parse().command {
        Command::Run {
            model,
            api_key,
            effort,
            web_search,
            websocket_url,
            api_base_url,
        } => {
            ensure!(!model.trim().is_empty(), "model must not be empty");
            ensure!(
                !websocket_url.trim().is_empty(),
                "Responses WebSocket URL must not be empty"
            );
            ensure!(
                !api_base_url.trim().is_empty(),
                "OpenAI API base URL must not be empty"
            );
            let api_base_url = reqwest::Url::parse(&api_base_url)
                .map_err(|error| eyre!("invalid OpenAI API base URL: {error}"))?;
            ensure!(
                matches!(api_base_url.scheme(), "http" | "https"),
                "OpenAI API base URL must use http or https"
            );
            let api_key = api_key
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| eyre!("OPENAI_API_KEY or --api-key is required"))?;
            let config = ModelConfig {
                model,
                api_key,
                effort,
                web_search,
                websocket_url,
                api_base_url: api_base_url.to_string(),
            };
            harness::run(io::stdin().lock(), io::stdout().lock(), config).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command};
    use clap::Parser;

    #[test]
    fn web_search_accepts_an_explicit_boolean() {
        let cli = Cli::try_parse_from(["harness", "run", "--web-search", "false"])
            .expect("explicit false should parse");
        let Command::Run { web_search, .. } = cli.command;

        assert!(!web_search);
    }
}
