mod run;

use clap::{Parser, Subcommand};
use eyre::Result;

#[derive(Parser)]
#[command(version, about = "A small Harbor-first OpenAI coding agent")]
struct Cli {
    #[command(subcommand)]
    command: Command,
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

    match Cli::parse().command {
        Command::Run(command) => command.run().await,
    }
}
