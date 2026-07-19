mod config;
mod mcp;
mod observability;
mod run;
mod tui;

use clap::{Parser, Subcommand, builder::NonEmptyStringValueParser};
use eyre::Result;

use config::AgentArgs;
use observability::ObservabilityArgs;

#[derive(Parser)]
#[command(
    version,
    about = "An interactive coding agent and headless JSONL runner"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    agent: AgentArgs,

    #[command(flatten)]
    observability: ObservabilityArgs,

    /// Submit an initial prompt immediately after the TUI opens.
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    prompt: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Run one prompt and stream JSONL events to stdout.
    Run(run::Run),
}

#[tokio::main]
async fn main() -> Result<()> {
    // Keep direct `cargo run` behavior consistent with the Justfile without
    // requiring shell-specific syntax to load the repository's `.env` file.
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();
    let _observability = cli
        .observability
        .install(cli.command.is_none(), cli.agent.cwd())?;
    match cli.command {
        Some(Command::Run(command)) => command.run(cli.agent).await,
        None => tui::run(cli.agent, cli.prompt).await,
    }
}
