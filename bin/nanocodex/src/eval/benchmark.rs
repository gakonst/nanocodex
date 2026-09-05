use std::path::PathBuf;

use clap::Args;
use eyre::{Result, WrapErr as _};
use nanocodex_eval::{
    Evaluation, EvaluationStatus, coordinator::CoordinatorClient, validate_prepared_eval_host,
};
use serde::Deserialize;

use super::{profile::default_state_dir, systemd};
use crate::{
    RetryableProcessExit, benchmark, config::AgentArgs, observability::ObservabilityArgs, run, tui,
    vm::VmArgs,
};

const CONTROLLER_INSTRUCTIONS: &str = "Act only as a stateless benchmark occupancy controller. \
Use Code Mode host commands to observe the supplied board, systemd units, and resource counters, \
then launch only the requested transient workers. Keep every command and output compact. Never \
browse, inspect source code, edit files, use subagents, or dump worker traces.";

#[derive(Args)]
pub(super) struct Benchmark {
    /// Named benchmark stored in SQLite.
    profile: String,

    /// Runtime harness helper configuration. SQLite owns desired work.
    #[arg(long, env = "NANOCODEX_EVAL_CONFIG", default_value = "nanocodex.toml")]
    config: PathBuf,

    /// Durable SQLite ledger and retained artifacts.
    ///
    /// The workflow and child commands default to ~/.nanocodex/evals.
    #[arg(long, value_name = "DIRECTORY")]
    state_dir: Option<PathBuf>,

    /// Pull all status and execution claims from this coordinator.
    #[arg(long, value_name = "URL", conflicts_with = "state_dir")]
    coordinator: Option<String>,

    /// Run the same benchmark workflow as flushed JSONL without a TUI.
    #[arg(long)]
    headless: bool,

    /// Install and start this controller as a durable user systemd service.
    #[arg(long)]
    systemd: bool,

    /// Host-local cache and temporary workspace for the systemd controller and workers.
    #[arg(long, value_name = "DIRECTORY", requires = "systemd")]
    runtime_dir: Option<PathBuf>,

    #[command(flatten)]
    agent: AgentArgs,

    #[command(flatten)]
    observability: ObservabilityArgs,

    #[command(flatten)]
    vm: VmArgs,
}

impl Benchmark {
    pub(super) async fn run(self) -> Result<()> {
        let Self {
            profile,
            config,
            state_dir,
            coordinator,
            headless,
            systemd,
            runtime_dir,
            mut agent,
            observability,
            vm,
        } = self;
        if systemd {
            return systemd::install(
                &profile,
                &config,
                state_dir.as_deref(),
                coordinator.as_deref(),
                runtime_dir.as_deref(),
            );
        }
        let executable =
            std::env::current_exe().wrap_err("failed to resolve nanocodex executable")?;
        let prompt = benchmark::prompt(
            Some(&profile),
            &config,
            state_dir.as_deref(),
            coordinator.as_deref(),
            Some(&executable),
        );
        let initial = BoardStatus::load(
            Some(&profile),
            &config,
            state_dir.as_deref(),
            coordinator.as_deref(),
        )
        .await?;
        if initial.is_complete() {
            return Ok(());
        }
        validate_prepared_eval_host().wrap_err("evaluation host preflight failed")?;
        agent.restrict_to_host_control(CONTROLLER_INSTRUCTIONS);
        let workflow = if headless {
            let _observability = observability.install(false, agent.cwd())?;
            run::run_prompt(prompt, agent, vm).await
        } else {
            let _observability = observability.install(true, agent.cwd())?;
            let display = format!("/benchmark {profile}");
            tui::run(
                agent,
                vm,
                Some(tui::InitialPrompt::workflow(display, prompt)),
                None,
            )
            .await
        };
        let board = BoardStatus::load(
            Some(&profile),
            &config,
            state_dir.as_deref(),
            coordinator.as_deref(),
        )
        .await?;
        board.require_complete(workflow.as_ref().err())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
struct BoardCounts {
    unclaimed: i64,
    running: i64,
    success: i64,
    failed: i64,
}

impl BoardCounts {
    const fn total(self) -> i64 {
        self.unclaimed + self.running + self.success + self.failed
    }

    const fn is_complete(self) -> bool {
        self.unclaimed == 0 && self.running == 0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
struct BoardStatus {
    tasks: BoardCounts,
}

impl BoardStatus {
    async fn load(
        profile: Option<&str>,
        config: &std::path::Path,
        state_dir: Option<&std::path::Path>,
        coordinator: Option<&str>,
    ) -> Result<Self> {
        if let Some(coordinator) = coordinator {
            let mut client = CoordinatorClient::new(coordinator)?;
            if let Some(profile) = profile {
                client = client.profile(profile);
            }
            let status = client.status().await?;
            return serde_json::from_value(status)
                .wrap_err("coordinator returned an invalid benchmark board status");
        }
        let state_dir = state_dir.map_or_else(default_state_dir, |path| Ok(path.to_path_buf()))?;
        let evaluation = Evaluation::open(config, profile, state_dir)?;
        Ok(evaluation.status()?.into())
    }

    const fn is_complete(self) -> bool {
        self.tasks.is_complete()
    }

    fn require_complete(self, workflow_error: Option<&eyre::Report>) -> Result<()> {
        if self.is_complete() {
            return Ok(());
        }
        let workflow_error = workflow_error.map_or_else(String::new, |error| {
            format!("; agent workflow ended with: {error:#}")
        });
        Err(RetryableProcessExit::new(format!(
            "benchmark board remains incomplete: {}/{} tasks finished ({} unclaimed, {} running, {} failed){workflow_error}",
            self.tasks.success + self.tasks.failed,
            self.tasks.total(),
            self.tasks.unclaimed,
            self.tasks.running,
            self.tasks.failed,
        ))
        .into())
    }
}

impl From<EvaluationStatus> for BoardStatus {
    fn from(status: EvaluationStatus) -> Self {
        Self {
            tasks: BoardCounts {
                unclaimed: status.tasks.unclaimed,
                running: status.tasks.running,
                success: status.tasks.success,
                failed: status.tasks.failed,
            },
        }
    }
}
