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

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use crate::Cli;

    #[test]
    fn complete_eval_surface_is_nested_under_nanocodex() {
        for arguments in [
            vec![
                "nanocodex",
                "eval",
                "add",
                "local-smoke",
                "--task",
                "tasks/write-greeting",
                "--trials",
                "5",
            ],
            vec![
                "nanocodex",
                "eval",
                "run",
                "local-smoke",
                "--task",
                "tasks/write-greeting",
            ],
            vec!["nanocodex", "eval", "status", "local-smoke"],
            vec!["nanocodex", "eval", "attach", "local-smoke"],
            vec!["nanocodex", "eval", "benchmark", "local-smoke"],
            vec!["nanocodex", "eval", "coordinator", "local-smoke"],
        ] {
            Cli::try_parse_from(arguments).expect("supported eval command must parse");
        }

        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("eval"));
    }

    #[test]
    fn eval_run_does_not_expose_host_substrate_overrides() {
        for argument in [
            "--vm-cache",
            "--vm-guest-runtime",
            "--vm-refresh",
            "--guest-memory-mb",
        ] {
            assert!(
                Cli::try_parse_from([
                    "nanocodex",
                    "eval",
                    "run",
                    "local-smoke",
                    "--task",
                    "tasks/write-greeting",
                    argument,
                    "override",
                ])
                .is_err(),
                "eval run unexpectedly accepted {argument}"
            );
        }
    }

    #[test]
    fn eval_coordinator_does_not_expose_a_bind_address() {
        assert!(
            Cli::try_parse_from([
                "nanocodex",
                "eval",
                "coordinator",
                "local-smoke",
                "--bind",
                "100.64.0.1",
            ])
            .is_err()
        );
    }
}
