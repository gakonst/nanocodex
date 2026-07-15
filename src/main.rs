use std::io;

use clap::{Parser, Subcommand};
use eyre::Result;
use harness::{Mode, ModelConfig, ReasoningEffort, RunConfig};

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
        #[arg(long, value_enum, default_value_t)]
        mode: Mode,

        /// `OpenAI` model used by model mode.
        #[arg(long, env = "OPENAI_MODEL", default_value = "gpt-5.6-sol")]
        model: String,

        /// `OpenAI` API key. Prefer `OPENAI_API_KEY` or the repository `.env` file.
        #[arg(long, env = "OPENAI_API_KEY", hide_env_values = true)]
        api_key: Option<String>,

        /// Reasoning effort used by model mode.
        #[arg(long, value_enum, env = "OPENAI_REASONING_EFFORT", default_value_t)]
        effort: ReasoningEffort,

        /// Responses API WebSocket endpoint.
        #[arg(
            long,
            env = "OPENAI_RESPONSES_WEBSOCKET_URL",
            default_value = "wss://api.openai.com/v1/responses"
        )]
        websocket_url: String,

        /// Maximum number of sequential model calls in one task.
        #[arg(long, default_value_t = 32)]
        max_model_calls: u32,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Run {
            mode,
            model,
            api_key,
            effort,
            websocket_url,
            max_model_calls,
        } => {
            let config = RunConfig {
                mode,
                model: ModelConfig {
                    model,
                    api_key,
                    effort,
                    websocket_url,
                    max_model_calls,
                },
            };
            harness::run(io::stdin().lock(), io::stdout().lock(), config).await?;
        }
    }
    Ok(())
}
