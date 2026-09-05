use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use clap::Args;
use eyre::{Result, WrapErr as _, eyre};
use nanocodex::{Model, Thinking};
use nanocodex_eval::{
    CanonicalTaskRunner, ClaimedEvaluationTask, Evaluation, EvaluationClaim, EvaluationExecution,
    EvaluationSelector, EvaluationTreatment, EvaluationWork, Task,
    coordinator::{CoordinatorClient, CoordinatorError, RemoteClaim, RemoteTaskSource},
    harness::HarnessAuth,
};
use nanocodex_eval_adapters::AdapterCatalog;
use serde::Serialize;

use crate::{
    config::{EvalAgentArgs, SharedAuth},
    observability::ObservabilityArgs,
};

const CONFIG_FILE: &str = "nanocodex.toml";
const CLAIM_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, Args)]
pub(super) struct ProfileTarget {
    /// Named benchmark stored in SQLite. Uses its newest generation.
    profile: String,

    /// Runtime harness helper configuration. SQLite owns desired work.
    #[arg(long, default_value = CONFIG_FILE)]
    config: PathBuf,

    /// Durable SQLite ledger and retained artifacts.
    ///
    /// Defaults to ~/.nanocodex/evals.
    #[arg(long, value_name = "DIRECTORY")]
    state_dir: Option<PathBuf>,

    /// Pull claims from a remote coordinator instead of opening SQLite directly.
    #[arg(long, value_name = "URL", conflicts_with = "state_dir")]
    coordinator: Option<String>,
}

#[derive(Args)]
pub(super) struct Add {
    /// Named benchmark to create or extend.
    profile: String,

    /// Expand one optional TOML profile recipe into SQLite.
    #[arg(long, value_name = "NAME")]
    recipe: Option<String>,

    /// Task package to add. Repeat to add multiple tasks.
    #[arg(long, value_name = "PATH")]
    task: Vec<PathBuf>,

    /// Harness name to add. Repeat to create a matrix.
    #[arg(long, value_name = "NAME")]
    harness: Vec<String>,

    /// Model to add. Repeat to create a matrix.
    #[arg(long)]
    model: Vec<Model>,

    /// Reasoning effort to add. Repeat to create a matrix.
    #[arg(long)]
    thinking: Vec<Thinking>,

    /// Number of rows to materialize for every treatment.
    #[arg(long, default_value_t = 1)]
    trials: u16,

    /// Enable model-facing web search for these rows.
    #[arg(long)]
    web_search: bool,

    /// Start a fresh generation instead of extending the newest one.
    #[arg(long)]
    new: bool,

    /// Optional profile recipes and runtime harness helpers.
    #[arg(long, default_value = CONFIG_FILE)]
    config: PathBuf,

    /// Durable SQLite ledger and retained artifacts.
    #[arg(long, value_name = "DIRECTORY")]
    state_dir: Option<PathBuf>,
}

#[derive(Args)]
pub(super) struct Status {
    #[command(flatten)]
    target: ProfileTarget,

    /// Print the complete machine-readable profile ledger.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
pub(super) struct Run {
    #[command(flatten)]
    target: ProfileTarget,

    /// Optionally restrict the atomic claim to one exact profile task.
    #[arg(long, value_name = "TASK")]
    task: Option<String>,

    /// Select one model when the profile contains a model matrix.
    #[arg(long)]
    model: Option<Model>,

    /// Select one configured external harness. Omission uses Nanocodex.
    #[arg(long, value_name = "NAME")]
    harness: Option<String>,

    /// Advisory stable name used for coordinator task affinity and status.
    #[arg(long, env = "NANOCODEX_WORKER_NAME", value_name = "NAME")]
    worker: Option<String>,

    #[command(flatten)]
    observability: ObservabilityArgs,

    #[command(flatten)]
    agent: EvalAgentArgs,
}

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum RunOutput<'a> {
    Completed {
        profile: &'a str,
        task: &'a str,
        repetition: u16,
        evidence: &'a str,
        status: &'a str,
    },
    InfrastructureFailed {
        profile: &'a str,
        task: &'a str,
        repetition: u16,
        error: &'a str,
    },
    AlreadyComplete {
        profile: &'a str,
        task: &'a str,
    },
    TemporarilyUnavailable {
        profile: &'a str,
        task: &'a str,
        reason: &'a str,
        retry_after_ms: u64,
    },
}

impl Add {
    pub(super) async fn run(self) -> Result<()> {
        let state = self.state_dir.map_or_else(default_state_dir, Ok)?;
        if let Some(recipe) = self.recipe.as_deref() {
            if !self.task.is_empty()
                || !self.harness.is_empty()
                || !self.model.is_empty()
                || !self.thinking.is_empty()
                || self.trials != 1
                || self.web_search
            {
                return Err(eyre!(
                    "--recipe is complete; use either --recipe or explicit work knobs"
                ));
            }
            let selectors = Evaluation::profile_benchmarks(&self.config, Some(recipe))?;
            if selectors.is_empty() {
                Evaluation::add_profile(
                    &self.config,
                    Some(recipe),
                    &state,
                    &self.profile,
                    self.new,
                )?;
            } else {
                let tasks = AdapterCatalog::new(&state)
                    .resolve(&self.config, &selectors)
                    .await?;
                Evaluation::add_profile_with_tasks(
                    &self.config,
                    Some(recipe),
                    tasks,
                    &state,
                    &self.profile,
                    self.new,
                )?;
            }
        } else {
            if self.task.is_empty() {
                return Err(eyre!("at least one --task or --recipe is required"));
            }
            let harnesses = if self.harness.is_empty() {
                vec!["nanocodex".to_owned()]
            } else {
                self.harness
            };
            let models = if self.model.is_empty() {
                vec![Model::default()]
            } else {
                self.model
            };
            let thinking = if self.thinking.is_empty() {
                vec![Thinking::default()]
            } else {
                self.thinking
            };
            let mut work = Vec::new();
            for path in self.task {
                let selector = path.to_string_lossy().into_owned();
                let task = Task::load(&path)?;
                for harness in &harnesses {
                    for model in &models {
                        for thinking in &thinking {
                            work.push(
                                EvaluationWork::new(&selector, task.clone())
                                    .harness(harness)
                                    .model(*model)
                                    .thinking(*thinking)
                                    .web_search(self.web_search)
                                    .trials(self.trials),
                            );
                        }
                    }
                }
            }
            Evaluation::add(&state, &self.profile, &work, self.new)?;
        }
        let status = Evaluation::open(&self.config, Some(&self.profile), state)?.status()?;
        println!(
            "{} {} · {} pre-materialized task row(s)",
            status.profile,
            &status.digest[..status.digest.len().min(12)],
            status.tasks.total()
        );
        Ok(())
    }
}

impl Status {
    pub(super) async fn run(self) -> Result<()> {
        if let Some(coordinator) = &self.target.coordinator {
            let status = CoordinatorClient::new(coordinator)?
                .profile(&self.target.profile)
                .status()
                .await?;
            if self.json {
                serde_json::to_writer_pretty(std::io::stdout().lock(), &status)?;
                println!();
            } else {
                print_remote_status(&status);
            }
            return Ok(());
        }
        let evaluation = self.target.open()?;
        let status = evaluation.status()?;
        if self.json {
            serde_json::to_writer_pretty(std::io::stdout().lock(), &status)?;
            println!();
        } else {
            println!(
                "{} {} · tasks {}/{} finished · {} running · {} failed",
                status.profile,
                &status.digest[..12],
                status.tasks.finished(),
                status.tasks.total(),
                status.tasks.running,
                status.tasks.failed,
            );
            for family in status.families {
                println!(
                    "  {} · {} success · {} failed · {} running · {} unclaimed",
                    family.task, family.success, family.failed, family.running, family.unclaimed
                );
            }
        }
        Ok(())
    }
}

impl Run {
    pub(super) async fn run(self) -> Result<()> {
        let _observability = self.observability.install(false, Path::new("."))?;
        let requested_thinking = self.agent.thinking();
        let selector = self.task.as_ref().map(|task| {
            EvaluationSelector::new(task)
                .harness(self.harness.clone())
                .model(self.model)
                .thinking(requested_thinking)
        });
        if selector.is_none() && (self.harness.is_some() || self.model.is_some()) {
            return Err(eyre!("--harness and --model require --task"));
        }
        if let Some(coordinator) = &self.target.coordinator {
            let mut coordinator =
                CoordinatorClient::new(coordinator)?.profile(&self.target.profile);
            if let Some(worker) = self.worker {
                coordinator = coordinator.worker(worker);
            }
            return run_remote(
                coordinator,
                selector,
                &self.target.config,
                &self.target.profile,
                self.agent,
            )
            .await;
        }
        let evaluation = self.target.open()?;
        let next = match &selector {
            Some(selector) => evaluation.claim(selector)?,
            None => evaluation.claim_next()?,
        };
        match next {
            EvaluationClaim::Run(claim) => {
                let repetition = claim.repetition();
                let task_selector = claim.task_selector().to_owned();
                let task = ClaimedEvaluationTask::from_claim(&claim);
                let result = async {
                    let runner =
                        canonical_runner(self.agent, claim.treatment(), evaluation.name())?;
                    Ok::<_, eyre::Report>(runner.run(task).await?)
                }
                .await;
                match result {
                    Ok(EvaluationExecution::Passed { evidence }) => {
                        claim.succeed(&evidence)?;
                        let evidence = evidence.to_string_lossy();
                        write_json(&RunOutput::Completed {
                            profile: evaluation.name(),
                            task: &task_selector,
                            repetition,
                            evidence: &evidence,
                            status: "passed",
                        })?;
                        Ok(())
                    }
                    Ok(EvaluationExecution::Failed { evidence, failure }) => {
                        claim.fail(Some(&evidence), &failure)?;
                        let evidence = evidence.to_string_lossy();
                        write_json(&RunOutput::Completed {
                            profile: evaluation.name(),
                            task: &task_selector,
                            repetition,
                            evidence: &evidence,
                            status: "failed",
                        })?;
                        Ok(())
                    }
                    Ok(EvaluationExecution::Retry { evidence, failure }) => {
                        claim.retry(evidence.as_deref(), &failure)?;
                        write_json(&RunOutput::InfrastructureFailed {
                            profile: evaluation.name(),
                            task: &task_selector,
                            repetition,
                            error: &failure,
                        })?;
                        Err(eyre!(
                            "task infrastructure failed and row requeued: {failure}"
                        ))
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        claim.retry(None, &message)?;
                        write_json(&RunOutput::InfrastructureFailed {
                            profile: evaluation.name(),
                            task: &task_selector,
                            repetition,
                            error: &message,
                        })?;
                        Err(error).wrap_err("task infrastructure failed and row was requeued")
                    }
                }
            }
            EvaluationClaim::Busy(busy) => {
                write_json(&RunOutput::TemporarilyUnavailable {
                    profile: evaluation.name(),
                    task: self.task.as_deref().unwrap_or("any"),
                    reason: busy.reason,
                    retry_after_ms: busy.retry_after_ms,
                })?;
                Err(eyre!(
                    "temporarily unavailable: {}; retry after {} ms",
                    busy.reason,
                    busy.retry_after_ms
                ))
            }
            EvaluationClaim::Complete => {
                write_json(&RunOutput::AlreadyComplete {
                    profile: evaluation.name(),
                    task: self.task.as_deref().unwrap_or("any"),
                })?;
                Ok(())
            }
        }
    }
}

async fn run_remote(
    coordinator: CoordinatorClient,
    selector: Option<EvaluationSelector>,
    config: &Path,
    profile: &str,
    agent: EvalAgentArgs,
) -> Result<()> {
    let remote_claim = match &selector {
        Some(selector) => coordinator.claim(selector).await?,
        None => coordinator.claim_next().await?,
    };
    match remote_claim {
        RemoteClaim::Run {
            claim,
            repetition,
            task: task_selector,
            task_source,
            treatment,
            ..
        } => {
            let task_materialization: Result<_> = match task_source {
                RemoteTaskSource::Filesystem { root, digest } => Ok((root, digest, None)),
                RemoteTaskSource::Package { key, digest } => {
                    let directory = tempfile::Builder::new()
                        .prefix("nanocodex-eval-task-")
                        .tempdir();
                    match directory {
                        Ok(directory) => {
                            let root = directory.path().join("task");
                            coordinator
                                .materialize_task_package(&key, &root)
                                .await
                                .map_err(eyre::Report::new)
                                .map(|()| (root, digest, Some(directory)))
                        }
                        Err(error) => Err(eyre::Report::new(error)),
                    }
                }
            };
            let (task_root, task_digest, task_materialization) = match task_materialization {
                Ok(materialization) => materialization,
                Err(error) => {
                    let detail = format!("task package materialization failed: {error:#}");
                    coordinator.retry(&claim, &detail).await?;
                    return Err(error).wrap_err(
                        "remote task package failed to materialize and row was requeued",
                    );
                }
            };
            let setup = (|| {
                validate_web_search(&agent, profile, treatment.web_search)?;
                let task = Task::load(&task_root)?;
                if task.package_digest() != task_digest {
                    return Err(eyre!(
                        "task package digest mismatch: expected {task_digest}, found {}",
                        task.package_digest(),
                    ));
                }
                let harness = Evaluation::resolve_harness(config, &treatment.harness)?;
                let output = tempfile::Builder::new()
                    .prefix("nanocodex-eval-worker-")
                    .tempdir()?;
                let output_directory = std::fs::canonicalize(output.path())?;
                let harnesses = harness.iter().cloned().collect();
                let task = ClaimedEvaluationTask::new(
                    task,
                    task_selector.clone(),
                    treatment.clone(),
                    harness,
                    harnesses,
                    &output_directory,
                );
                let runner = canonical_runner(agent, &treatment, profile)?;
                Ok::<_, eyre::Report>((
                    runner,
                    task,
                    output,
                    output_directory,
                    task_materialization,
                ))
            })();
            let (runner, task, _output, output_directory, _task_materialization) = match setup {
                Ok(setup) => setup,
                Err(error) => {
                    let detail = format!("{error:#}");
                    coordinator.retry(&claim, &detail).await?;
                    return Err(error).wrap_err("remote task setup failed and row was requeued");
                }
            };
            let heartbeat_coordinator = coordinator.clone();
            let heartbeat_claim = claim.clone();
            let mut heartbeat = tokio::spawn(async move {
                let mut interval = tokio::time::interval(CLAIM_HEARTBEAT_INTERVAL);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    interval.tick().await;
                    heartbeat_coordinator.heartbeat(&heartbeat_claim).await?;
                }
                #[allow(unreachable_code)]
                Ok::<(), CoordinatorError>(())
            });
            let mut run = Box::pin(runner.run(task));
            let result = tokio::select! {
                result = &mut run => result,
                heartbeat_result = &mut heartbeat => {
                    drop(run);
                    let error = match heartbeat_result {
                        Ok(Err(error)) => eyre::Report::new(error),
                        Ok(Ok(())) => eyre!("remote claim heartbeat stopped unexpectedly"),
                        Err(error) => eyre::Report::new(error),
                    };
                    let detail = format!("claim heartbeat failed: {error:#}");
                    coordinator.retry(&claim, &detail).await?;
                    return Err(error).wrap_err(
                        "remote claim heartbeat failed and row was requeued",
                    );
                }
            };
            heartbeat.abort();
            let _ = heartbeat.await;
            match result {
                Ok(EvaluationExecution::Passed { evidence }) => {
                    coordinator
                        .succeed(&claim, &output_directory, &evidence)
                        .await?;
                    write_json(&RunOutput::Completed {
                        profile,
                        task: &task_selector,
                        repetition,
                        evidence: "coordinator",
                        status: "passed",
                    })?;
                    Ok(())
                }
                Ok(EvaluationExecution::Failed { evidence, failure }) => {
                    coordinator
                        .fail_with_evidence(&claim, &output_directory, &evidence, &failure)
                        .await?;
                    write_json(&RunOutput::Completed {
                        profile,
                        task: &task_selector,
                        repetition,
                        evidence: "coordinator",
                        status: "failed",
                    })?;
                    Ok(())
                }
                Ok(EvaluationExecution::Retry { evidence, failure }) => {
                    if let Some(evidence) = evidence.as_deref() {
                        coordinator
                            .retry_with_evidence(&claim, &output_directory, evidence, &failure)
                            .await?;
                    } else {
                        coordinator.retry(&claim, &failure).await?;
                    }
                    write_json(&RunOutput::InfrastructureFailed {
                        profile,
                        task: &task_selector,
                        repetition,
                        error: &failure,
                    })?;
                    Err(eyre!(
                        "remote task infrastructure failed and row was requeued: {failure}"
                    ))
                }
                Err(error) => {
                    let error = eyre::Report::new(error);
                    let detail = format!("{error:#}");
                    let finish = coordinator
                        .retry_with_evidence(&claim, &output_directory, &output_directory, &detail)
                        .await;
                    finish?;
                    Err(error).wrap_err("remote task infrastructure failed and row was requeued")
                }
            }
        }
        RemoteClaim::Busy {
            reason,
            retry_after_ms,
        } => {
            write_json(&RunOutput::TemporarilyUnavailable {
                profile,
                task: "any",
                reason: &reason,
                retry_after_ms,
            })?;
            Err(eyre!(
                "temporarily unavailable: {reason}; retry after {retry_after_ms} ms"
            ))
        }
        RemoteClaim::Complete => {
            write_json(&RunOutput::AlreadyComplete {
                profile,
                task: "any",
            })?;
            Ok(())
        }
    }
}

fn validate_web_search(agent: &EvalAgentArgs, profile: &str, web_search: bool) -> Result<()> {
    if agent
        .web_search()
        .is_some_and(|requested| requested != web_search)
    {
        return Err(eyre!(
            "--web-search cannot override profile `{profile}`; the profile fixes web_search={web_search}"
        ));
    }
    Ok(())
}

impl ProfileTarget {
    fn open(&self) -> Result<Evaluation> {
        let state_directory = self.state_dir.clone().map_or_else(default_state_dir, Ok)?;
        Ok(Evaluation::open(
            &self.config,
            Some(&self.profile),
            state_directory,
        )?)
    }
}

fn canonical_runner(
    agent: EvalAgentArgs,
    treatment: &EvaluationTreatment,
    profile: &str,
) -> Result<CanonicalTaskRunner> {
    validate_web_search(&agent, profile, treatment.web_search)?;
    let (nanocodex, auth) =
        agent.shared_builder(treatment.model, treatment.thinking, treatment.web_search)?;
    let auth = match auth {
        SharedAuth::ApiKey(api_key) => HarnessAuth::api_key(api_key),
        SharedAuth::AccessToken(access_token) => HarnessAuth::access_token(access_token),
        SharedAuth::AuthFile(path) => HarnessAuth::auth_file(path),
    };
    Ok(CanonicalTaskRunner::new(nanocodex, auth))
}

pub(super) fn default_state_dir() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("NANOCODEX_HOME") {
        return Ok(PathBuf::from(home).join("evals"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| eyre!("HOME is not set; pass --state-dir for durable eval state"))?;
    Ok(PathBuf::from(home).join(".nanocodex/evals"))
}

fn print_remote_status(status: &serde_json::Value) {
    let profile = status["profile"].as_str().unwrap_or("unknown");
    let digest = status["digest"].as_str().unwrap_or("unknown");
    let tasks = &status["tasks"];
    println!(
        "{} {} · tasks {}/{} finished · {} running · {} failed",
        profile,
        &digest[..digest.len().min(12)],
        tasks["success"].as_u64().unwrap_or(0) + tasks["failed"].as_u64().unwrap_or(0),
        count_total(tasks),
        tasks["running"].as_u64().unwrap_or(0),
        tasks["failed"].as_u64().unwrap_or(0),
    );
    for family in status["families"].as_array().into_iter().flatten() {
        println!(
            "  {} · {} success · {} failed · {} running · {} unclaimed",
            family["task"].as_str().unwrap_or("unknown"),
            family["success"].as_u64().unwrap_or(0),
            family["failed"].as_u64().unwrap_or(0),
            family["running"].as_u64().unwrap_or(0),
            family["unclaimed"].as_u64().unwrap_or(0),
        );
    }
}

fn count_total(counts: &serde_json::Value) -> u64 {
    ["unclaimed", "running", "success", "failed"]
        .into_iter()
        .map(|key| counts[key].as_u64().unwrap_or(0))
        .sum()
}

fn write_json(value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(std::io::stdout().lock(), value)?;
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::Parser as _;

    use super::default_state_dir;
    use crate::{Cli, Command, eval::EvalCommand};

    #[test]
    fn run_can_restrict_the_atomic_claim_to_one_profile_task() {
        let cli = Cli::try_parse_from([
            "nanocodex",
            "eval",
            "run",
            "release",
            "--task",
            "terminal/fix-git",
            "--worker",
            "dev-one",
            "--api-key",
            "test-key",
        ])
        .unwrap();
        let Some(Command::Eval(eval)) = cli.command else {
            panic!("expected eval command");
        };
        let EvalCommand::Run(run) = eval.command else {
            panic!("expected profile run");
        };
        assert_eq!(run.task.as_deref(), Some("terminal/fix-git"));
        assert_eq!(run.worker.as_deref(), Some("dev-one"));
    }

    #[test]
    fn run_claims_the_next_row_when_task_is_omitted() {
        let cli = Cli::try_parse_from([
            "nanocodex",
            "eval",
            "run",
            "release",
            "--worker",
            "dev-one",
            "--api-key",
            "test-key",
        ])
        .unwrap();
        let Some(Command::Eval(eval)) = cli.command else {
            panic!("expected eval command");
        };
        let EvalCommand::Run(run) = eval.command else {
            panic!("expected profile run");
        };
        assert!(run.task.is_none());
    }

    #[test]
    fn run_accepts_one_optional_external_harness() {
        let cli = Cli::try_parse_from([
            "nanocodex",
            "eval",
            "run",
            "release",
            "--task",
            "terminal/fix-git",
            "--harness",
            "codex",
            "--api-key",
            "test-key",
        ])
        .unwrap();
        let Some(Command::Eval(eval)) = cli.command else {
            panic!("expected eval command");
        };
        let EvalCommand::Run(run) = eval.command else {
            panic!("expected profile run");
        };
        assert_eq!(run.harness.as_deref(), Some("codex"));
    }

    #[test]
    fn explicit_state_directory_is_optional() {
        let cli = Cli::try_parse_from([
            "nanocodex",
            "eval",
            "status",
            "release",
            "--state-dir",
            "/mnt/evals",
        ])
        .unwrap();
        let Some(Command::Eval(eval)) = cli.command else {
            panic!("expected eval command");
        };
        let EvalCommand::Status(status) = eval.command else {
            panic!("expected profile status");
        };
        assert_eq!(
            status.target.state_dir.as_deref(),
            Some(Path::new("/mnt/evals"))
        );
    }

    #[test]
    fn nanocodex_home_owns_the_default_eval_directory() {
        let path = default_state_dir().unwrap();
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("evals")
        );
    }
}
