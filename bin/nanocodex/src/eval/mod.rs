mod attach;
mod benchmark;
mod coordinator;
mod profile;
mod systemd;

use clap::{Args, Subcommand};
use eyre::Result;

#[derive(Args)]
pub(crate) struct Eval {
    #[command(subcommand)]
    command: EvalCommand,
}

#[derive(Subcommand)]
enum EvalCommand {
    /// Add task rows to a durable benchmark generation.
    Add(profile::Add),

    /// Observe a live local SQLite evaluation ledger in a read-only TUI.
    Attach(attach::Attach),

    /// Launch the agent-owned benchmark workflow in the TUI or headlessly.
    Benchmark(benchmark::Benchmark),

    /// Own one SQLite ledger for pull workers on this machine.
    Coordinator(coordinator::Coordinator),

    /// Inspect one immutable profile revision and its durable progress.
    Status(profile::Status),

    /// Durably execute one agent-selected task repetition from a profile.
    Run(profile::Run),
}

impl Eval {
    pub(crate) async fn run(self) -> Result<()> {
        enable_paint();
        run(self).await
    }
}

fn enable_paint() {
    let enable = yansi::Condition::os_support() && yansi::Condition::tty_and_color_live();
    yansi::whenever(yansi::Condition::cached(enable));
}

async fn run(eval: Eval) -> Result<()> {
    match eval.command {
        EvalCommand::Add(command) => command.run().await?,
        EvalCommand::Attach(command) => command.run().await?,
        EvalCommand::Benchmark(command) => command.run().await?,
        EvalCommand::Coordinator(command) => command.run().await?,
        EvalCommand::Status(command) => command.run().await?,
        EvalCommand::Run(command) => command.run().await?,
    }
    Ok(())
}
