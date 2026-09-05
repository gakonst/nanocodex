use std::{
    error::Error,
    fmt, fs,
    future::Future,
    io,
    num::ParseFloatError,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    task::{Context, Poll},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use nanocodex_agent::{
    Nanocodex, NanocodexBuilder, NanocodexError,
    events::{
        AgentEvent, AgentEventKind, AgentEvents, CompactionCompleted, CompactionFailed,
        ModelCallCompleted, ModelCallFailed, ModelWarmupCompleted, ModelWarmupFailed, RunStarted,
        ToolResultEvent,
    },
    session::SessionId,
    transport::ResponsesError,
};
use nanocodex_oai_api::{MODEL, pricing::CostStatus, responses::Usage};
use serde::Deserialize;
use tokio::{sync::broadcast, time::timeout};
use tracing::{Instrument, Span, info, info_span};
use uuid::Uuid;

use crate::{
    AgentMetadata, AgentResult, AgentStatus, CleanupPhase, EvalArtifacts, EvalAttemptOutcome,
    EvalCleanup, EvalEnvironment, EvalEvent, EvalEventAttempt, EvalEventKind, EvalEvents,
    EvalException, EvalExceptionKind, EvalFailure, EvalFailureTiming, EvalOutcome, EvalResult,
    EvalStatus, EvalTiming, PhaseTiming, Task, TaskLoadError, UsageTotals, VerifierResult,
    harness_exec::{HarnessExec, HarnessRunError},
    job::EvalJob,
    native::{NativeAttempt, VerifierExecution},
};

const EVENT_CAPACITY: usize = 16_384;
// A healthy driver normally acknowledges shutdown and emits its retained
// terminal event immediately. Ten seconds bounds how long the evaluator waits
// for that optional terminal snapshot. Resource shutdown remains a mandatory
// join after this deadline: a broken driver quarantines its admission lane
// instead of racing a verifier against live agent work.
const AGENT_CANCELLATION_GRACE: Duration = Duration::from_secs(10);

/// A reusable evaluation recipe. Every task call creates an independent agent
/// session and disposable workspace.
#[derive(Clone)]
pub struct Evaluator {
    inner: Arc<EvaluatorInner>,
}

/// One independently awaitable evaluator invocation and its optional events.
#[must_use = "evaluation runs do nothing unless awaited"]
pub struct EvalRun<T> {
    invocation_id: Uuid,
    events: EvalEvents,
    emitter: RunEmitter,
    future: Pin<Box<dyn Future<Output = Result<T, EvalError>> + Send + 'static>>,
}

/// Deliberate evaluator policy configured before running tasks.
pub struct EvaluatorBuilder {
    nanocodex: NanocodexBuilder,
    output_directory: PathBuf,
    attempt_environment: EvalEnvironment,
    attempt_agent: Option<AttemptAgentFactory>,
}

struct EvaluatorInner {
    nanocodex: NanocodexBuilder,
    job: EvalJob,
    attempt_environment: EvalEnvironment,
    attempt_agent: Option<AttemptAgentFactory>,
}

type AttemptError = Box<dyn Error + Send + Sync + 'static>;
type AttemptAgentFactory = Arc<
    dyn for<'a> Fn(EvalAttempt<'a>, NanocodexBuilder) -> Result<AttemptAgent, AttemptError>
        + Send
        + Sync
        + 'static,
>;

type AttemptVerifierFuture<'a> = Pin<
    Box<dyn Future<Output = Result<AttemptVerification, AttemptVerificationFailure>> + Send + 'a>,
>;
type AttemptVerifierCleanupFuture<'a> = Pin<Box<dyn Future<Output = CleanupPhase> + Send + 'a>>;
type AttemptReadinessFuture =
    Pin<Box<dyn Future<Output = Result<(), AttemptError>> + Send + 'static>>;
type AttemptDriverPreparationFuture =
    Pin<Box<dyn Future<Output = Result<AttemptDriver, AttemptError>> + Send + 'static>>;

/// The Nanocodex configuration and resources owned by one attempt.
pub(crate) struct AttemptAgent {
    driver: AttemptDriverSetup,
    readiness: Option<AttemptReadinessFuture>,
    verifier: Option<Box<dyn AttemptVerifier>>,
}

enum AttemptDriverSetup {
    Ready(AttemptDriver),
    Preparing(AttemptDriverPreparationFuture),
}

enum AttemptDriver {
    Nanocodex(NanocodexBuilder),
    Harness(HarnessExec),
}

/// A verifier that runs against the same retained environment as the agent.
pub(crate) trait AttemptVerifier: Send {
    /// Verifies one completed agent attempt.
    ///
    /// The returned future may borrow the verifier, task, and attempt for its
    /// complete execution. Failures are retained as typed evaluation errors.
    fn verify<'a>(
        &'a mut self,
        task: &'a Task,
        attempt: EvalAttempt<'a>,
    ) -> AttemptVerifierFuture<'a>;

    /// Explicitly joins verifier-owned resources when verification will not run.
    ///
    /// Implementations that own processes, VMs, mounts, or other asynchronous
    /// resources must override this method. The evaluator awaits it on every
    /// post-construction abort path.
    fn shutdown(&mut self) -> AttemptVerifierCleanupFuture<'_> {
        Box::pin(async { CleanupPhase::not_required() })
    }
}

/// A verifier's primary semantic error plus independently retained cleanup.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub(crate) struct AttemptVerificationFailure {
    #[source]
    error: AttemptError,
    occurred_at: DateTime<Utc>,
    /// Cleanup health observed after the primary verification failure.
    pub cleanup: CleanupPhase,
}

impl AttemptVerificationFailure {
    /// Retains a verifier error and the cleanup attempted after it.
    pub(crate) fn new(error: impl Error + Send + Sync + 'static, cleanup: CleanupPhase) -> Self {
        let occurred_at = cleanup
            .timing
            .as_ref()
            .map_or_else(Utc::now, |timing| timing.started_at);
        Self {
            error: Box::new(error),
            occurred_at,
            cleanup,
        }
    }

    /// Retains an error timestamp captured before asynchronous cleanup began.
    pub(crate) fn observed_at(
        error: impl Error + Send + Sync + 'static,
        occurred_at: DateTime<Utc>,
        cleanup: CleanupPhase,
    ) -> Self {
        Self {
            error: Box::new(error),
            occurred_at,
            cleanup,
        }
    }

    fn into_parts(self) -> (AttemptError, DateTime<Utc>, CleanupPhase) {
        (self.error, self.occurred_at, self.cleanup)
    }
}

/// Complete typed output returned by an attempt-owned verifier.
pub(crate) struct AttemptVerification {
    /// Process-equivalent exit status and named rewards.
    pub result: VerifierResult,
    /// Complete captured verifier standard output.
    pub stdout: String,
    /// Complete captured verifier standard error.
    pub stderr: String,
    /// Attempt-owned verifier cleanup health and timing.
    pub cleanup: CleanupPhase,
}

struct AttemptInput {
    task: Task,
    nanocodex: NanocodexBuilder,
    queued_at: DateTime<Utc>,
    run: RunEmitter,
}

type AttemptOutput = EvalAttemptOutcome;

/// Immutable paths and task metadata available while configuring one attempt.
#[derive(Clone, Copy)]
pub(crate) struct EvalAttempt<'a> {
    task: &'a Task,
    directory: &'a Path,
    workspace: &'a Path,
    final_message: Option<&'a str>,
}

/// Failure to configure, execute, verify, or durably retain an attempt.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// A task requires behavior unavailable in the native backend.
    #[error("task {task} cannot run with the native backend: {reason}")]
    UnsupportedNativeTask {
        /// Stable task name.
        task: String,
        /// Unsupported task requirement.
        reason: &'static str,
    },

    /// Filesystem or process I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// A loaded task package changed before an attempt could use it.
    #[error(transparent)]
    TaskPackage(#[from] TaskLoadError),

    /// Retained evaluation output would mutate a hashed task package.
    #[error("evaluation output {output} must not be nested in task package {task}")]
    OutputOverlapsTask {
        /// Prospective canonical output parent.
        output: PathBuf,
        /// Canonical task package root.
        task: PathBuf,
    },

    /// Agent setup or execution failed.
    #[error("Nanocodex failed: {0}")]
    Nanocodex(#[from] NanocodexError),

    /// Agent execution completed but explicit resource cleanup failed.
    #[error("Nanocodex cleanup failed: {0}")]
    AgentCleanup(#[source] NanocodexError),

    /// A pinned external harness child process failed.
    #[error("External harness failed: {0}")]
    Harness(#[source] crate::HarnessExecError),

    /// The attempt backend factory failed.
    #[error("failed to configure attempt agent: {0}")]
    AttemptAgent(#[source] AttemptError),

    /// An attempt-owned verifier failed.
    #[error("attempt verifier failed: {0}")]
    AttemptVerifier(#[source] AttemptError),

    /// Agent execution exceeded the task deadline.
    #[error("agent exceeded its {0:?} timeout")]
    AgentTimeout(Duration),

    /// Verifier execution exceeded the task deadline.
    #[error("verifier exceeded its {0:?} timeout")]
    VerifierTimeout(Duration),

    /// The verifier could not start because its own dependencies or network were unavailable.
    #[error("verifier bootstrap failed: {0}")]
    VerifierBootstrap(String),

    /// The agent firehose ended without a terminal event.
    #[error("agent event stream closed before a terminal event")]
    AgentEventsClosed,

    /// The agent emitted a terminal event whose typed metrics were invalid.
    #[error("failed to decode agent terminal metrics: {0}")]
    AgentTerminal(#[source] serde_json::Error),

    /// Typed artifact JSON could not be encoded or decoded.
    #[error("failed to encode or decode JSON: {0}")]
    Json(#[from] serde_json::Error),

    /// A verifier emitted an invalid numeric reward.
    #[error("invalid verifier reward: {0}")]
    ParseReward(#[from] ParseFloatError),
}

impl<T> EvalRun<T> {
    /// Returns the stable identity carried by every event from this invocation.
    #[must_use]
    pub const fn id(&self) -> Uuid {
        self.invocation_id
    }

    /// Returns a cloneable source of independent event subscriptions.
    #[must_use]
    pub fn events(&self) -> EvalEvents {
        self.events.clone()
    }
}

impl<T> Future for EvalRun<T> {
    type Output = Result<T, EvalError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.future.as_mut().poll(context)
    }
}

impl<T> Drop for EvalRun<T> {
    fn drop(&mut self) {
        self.emitter.cancel();
    }
}

impl Evaluator {
    pub(crate) fn new_builder(nanocodex: NanocodexBuilder) -> EvaluatorBuilder {
        EvaluatorBuilder {
            nanocodex,
            output_directory: PathBuf::from(".nanocodex/evals"),
            attempt_environment: EvalEnvironment::Native,
            attempt_agent: None,
        }
    }

    /// Runs one independent attempt.
    ///
    /// # Errors
    ///
    /// Returns an operational error when the attempt cannot be admitted.
    /// Accepted setup, agent, and verifier failures are returned as typed
    /// [`EvalAttemptOutcome::Unscored`] values.
    pub fn task(&self, task: Task) -> EvalRun<EvalAttemptOutcome> {
        let evaluator = self.clone();
        self.start_run(move |run| async move { evaluator.run_one(task, run).await })
    }

    async fn run_one(&self, task: Task, run: RunEmitter) -> Result<EvalAttemptOutcome, EvalError> {
        let queued_at = Utc::now();
        let result = self
            .run_task(AttemptInput {
                task,
                nanocodex: self.inner.nanocodex.clone(),
                queued_at,
                run: run.clone(),
            })
            .await;
        run.finish(&result);
        result
    }

    fn start_run<T, F, Fut>(&self, work: F) -> EvalRun<T>
    where
        T: Send + 'static,
        F: FnOnce(RunEmitter) -> Fut,
        Fut: Future<Output = Result<T, EvalError>> + Send + 'static,
    {
        let (run, events) = RunEmitter::new(self.inner.job.id());
        let invocation_id = run.invocation_id;
        EvalRun {
            invocation_id,
            events,
            emitter: run.clone(),
            future: Box::pin(work(run)),
        }
    }

    /// Returns the stable identifier shared by this evaluator's attempts.
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.inner.job.id()
    }

    /// Returns the directory containing this evaluator's attempt artifacts.
    #[must_use]
    pub fn directory(&self) -> &std::path::Path {
        self.inner.job.directory()
    }

    /// Returns the execution environment selected for every attempt.
    #[must_use]
    pub fn attempt_environment(&self) -> EvalEnvironment {
        self.inner.attempt_environment
    }

    async fn run_task(&self, input: AttemptInput) -> Result<AttemptOutput, EvalError> {
        let AttemptInput {
            task,
            nanocodex,
            queued_at,
            run,
        } = input;
        let session_id = SessionId::new();
        let attempt_id = session_id.as_uuid();
        let trial_name = trial_name(&task, attempt_id);
        let admitted_at = Utc::now();
        let queue_wait = PhaseTiming {
            started_at: queued_at,
            finished_at: admitted_at,
        };
        let started_at = queued_at;
        let mut emitter = AttemptEmitter::new(run, session_id, &task, &trial_name);
        let span = attempt_span(self, &task, attempt_id, &trial_name);
        record_content(&span, "task.prompt", task.prompt());
        let trace_started = Instant::now();
        let result = self
            .run_task_inner(
                task.clone(),
                nanocodex,
                attempt_id,
                trial_name.clone(),
                queue_wait.clone(),
                &mut emitter,
            )
            .instrument(span.clone())
            .await;
        record_attempt_result(&span, trace_started, &result);
        let outcome = match result {
            Ok(result) => EvalAttemptOutcome::Scored(result),
            Err(failure) => {
                let failure = attempt_failure(
                    self, attempt_id, task, trial_name, started_at, queue_wait, &failure,
                );
                emitter.emit(EvalEventKind::Failed(Box::new(failure.clone())));
                EvalAttemptOutcome::Unscored(failure)
            }
        };
        Ok(outcome)
    }

    async fn run_task_inner(
        &self,
        task: Task,
        nanocodex: NanocodexBuilder,
        attempt_id: Uuid,
        trial_name: String,
        queue_wait: PhaseTiming,
        emitter: &mut AttemptEmitter,
    ) -> Result<EvalResult, AttemptRunFailure> {
        reject_output_overlap(self.inner.job.parent_directory(), task.root())
            .map_err(AttemptRunFailure::new)?;
        task.validate_package()
            .map_err(|error| AttemptRunFailure::new(EvalError::TaskPackage(error)))?;
        let attempt = {
            let span = info_span!(
                target: "nanocodex_eval",
                "eval.environment.setup",
                otel.kind = "internal",
                otel.status_code = tracing::field::Empty,
                eval.task.name = task.name(),
                eval.trial.name = trial_name.as_str(),
                output.directory = %self.inner.job.directory().display(),
                status = tracing::field::Empty,
                error.message = tracing::field::Empty,
                duration_ns = tracing::field::Empty,
            );
            let trace_started = Instant::now();
            let result = span.in_scope(|| {
                validate_attempt_environment(&task, self.inner.attempt_agent.is_some())?;
                NativeAttempt::prepare(self.inner.job.directory(), &trial_name, &task)
            });
            record_span_result(&span, trace_started, &result);
            result.map_err(AttemptRunFailure::new)?
        };
        emitter.emit(EvalEventKind::AttemptStarted {
            prompt: task.prompt().to_owned(),
            workspace: attempt.paths.workspace.clone(),
        });
        let mut agent = self
            .execute_agent(emitter, &task, &attempt, nanocodex)
            .await
            .map_err(|failure| AttemptRunFailure::from_agent(&attempt, failure))?;

        if let Err(error) = task.validate_package() {
            let error = RecordedEvalError::now(EvalError::TaskPackage(error));
            agent.shutdown().await;
            let verifier_cleanup = shutdown_attempt_verifier(&mut agent.verifier).await;
            return Err(AttemptRunFailure::after_agent(
                &attempt,
                &agent,
                error,
                verifier_cleanup,
            ));
        }
        if agent
            .error
            .as_ref()
            .is_some_and(|error| !verifier_workspace_usable_after_agent_error(&error.error))
        {
            agent.shutdown().await;
            let verifier_cleanup = shutdown_attempt_verifier(&mut agent.verifier).await;
            let error = agent
                .error
                .take()
                .unwrap_or_else(|| RecordedEvalError::now(EvalError::AgentEventsClosed));
            return Err(AttemptRunFailure::after_agent(
                &attempt,
                &agent,
                error,
                verifier_cleanup,
            ));
        }
        let final_message = agent
            .result
            .as_ref()
            .map(|result| result.final_message.clone());
        // Joining the agent drops its caller-defined tools before a verifier
        // may shut down the agent environment and launch an isolated verifier
        // environment. The verifier retains the owning environment session;
        // keeping the agent driver alive here would leave a sibling tool
        // capability that correctly prevents graceful VM shutdown.
        agent.shutdown().await;
        emitter.emit(EvalEventKind::VerifierStarted);
        let verifier = match self
            .execute_verifier(
                &task,
                &attempt,
                final_message.as_deref(),
                agent.verifier.take(),
            )
            .await
        {
            Ok(verifier) => verifier,
            Err(failure) => {
                let primary = agent.error.take();
                return Err(AttemptRunFailure::after_verifier_failure(
                    &attempt, &agent, primary, failure,
                ));
            }
        };
        if let Err(error) = task.validate_package() {
            return Err(AttemptRunFailure::after_verifier(
                &attempt,
                &agent,
                &verifier,
                RecordedEvalError::now(EvalError::TaskPackage(error)),
            ));
        }
        emitter.emit(EvalEventKind::VerifierOutput {
            stdout: verifier.stdout.clone(),
            stderr: verifier.stderr.clone(),
        });
        emitter.emit(EvalEventKind::VerifierCompleted(verifier.result.clone()));

        if let Some(error) = verifier_bootstrap_error(&verifier) {
            return Err(AttemptRunFailure::after_verifier(
                &attempt,
                &agent,
                &verifier,
                RecordedEvalError::now(error),
            ));
        }

        let status = verifier_status(&task, &verifier.result);
        let score_outcome = match status {
            EvalStatus::Passed => EvalOutcome::Passed,
            EvalStatus::Failed => EvalOutcome::VerifierFailed,
        };
        let exception = agent
            .error
            .as_ref()
            .map(|error| eval_exception(&error.error, error.occurred_at));
        let result = EvalResult {
            attempt_id,
            task_name: task.name().to_owned(),
            trial_name,
            status,
            outcome: exception
                .as_ref()
                .map_or(score_outcome, |exception| exception.outcome),
            environment: self.inner.attempt_environment,
            agent: agent.result,
            verifier: verifier.result,
            exception,
            timing: EvalTiming {
                started_at: queue_wait.started_at,
                finished_at: Utc::now(),
                queue_wait,
                environment_setup: attempt.setup_timing.clone(),
                environment_readiness: agent.readiness_timing,
                agent_setup: agent.setup_timing,
                agent_execution: agent.execution_timing,
                verifier: verifier.timing,
            },
            cleanup: EvalCleanup {
                agent: agent.cleanup,
                verifier: verifier.cleanup,
            },
            artifacts: EvalArtifacts {
                directory: attempt.paths.root.clone(),
                workspace: attempt.paths.workspace.clone(),
                verifier_output: attempt.paths.verifier_output.clone(),
            },
            task,
        };
        emitter.emit(EvalEventKind::Completed(Box::new(result.clone())));
        Ok(result)
    }

    async fn execute_verifier(
        &self,
        task: &Task,
        attempt: &NativeAttempt,
        final_message: Option<&str>,
        verifier: Option<Box<dyn AttemptVerifier>>,
    ) -> Result<VerifierExecution, VerifierExecutionFailure> {
        let span = info_span!(
            target: "nanocodex_eval",
            "eval.verifier",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            eval.task.name = task.name(),
            verifier.script = %task.verifier().script().display(),
            verifier.timeout_ms = duration_ms(task.verifier().timeout()),
            process.exit.code = tracing::field::Empty,
            verifier.reward.total = tracing::field::Empty,
            verifier.passed = tracing::field::Empty,
            verifier.stdout.bytes = tracing::field::Empty,
            verifier.stderr.bytes = tracing::field::Empty,
            status = tracing::field::Empty,
            error.message = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        let trace_started = Instant::now();
        let result = async {
            if let Some(mut verifier) = verifier {
                let started_at = Utc::now();
                let execution = match verifier
                    .verify(
                        task,
                        EvalAttempt {
                            task,
                            directory: &attempt.paths.root,
                            workspace: &attempt.paths.workspace,
                            final_message,
                        },
                    )
                    .await
                {
                    Ok(execution) => execution,
                    Err(failure) => {
                        let (error, occurred_at, cleanup) = failure.into_parts();
                        let finished_at = cleanup
                            .timing
                            .as_ref()
                            .map_or_else(Utc::now, |timing| timing.started_at);
                        return Err(VerifierExecutionFailure {
                            error: RecordedEvalError {
                                error: EvalError::AttemptVerifier(error),
                                occurred_at,
                            },
                            cleanup,
                            timing: Some(PhaseTiming {
                                started_at,
                                finished_at,
                            }),
                        });
                    }
                };
                Ok(VerifierExecution {
                    result: execution.result,
                    timing: PhaseTiming {
                        started_at,
                        finished_at: execution
                            .cleanup
                            .timing
                            .as_ref()
                            .map_or_else(Utc::now, |timing| timing.started_at),
                    },
                    stdout: execution.stdout,
                    stderr: execution.stderr,
                    cleanup: execution.cleanup,
                })
            } else {
                let started_at = Utc::now();
                attempt.verify(task, final_message).await.map_err(|error| {
                    VerifierExecutionFailure {
                        error: RecordedEvalError::now(error),
                        cleanup: CleanupPhase::not_required(),
                        timing: Some(PhaseTiming::finished(started_at)),
                    }
                })
            }
        }
        .instrument(span.clone())
        .await;
        if let Ok(verifier) = &result {
            let passed = task
                .verifier()
                .scoring_policy()
                .passes(&verifier.result.rewards);
            span.record("process.exit.code", verifier.result.exit_code);
            span.record(
                "verifier.reward.total",
                verifier.result.rewards.values().sum::<f64>(),
            );
            span.record("verifier.passed", passed);
            span.record("verifier.stdout.bytes", verifier.stdout.len());
            span.record("verifier.stderr.bytes", verifier.stderr.len());
            record_content(&span, "verifier.stdout", &verifier.stdout);
            record_content(&span, "verifier.stderr", &verifier.stderr);
        }
        record_span_result(&span, trace_started, &result);
        result
    }

    async fn execute_agent(
        &self,
        emitter: &mut AttemptEmitter,
        task: &Task,
        attempt: &NativeAttempt,
        nanocodex: NanocodexBuilder,
    ) -> Result<AgentExecution, AgentExecutionFailure> {
        let AgentSetup {
            agent,
            verifier,
            readiness_timing,
            timing: setup_timing,
        } = self.setup_agent(emitter, task, attempt, nanocodex).await?;
        match agent {
            PreparedAgent::Nanocodex { agent, events } => {
                self.execute_nanocodex_agent(
                    emitter,
                    task,
                    agent,
                    events,
                    verifier,
                    readiness_timing,
                    setup_timing,
                )
                .await
            }
            PreparedAgent::Harness(harness) => {
                self.execute_harness_agent(
                    emitter,
                    task,
                    attempt,
                    harness,
                    verifier,
                    readiness_timing,
                    setup_timing,
                )
                .await
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_nanocodex_agent(
        &self,
        emitter: &mut AttemptEmitter,
        task: &Task,
        agent: Nanocodex,
        mut events: AgentEvents,
        verifier: Option<Box<dyn AttemptVerifier>>,
        readiness_timing: PhaseTiming,
        setup_timing: PhaseTiming,
    ) -> Result<AgentExecution, AgentExecutionFailure> {
        let execution_started = Utc::now();
        let span = info_span!(
            target: "nanocodex_eval",
            "eval.agent.execution",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            eval.task.name = task.name(),
            eval.attempt.id = %emitter.attempt_id,
            agent.timeout_ms = duration_ms(task.agent_timeout()),
            status = tracing::field::Empty,
            error.message = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        let trace_started = Instant::now();
        let result = async {
            let turn = agent.prompt(task.agent_prompt()).await?;
            let mut observation = AgentObservation::default();
            let event_result = timeout(
                task.agent_timeout(),
                receive_agent_terminal(&mut events, emitter, &mut observation),
            )
            .await;
            match event_result {
                Ok(Ok(terminal)) => {
                    let (primary, final_message) = match turn.result().await {
                        Ok(result) => (None, result.into_final_message()),
                        Err(error) => (
                            Some(EvalError::Nanocodex(error)),
                            observation.final_message.clone(),
                        ),
                    };
                    observation.final_message = final_message;
                    let selection = observation.select_result(Some(&terminal));
                    if let Some(error) = &selection.terminal_error {
                        tracing::warn!(
                            target: "nanocodex_eval",
                            error = %error,
                            "failed to decode terminal agent metrics; retaining \
                             completed-operation lower bound"
                        );
                    }
                    let primary = primary
                        .map(RecordedEvalError::now)
                        .or_else(|| selection.terminal_error.map(RecordedEvalError::now));
                    Ok(AgentRunState::Finished(AgentTurnOutcome {
                        primary,
                        result: selection.result,
                        result_is_lower_bound: selection.used_lower_bound,
                    }))
                }
                Ok(Err(error)) => {
                    let selection = observation.select_result(None);
                    Ok(AgentRunState::Finished(AgentTurnOutcome {
                        primary: Some(RecordedEvalError::now(error)),
                        result: selection.result,
                        result_is_lower_bound: selection.used_lower_bound,
                    }))
                }
                Err(_) => {
                    let primary =
                        RecordedEvalError::now(EvalError::AgentTimeout(task.agent_timeout()));
                    Ok(AgentRunState::TimedOut {
                        primary,
                        observation,
                    })
                }
            }
        };
        let result = result.instrument(span.clone()).await;
        record_span_result(&span, trace_started, &result);
        let mut result = result.map_err(RecordedEvalError::now);
        let execution_timing = PhaseTiming::finished(execution_started);
        if let Ok(AgentRunState::Finished(outcome)) = &mut result {
            outcome.apply_lower_bound_duration(phase_timing_ns(&execution_timing));
        }
        let cleanup_started = Utc::now();
        let (outcome, cleanup, retained_agent) = match result {
            Ok(AgentRunState::Finished(outcome)) => {
                (outcome, CleanupPhase::not_required(), Some(agent))
            }
            Ok(AgentRunState::TimedOut {
                primary,
                mut observation,
            }) => {
                let recovery = recover_timed_out_agent(
                    AGENT_CANCELLATION_GRACE,
                    agent.shutdown(),
                    receive_agent_terminal(&mut events, emitter, &mut observation),
                )
                .await;
                let cleanup = match recovery.shutdown {
                    Ok(()) => CleanupPhase::completed(cleanup_started),
                    Err(error) => CleanupPhase::failed(cleanup_started, &error),
                };
                if recovery.grace_elapsed && recovery.terminal.is_none() {
                    tracing::warn!(
                        target: "nanocodex_eval",
                        grace_ms = duration_ms(AGENT_CANCELLATION_GRACE),
                        primary_error = %primary.error,
                        "agent terminal recovery exceeded its private grace; \
                         resource shutdown remained joined"
                    );
                }
                let terminal = match recovery.terminal {
                    Some(Ok(terminal)) => Some(terminal),
                    Some(Err(error)) => {
                        tracing::warn!(
                            target: "nanocodex_eval",
                            error = %error,
                            primary_error = %primary.error,
                            "agent events closed without a terminal snapshot after timeout; \
                             retaining completed-operation lower bound"
                        );
                        None
                    }
                    None => None,
                };
                let selection = observation.select_result(terminal.as_ref());
                if let Some(error) = &selection.terminal_error {
                    tracing::warn!(
                        target: "nanocodex_eval",
                        error = %error,
                        primary_error = %primary.error,
                        "failed to decode terminal metrics after agent timeout; retaining \
                         completed-operation lower bound"
                    );
                }
                let outcome = AgentTurnOutcome {
                    primary: Some(primary),
                    result: selection.result,
                    result_is_lower_bound: selection.used_lower_bound,
                };
                let mut outcome = outcome;
                outcome.apply_lower_bound_duration(phase_timing_ns(&execution_timing));
                (outcome, cleanup, None)
            }
            Err(error) => {
                let shutdown = agent.shutdown().await;
                let cleanup = match shutdown {
                    Ok(()) => CleanupPhase::completed(cleanup_started),
                    Err(error) => CleanupPhase::failed(cleanup_started, &error),
                };
                (
                    AgentTurnOutcome {
                        primary: Some(error),
                        result: None,
                        result_is_lower_bound: false,
                    },
                    cleanup,
                    None,
                )
            }
        };
        let error = outcome.primary.or_else(|| {
            outcome
                .result
                .is_none()
                .then(|| RecordedEvalError::now(EvalError::AgentEventsClosed))
        });
        Ok(AgentExecution {
            result: outcome.result,
            error,
            verifier,
            readiness_timing,
            setup_timing,
            execution_timing,
            cleanup,
            retained_agent,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_harness_agent(
        &self,
        emitter: &AttemptEmitter,
        task: &Task,
        attempt: &NativeAttempt,
        harness: HarnessExec,
        verifier: Option<Box<dyn AttemptVerifier>>,
        readiness_timing: PhaseTiming,
        setup_timing: PhaseTiming,
    ) -> Result<AgentExecution, AgentExecutionFailure> {
        let execution_started = Utc::now();
        let span = info_span!(
            target: "nanocodex_eval",
            "eval.agent.execution",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            eval.task.name = task.name(),
            eval.attempt.id = %emitter.attempt_id,
            agent.kind = "external_harness",
            agent.timeout_ms = duration_ms(task.agent_timeout()),
            status = tracing::field::Empty,
            error.message = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        let trace_started = Instant::now();
        let execution = harness
            .run(
                &attempt.paths.workspace,
                &attempt.paths.root,
                task.prompt(),
                task.agent_timeout(),
            )
            .instrument(span.clone())
            .await;
        let execution_timing = PhaseTiming::finished(execution_started);
        let error = execution.error.map(|error| {
            RecordedEvalError::now(match error {
                HarnessRunError::Timeout(timeout) => EvalError::AgentTimeout(timeout),
                HarnessRunError::Execution(error) => EvalError::Harness(error),
            })
        });
        if let Some(error) = &error {
            span.record("status", "failed");
            span.record("otel.status_code", "ERROR");
            span.record("error.message", error.error.to_string());
        } else {
            span.record("status", "completed");
            span.record("otel.status_code", "OK");
        }
        span.record("duration_ns", elapsed_ns(trace_started));
        Ok(AgentExecution {
            result: execution.result,
            error,
            verifier,
            readiness_timing,
            setup_timing,
            execution_timing,
            cleanup: execution.cleanup,
            retained_agent: None,
        })
    }

    async fn setup_agent(
        &self,
        emitter: &AttemptEmitter,
        task: &Task,
        attempt: &NativeAttempt,
        nanocodex: NanocodexBuilder,
    ) -> Result<AgentSetup, AgentExecutionFailure> {
        let readiness_started = Utc::now();
        let span = info_span!(
            target: "nanocodex_eval",
            "eval.agent.setup",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            eval.task.name = task.name(),
            eval.attempt.id = %emitter.attempt_id,
            workspace = %attempt.paths.workspace.display(),
            status = tracing::field::Empty,
            error.message = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        let trace_started = Instant::now();
        let result = async {
            let builder = nanocodex
                .workspace(&attempt.paths.workspace)
                .session_id(emitter.session_id);
            let configured = if let Some(factory) = &self.inner.attempt_agent {
                match factory(
                    EvalAttempt {
                        task,
                        directory: &attempt.paths.root,
                        workspace: &attempt.paths.workspace,
                        final_message: None,
                    },
                    builder,
                ) {
                    Ok(configured) => configured,
                    Err(error) => {
                        let error = RecordedEvalError::now(EvalError::AttemptAgent(error));
                        return Err(AgentExecutionFailure::setup(
                            error,
                            CleanupPhase::not_required(),
                            None,
                        ));
                    }
                }
            } else {
                AttemptAgent::new(builder)
            };
            let (driver, readiness, mut verifier) = configured.into_parts();
            if let Some(readiness) = readiness
                && let Err(error) = readiness.await
            {
                let error = RecordedEvalError::now(EvalError::AttemptAgent(error));
                let verifier_cleanup = shutdown_attempt_verifier(&mut verifier).await;
                return Err(AgentExecutionFailure::setup(error, verifier_cleanup, None));
            }
            let readiness_timing = PhaseTiming::finished(readiness_started);
            let setup_started = Utc::now();
            let driver = match driver {
                AttemptDriverSetup::Ready(driver) => driver,
                AttemptDriverSetup::Preparing(preparation) => match preparation.await {
                    Ok(driver) => driver,
                    Err(error) => {
                        let error = RecordedEvalError::now(EvalError::AttemptAgent(error));
                        let verifier_cleanup = shutdown_attempt_verifier(&mut verifier).await;
                        return Err(AgentExecutionFailure::setup(
                            error,
                            verifier_cleanup,
                            Some(readiness_timing),
                        ));
                    }
                },
            };
            match driver {
                AttemptDriver::Nanocodex(builder) => match builder.build() {
                    Ok((agent, events)) => Ok(AgentSetup {
                        agent: PreparedAgent::Nanocodex { agent, events },
                        verifier,
                        readiness_timing,
                        timing: PhaseTiming::finished(setup_started),
                    }),
                    Err(error) => {
                        let error = RecordedEvalError::now(EvalError::Nanocodex(error));
                        let verifier_cleanup = shutdown_attempt_verifier(&mut verifier).await;
                        Err(AgentExecutionFailure::setup(
                            error,
                            verifier_cleanup,
                            Some(readiness_timing),
                        ))
                    }
                },
                AttemptDriver::Harness(codex) => Ok(AgentSetup {
                    agent: PreparedAgent::Harness(codex),
                    verifier,
                    readiness_timing,
                    timing: PhaseTiming::finished(setup_started),
                }),
            }
        }
        .instrument(span.clone())
        .await;
        record_span_result(&span, trace_started, &result);
        result
    }
}

async fn shutdown_attempt_verifier(
    verifier: &mut Option<Box<dyn AttemptVerifier>>,
) -> CleanupPhase {
    let Some(mut verifier) = verifier.take() else {
        return CleanupPhase::not_required();
    };
    verifier.shutdown().await
}

struct AgentExecution {
    result: Option<AgentResult>,
    error: Option<RecordedEvalError>,
    verifier: Option<Box<dyn AttemptVerifier>>,
    readiness_timing: PhaseTiming,
    setup_timing: PhaseTiming,
    execution_timing: PhaseTiming,
    cleanup: CleanupPhase,
    retained_agent: Option<Nanocodex>,
}

impl AgentExecution {
    async fn shutdown(&mut self) {
        let Some(agent) = self.retained_agent.take() else {
            return;
        };
        let started_at = Utc::now();
        self.cleanup = match agent.shutdown().await {
            Ok(()) => CleanupPhase::completed(started_at),
            Err(error) => CleanupPhase::failed(started_at, &error),
        };
    }
}

#[derive(Debug)]
struct AgentExecutionFailure {
    error: RecordedEvalError,
    result: Option<AgentResult>,
    cleanup: CleanupPhase,
    verifier_cleanup: CleanupPhase,
    readiness_timing: Option<PhaseTiming>,
    setup_timing: Option<PhaseTiming>,
    execution_timing: Option<PhaseTiming>,
}

impl AgentExecutionFailure {
    const fn setup(
        error: RecordedEvalError,
        verifier_cleanup: CleanupPhase,
        readiness_timing: Option<PhaseTiming>,
    ) -> Self {
        Self {
            error,
            result: None,
            cleanup: CleanupPhase::not_required(),
            verifier_cleanup,
            readiness_timing,
            setup_timing: None,
            execution_timing: None,
        }
    }
}

impl fmt::Display for AgentExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.error.fmt(formatter)
    }
}

impl Error for AgentExecutionFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error.error)
    }
}

#[derive(Debug)]
struct VerifierExecutionFailure {
    error: RecordedEvalError,
    cleanup: CleanupPhase,
    timing: Option<PhaseTiming>,
}

impl fmt::Display for VerifierExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.error.fmt(formatter)
    }
}

impl Error for VerifierExecutionFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error.error)
    }
}

struct AgentTurnOutcome {
    primary: Option<RecordedEvalError>,
    result: Option<AgentResult>,
    result_is_lower_bound: bool,
}

impl AgentTurnOutcome {
    const fn apply_lower_bound_duration(&mut self, duration_ns: u64) {
        if !self.result_is_lower_bound {
            return;
        }
        if let Some(result) = &mut self.result {
            result.metadata.duration_ns = duration_ns;
            result.metadata.duration_ms = duration_ns / 1_000_000;
        }
    }
}

enum AgentRunState {
    Finished(AgentTurnOutcome),
    TimedOut {
        primary: RecordedEvalError,
        observation: AgentObservation,
    },
}

struct TimedOutAgentRecovery<T, S> {
    terminal: Option<T>,
    shutdown: S,
    grace_elapsed: bool,
}

#[derive(Debug)]
struct RecordedEvalError {
    error: EvalError,
    occurred_at: DateTime<Utc>,
}

impl RecordedEvalError {
    fn now(error: EvalError) -> Self {
        Self {
            error,
            occurred_at: Utc::now(),
        }
    }
}

#[derive(Default)]
struct AgentObservation {
    final_message: String,
    run: Option<ObservedRun>,
    steers: u32,
    model_calls_started: u32,
    compactions_started: u32,
    tool_calls_started: u32,
    tool_work_duration_ns: u64,
    connection_attempts: u32,
    websocket_reconnects: u32,
    response_attempts: u32,
    response_retries: u32,
    connection_duration_ns: u64,
    retry_backoff_duration_ns: u64,
    pending_retry_delay_ns: Option<u64>,
    completed: CompletedBillableOperations,
}

struct ObservedRun {
    model: String,
    effort: String,
    transport: String,
    orchestration: String,
}

#[derive(Deserialize)]
struct AttemptRetryObservation {
    delay_ns: u64,
}

#[derive(Deserialize)]
struct ConnectionCompletedObservation {
    purpose: String,
    duration_ns: u64,
}

#[derive(Deserialize)]
struct ConnectionFailedObservation {
    duration_ns: u64,
}

struct AgentResultSelection {
    result: Option<AgentResult>,
    terminal_error: Option<EvalError>,
    used_lower_bound: bool,
}

#[derive(Default)]
struct CompletedBillableOperations {
    usage: UsageTotals,
    warmup_usage: UsageTotals,
    model_calls: u32,
    compactions: u32,
    tool_calls: u32,
    response_attempts: u32,
    response_retries: u32,
    model_duration_ns: u64,
    warmup_duration_ns: u64,
    completed_responses: u32,
}

struct AttemptRunFailure {
    error: EvalError,
    occurred_at: DateTime<Utc>,
    agent: Option<AgentResult>,
    verifier: Option<VerifierResult>,
    cleanup: EvalCleanup,
    environment_setup: Option<PhaseTiming>,
    environment_readiness: Option<PhaseTiming>,
    agent_setup: Option<PhaseTiming>,
    agent_execution: Option<PhaseTiming>,
    verifier_timing: Option<PhaseTiming>,
}

impl AttemptRunFailure {
    fn new(error: EvalError) -> Self {
        Self {
            error,
            occurred_at: Utc::now(),
            agent: None,
            verifier: None,
            cleanup: EvalCleanup::default(),
            environment_setup: None,
            environment_readiness: None,
            agent_setup: None,
            agent_execution: None,
            verifier_timing: None,
        }
    }

    fn from_agent(attempt: &NativeAttempt, failure: AgentExecutionFailure) -> Self {
        let RecordedEvalError { error, occurred_at } = failure.error;
        Self {
            error,
            occurred_at,
            agent: failure.result,
            verifier: None,
            cleanup: EvalCleanup {
                agent: failure.cleanup,
                verifier: failure.verifier_cleanup,
            },
            environment_setup: Some(attempt.setup_timing.clone()),
            environment_readiness: failure.readiness_timing,
            agent_setup: failure.setup_timing,
            agent_execution: failure.execution_timing,
            verifier_timing: None,
        }
    }

    fn after_agent(
        attempt: &NativeAttempt,
        agent: &AgentExecution,
        error: RecordedEvalError,
        verifier_cleanup: CleanupPhase,
    ) -> Self {
        Self {
            error: error.error,
            occurred_at: error.occurred_at,
            agent: agent.result.clone(),
            verifier: None,
            cleanup: EvalCleanup {
                agent: agent.cleanup.clone(),
                verifier: verifier_cleanup,
            },
            environment_setup: Some(attempt.setup_timing.clone()),
            environment_readiness: Some(agent.readiness_timing.clone()),
            agent_setup: Some(agent.setup_timing.clone()),
            agent_execution: Some(agent.execution_timing.clone()),
            verifier_timing: None,
        }
    }

    fn after_verifier_failure(
        attempt: &NativeAttempt,
        agent: &AgentExecution,
        primary: Option<RecordedEvalError>,
        failure: VerifierExecutionFailure,
    ) -> Self {
        let VerifierExecutionFailure {
            error: verifier_error,
            cleanup,
            timing,
        } = failure;
        if let Some(primary) = &primary {
            tracing::warn!(
                target: "nanocodex_eval",
                primary_error = %primary.error,
                verifier_error = %verifier_error.error,
                "verifier failed after an earlier agent exception"
            );
        }
        let error = primary.unwrap_or(verifier_error);
        Self {
            error: error.error,
            occurred_at: error.occurred_at,
            agent: agent.result.clone(),
            verifier: None,
            cleanup: EvalCleanup {
                agent: agent.cleanup.clone(),
                verifier: cleanup,
            },
            environment_setup: Some(attempt.setup_timing.clone()),
            environment_readiness: Some(agent.readiness_timing.clone()),
            agent_setup: Some(agent.setup_timing.clone()),
            agent_execution: Some(agent.execution_timing.clone()),
            verifier_timing: timing,
        }
    }

    fn after_verifier(
        attempt: &NativeAttempt,
        agent: &AgentExecution,
        verifier: &VerifierExecution,
        error: RecordedEvalError,
    ) -> Self {
        Self {
            error: error.error,
            occurred_at: error.occurred_at,
            agent: agent.result.clone(),
            verifier: Some(verifier.result.clone()),
            cleanup: EvalCleanup {
                agent: agent.cleanup.clone(),
                verifier: verifier.cleanup.clone(),
            },
            environment_setup: Some(attempt.setup_timing.clone()),
            environment_readiness: Some(agent.readiness_timing.clone()),
            agent_setup: Some(agent.setup_timing.clone()),
            agent_execution: Some(agent.execution_timing.clone()),
            verifier_timing: Some(verifier.timing.clone()),
        }
    }
}

async fn receive_agent_terminal(
    events: &mut AgentEvents,
    emitter: &mut AttemptEmitter,
    observation: &mut AgentObservation,
) -> Result<AgentEvent, EvalError> {
    loop {
        let event = events.recv().await.ok_or(EvalError::AgentEventsClosed)?;
        observation.observe(&event)?;
        let terminal = event.kind.is_terminal();
        emitter.emit(EvalEventKind::Agent(event.clone()));
        if terminal {
            return Ok(event);
        }
    }
}

async fn recover_timed_out_agent<S, R>(
    grace: Duration,
    shutdown: S,
    terminal: R,
) -> TimedOutAgentRecovery<R::Output, S::Output>
where
    S: Future,
    R: Future,
{
    tokio::pin!(shutdown);
    tokio::pin!(terminal);
    let deadline = tokio::time::sleep(grace);
    tokio::pin!(deadline);
    let mut shutdown_output = None;
    let mut terminal_output = None;
    let grace_elapsed = loop {
        if shutdown_output.is_some() && terminal_output.is_some() {
            break false;
        }
        tokio::select! {
            biased;
            output = &mut shutdown, if shutdown_output.is_none() => {
                shutdown_output = Some(output);
            }
            output = &mut terminal, if terminal_output.is_none() => {
                terminal_output = Some(output);
            }
            () = &mut deadline => break true,
        }
    };
    let shutdown = match shutdown_output {
        Some(output) => output,
        None => shutdown.await,
    };
    TimedOutAgentRecovery {
        terminal: terminal_output,
        shutdown,
        grace_elapsed,
    }
}

impl AgentObservation {
    fn observe(&mut self, event: &AgentEvent) -> Result<(), EvalError> {
        match event.kind {
            AgentEventKind::RunStarted => {
                let run: RunStarted = event.decode_payload()?;
                self.run = Some(ObservedRun {
                    model: run.model,
                    effort: run.effort,
                    transport: run.transport,
                    orchestration: run.orchestration,
                });
            }
            AgentEventKind::AssistantMessage => {
                let message: nanocodex_agent::events::AssistantMessage = event.decode_payload()?;
                self.final_message = message.text;
            }
            AgentEventKind::RunSteered => {
                self.steers = self.steers.saturating_add(1);
            }
            AgentEventKind::ModelCallStarted => {
                self.model_calls_started = self.model_calls_started.saturating_add(1);
            }
            AgentEventKind::ModelCompactionStarted => {
                self.compactions_started = self.compactions_started.saturating_add(1);
            }
            AgentEventKind::ToolCall => {
                self.tool_calls_started = self.tool_calls_started.saturating_add(1);
            }
            AgentEventKind::ToolResult => {
                let result: ToolResultEvent = event.decode_payload()?;
                self.tool_work_duration_ns = self
                    .tool_work_duration_ns
                    .saturating_add(result.duration_ns);
            }
            AgentEventKind::ModelAttemptStarted => {
                self.response_attempts = self.response_attempts.saturating_add(1);
                if let Some(delay_ns) = self.pending_retry_delay_ns.take() {
                    self.retry_backoff_duration_ns =
                        self.retry_backoff_duration_ns.saturating_add(delay_ns);
                }
            }
            AgentEventKind::ModelAttemptRetrying => {
                let retry: AttemptRetryObservation = event.decode_payload()?;
                self.response_retries = self.response_retries.saturating_add(1);
                self.pending_retry_delay_ns = Some(
                    self.pending_retry_delay_ns
                        .unwrap_or_default()
                        .saturating_add(retry.delay_ns),
                );
            }
            AgentEventKind::ModelConnectionStarted => {
                self.connection_attempts = self.connection_attempts.saturating_add(1);
            }
            AgentEventKind::ModelConnectionCompleted => {
                let connection: ConnectionCompletedObservation = event.decode_payload()?;
                self.connection_duration_ns = self
                    .connection_duration_ns
                    .saturating_add(connection.duration_ns);
                if connection.purpose != "initial" {
                    self.websocket_reconnects = self.websocket_reconnects.saturating_add(1);
                }
            }
            AgentEventKind::ModelConnectionFailed => {
                let connection: ConnectionFailedObservation = event.decode_payload()?;
                self.connection_duration_ns = self
                    .connection_duration_ns
                    .saturating_add(connection.duration_ns);
            }
            AgentEventKind::ModelWarmupCompleted => {
                let completed: ModelWarmupCompleted = event.decode_payload()?;
                self.completed.warmup_duration_ns = self
                    .completed
                    .warmup_duration_ns
                    .saturating_add(completed.duration_ns);
                if completed.source == "response" {
                    self.completed
                        .observe(completed.usage.as_ref(), true, completed.attempt);
                }
            }
            AgentEventKind::ModelWarmupFailed => {
                let failed: ModelWarmupFailed = event.decode_payload()?;
                self.completed.warmup_duration_ns = self
                    .completed
                    .warmup_duration_ns
                    .saturating_add(failed.duration_ns);
            }
            AgentEventKind::ModelCallCompleted => {
                let completed: ModelCallCompleted = event.decode_payload()?;
                self.completed.model_calls = self.completed.model_calls.saturating_add(1);
                self.completed.tool_calls = self
                    .completed
                    .tool_calls
                    .saturating_add(u32::try_from(completed.tool_calls).unwrap_or(u32::MAX));
                self.completed.model_duration_ns = self
                    .completed
                    .model_duration_ns
                    .saturating_add(completed.duration_ns);
                self.completed
                    .observe(completed.usage.as_ref(), false, Some(completed.attempt));
            }
            AgentEventKind::ModelCallFailed => {
                let failed: ModelCallFailed = event.decode_payload()?;
                self.completed.model_duration_ns = self
                    .completed
                    .model_duration_ns
                    .saturating_add(failed.duration_ns);
            }
            AgentEventKind::ModelCompactionCompleted => {
                let completed: CompactionCompleted = event.decode_payload()?;
                self.completed.compactions = self.completed.compactions.saturating_add(1);
                self.completed.model_duration_ns = self
                    .completed
                    .model_duration_ns
                    .saturating_add(completed.duration_ns);
                self.completed
                    .observe(completed.usage.as_ref(), false, Some(completed.attempt));
            }
            AgentEventKind::ModelCompactionFailed => {
                let failed: CompactionFailed = event.decode_payload()?;
                self.completed.model_duration_ns = self
                    .completed
                    .model_duration_ns
                    .saturating_add(failed.duration_ns);
            }
            _ => {}
        }
        Ok(())
    }

    fn select_result(&self, terminal: Option<&AgentEvent>) -> AgentResultSelection {
        let Some(terminal) = terminal else {
            let result = self.lower_bound_result(None);
            return AgentResultSelection {
                used_lower_bound: result.is_some(),
                result,
                terminal_error: None,
            };
        };
        match AgentResult::from_terminal(self.final_message.clone(), terminal) {
            Ok(result) => AgentResultSelection {
                result: Some(result),
                terminal_error: None,
                used_lower_bound: false,
            },
            Err(error) => {
                let result = self.lower_bound_result(Some(terminal.kind));
                AgentResultSelection {
                    used_lower_bound: result.is_some(),
                    result,
                    terminal_error: Some(error),
                }
            }
        }
    }

    fn lower_bound_result(&self, terminal_kind: Option<AgentEventKind>) -> Option<AgentResult> {
        if self.run.is_none()
            && self.completed.completed_responses == 0
            && self.model_calls_started == 0
            && self.compactions_started == 0
            && self.tool_calls_started == 0
            && self.connection_attempts == 0
            && self.response_attempts == 0
        {
            return None;
        }
        let run = self.run.as_ref();
        let model = run.map_or_else(|| MODEL.to_owned(), |run| run.model.clone());
        let effort = run.map_or_else(String::new, |run| run.effort.clone());
        let cost_usd = None;
        let model_calls = self.model_calls_started.max(self.completed.model_calls);
        let compactions = self.compactions_started.max(self.completed.compactions);
        let tool_calls = self.tool_calls_started.max(self.completed.tool_calls);
        let response_attempts = self.response_attempts.max(self.completed.response_attempts);
        let response_retries = self.response_retries.max(self.completed.response_retries);
        let metadata = AgentMetadata {
            status: match terminal_kind {
                Some(AgentEventKind::RunCompleted) => AgentStatus::Completed,
                Some(AgentEventKind::RunFailed) => AgentStatus::Failed,
                _ => AgentStatus::Cancelled,
            },
            model: model.clone(),
            effort: effort.clone(),
            reasoning_mode: None,
            transport: run.map_or_else(String::new, |run| run.transport.clone()),
            orchestration: run.map_or_else(String::new, |run| run.orchestration.clone()),
            runtime_completeness: crate::MeasurementCompleteness::ObservedLowerBound,
            duration_ms: 0,
            duration_ns: 0,
            model_calls,
            steers: self.steers,
            compactions,
            tool_calls,
            connection_attempts: self.connection_attempts,
            websocket_reconnects: self.websocket_reconnects,
            response_attempts,
            response_retries,
            connection_duration_ns: self.connection_duration_ns,
            retry_backoff_duration_ns: self.retry_backoff_duration_ns,
            model_duration_ns: self.completed.model_duration_ns,
            warmup_duration_ns: self.completed.warmup_duration_ns,
            tool_work_duration_ns: self.tool_work_duration_ns,
            tool_wall_duration_ns: 0,
            usage: self.completed.usage.clone(),
            warmup_usage: self.completed.warmup_usage.clone(),
            cost_usd,
            cost_status: CostStatus::UsageNotReported.as_str().to_owned(),
            estimated_cost: None,
        };
        Some(AgentResult {
            final_message: self.final_message.clone(),
            model,
            effort,
            model_calls,
            tool_calls,
            usage: self.completed.usage.clone(),
            cost_usd,
            metadata,
        })
    }
}

impl CompletedBillableOperations {
    fn observe(&mut self, usage: Option<&Usage>, warmup: bool, attempt: Option<u32>) {
        self.completed_responses = self.completed_responses.saturating_add(1);
        if let Some(attempt) = attempt {
            self.response_attempts = self.response_attempts.saturating_add(attempt);
            self.response_retries = self
                .response_retries
                .saturating_add(attempt.saturating_sub(1));
        }
        if let Some(usage) = usage {
            if warmup {
                self.warmup_usage.add(usage);
            } else {
                self.usage.add(usage);
            }
        }
    }
}

impl UsageTotals {
    fn add(&mut self, usage: &Usage) {
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.cached_input_tokens = self.cached_input_tokens.saturating_add(
            usage
                .input_tokens_details
                .as_ref()
                .map_or(0, |details| details.cached_tokens),
        );
        self.cache_write_input_tokens = self.cache_write_input_tokens.saturating_add(
            usage
                .input_tokens_details
                .as_ref()
                .map_or(0, |details| details.cache_write_tokens),
        );
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.reasoning_output_tokens = self.reasoning_output_tokens.saturating_add(
            usage
                .output_tokens_details
                .as_ref()
                .map_or(0, |details| details.reasoning_tokens),
        );
        self.total_tokens = self.total_tokens.saturating_add(usage.total_tokens);
    }
}

struct AgentSetup {
    agent: PreparedAgent,
    verifier: Option<Box<dyn AttemptVerifier>>,
    readiness_timing: PhaseTiming,
    timing: PhaseTiming,
}

enum PreparedAgent {
    Nanocodex {
        agent: Nanocodex,
        events: AgentEvents,
    },
    Harness(HarnessExec),
}

impl EvaluatorBuilder {
    /// Sets the parent under which this evaluator creates one UUID-named
    /// artifact directory.
    #[must_use]
    pub fn output_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.output_directory = directory.into();
        self
    }

    /// Records the execution environment used by the configured attempt.
    #[must_use]
    pub(crate) const fn attempt_environment(mut self, environment: EvalEnvironment) -> Self {
        self.attempt_environment = environment;
        self
    }

    /// Configures the fresh Nanocodex builder for each attempt.
    ///
    /// The factory runs after the disposable workspace is populated and before
    /// the agent is built. This is the boundary for attempt-owned resources
    /// such as a retained VM tool session and its guest-visible workspace.
    #[must_use]
    pub(crate) fn attempt_agent<F, E>(mut self, factory: F) -> Self
    where
        F: for<'a> Fn(EvalAttempt<'a>, NanocodexBuilder) -> Result<AttemptAgent, E>
            + Send
            + Sync
            + 'static,
        E: Error + Send + Sync + 'static,
    {
        self.attempt_agent = Some(Arc::new(move |attempt, builder| {
            factory(attempt, builder).map_err(|error| Box::new(error) as AttemptError)
        }));
        self
    }

    /// Builds a reusable evaluator.
    ///
    /// # Errors
    ///
    /// Returns an error when the output path is unavailable.
    pub fn build(self) -> Result<Evaluator, EvalError> {
        let job = EvalJob::create(&self.output_directory)?;
        Ok(Evaluator {
            inner: Arc::new(EvaluatorInner {
                nanocodex: self.nanocodex,
                job,
                attempt_environment: self.attempt_environment,
                attempt_agent: self.attempt_agent,
            }),
        })
    }
}

impl AttemptAgent {
    /// Uses `nanocodex` for one attempt with the default native verifier.
    #[must_use]
    pub(crate) fn new(nanocodex: NanocodexBuilder) -> Self {
        Self {
            driver: AttemptDriverSetup::Ready(AttemptDriver::Nanocodex(nanocodex)),
            readiness: None,
            verifier: None,
        }
    }

    pub(crate) fn preparing_nanocodex<F, E>(preparation: F) -> Self
    where
        F: Future<Output = Result<NanocodexBuilder, E>> + Send + 'static,
        E: Error + Send + Sync + 'static,
    {
        Self {
            driver: AttemptDriverSetup::Preparing(Box::pin(async move {
                preparation
                    .await
                    .map(AttemptDriver::Nanocodex)
                    .map_err(|error| Box::new(error) as AttemptError)
            })),
            readiness: None,
            verifier: None,
        }
    }

    /// Uses one pinned stock-harness CLI process for an evaluator attempt.
    ///
    /// This concrete adapter preserves the evaluator's workspace, timeout,
    /// verifier, cleanup, and retention lifecycle.
    #[doc(hidden)]
    #[must_use]
    pub(crate) fn harness(harness: HarnessExec) -> Self {
        Self {
            driver: AttemptDriverSetup::Ready(AttemptDriver::Harness(harness)),
            readiness: None,
            verifier: None,
        }
    }

    /// Installs asynchronous environment readiness work that must complete
    /// before the agent is built or any model request is sent.
    ///
    /// VM adapters use this to wait for a typed guest handshake. A readiness
    /// failure aborts the attempt as an environment error without spending a
    /// model request.
    #[must_use]
    pub(crate) fn ready<F, E>(mut self, readiness: F) -> Self
    where
        F: Future<Output = Result<(), E>> + Send + 'static,
        E: Error + Send + Sync + 'static,
    {
        self.readiness = Some(Box::pin(async move {
            readiness
                .await
                .map_err(|error| Box::new(error) as AttemptError)
        }));
        self
    }

    /// Installs the verifier that owns this attempt's environment backend.
    #[must_use]
    pub(crate) fn verifier(mut self, verifier: impl AttemptVerifier + 'static) -> Self {
        self.verifier = Some(Box::new(verifier));
        self
    }

    fn into_parts(
        self,
    ) -> (
        AttemptDriverSetup,
        Option<AttemptReadinessFuture>,
        Option<Box<dyn AttemptVerifier>>,
    ) {
        (self.driver, self.readiness, self.verifier)
    }
}

impl EvalAttempt<'_> {
    /// Returns the immutable task definition.
    #[must_use]
    pub(crate) const fn task(&self) -> &Task {
        self.task
    }

    /// Returns the retained attempt root.
    #[must_use]
    pub(crate) const fn directory(&self) -> &Path {
        self.directory
    }

    /// Returns the workspace path presented to the agent.
    #[must_use]
    pub(crate) const fn workspace(&self) -> &Path {
        self.workspace
    }

    /// Returns the exact final assistant message produced by the candidate.
    #[must_use]
    pub(crate) const fn final_message(&self) -> Option<&str> {
        self.final_message
    }
}

#[derive(Clone)]
struct RunEmitter {
    run_id: Uuid,
    invocation_id: Uuid,
    state: Arc<Mutex<RunEventState>>,
}

struct RunEventState {
    sequence: u64,
    sender: broadcast::Sender<Arc<EvalEvent>>,
    terminal: bool,
}

impl RunEmitter {
    fn new(run_id: Uuid) -> (Self, EvalEvents) {
        let invocation_id = Uuid::now_v7();
        let (sender, _) = broadcast::channel(EVENT_CAPACITY);
        let events = EvalEvents::new(&sender);
        (
            Self {
                run_id,
                invocation_id,
                state: Arc::new(Mutex::new(RunEventState {
                    sequence: 0,
                    sender,
                    terminal: false,
                })),
            },
            events,
        )
    }

    fn emit(&self, attempt: Option<EvalEventAttempt>, kind: EvalEventKind) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.terminal {
            return;
        }
        state.sequence = state.sequence.saturating_add(1);
        let event = Arc::new(EvalEvent {
            run_id: self.run_id,
            invocation_id: self.invocation_id,
            sequence: state.sequence,
            attempt,
            kind,
        });
        let _ = state.sender.send(event);
    }

    fn finish<T>(&self, result: &Result<T, EvalError>) {
        let kind = match result {
            Ok(_) => EvalEventKind::RunCompleted,
            Err(error) => EvalEventKind::RunFailed {
                error: error.to_string(),
            },
        };
        self.emit_terminal(kind);
    }

    fn cancel(&self) {
        self.emit_terminal(EvalEventKind::RunFailed {
            error: "evaluation invocation cancelled".to_owned(),
        });
    }

    fn emit_terminal(&self, kind: EvalEventKind) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.terminal {
            return;
        }
        state.terminal = true;
        state.sequence = state.sequence.saturating_add(1);
        let event = Arc::new(EvalEvent {
            run_id: self.run_id,
            invocation_id: self.invocation_id,
            sequence: state.sequence,
            attempt: None,
            kind,
        });
        let _ = state.sender.send(event);
    }
}

struct AttemptEmitter {
    run: RunEmitter,
    attempt_id: Uuid,
    session_id: SessionId,
    task_name: String,
    trial_name: String,
    sequence: u64,
}

impl AttemptEmitter {
    fn new(run: RunEmitter, session_id: SessionId, task: &Task, trial_name: &str) -> Self {
        Self {
            run,
            attempt_id: session_id.as_uuid(),
            session_id,
            task_name: task.name().to_owned(),
            trial_name: trial_name.to_owned(),
            sequence: 0,
        }
    }

    fn emit(&mut self, kind: EvalEventKind) {
        self.sequence += 1;
        self.run.emit(
            Some(EvalEventAttempt {
                id: self.attempt_id,
                task_name: self.task_name.clone(),
                trial_name: self.trial_name.clone(),
                sequence: self.sequence,
            }),
            kind,
        );
    }
}

#[derive(Deserialize)]
struct ResponsesApiErrorEnvelope {
    error: Option<ResponsesApiError>,
    response: Option<ResponsesApiErrorResponse>,
}

#[derive(Deserialize)]
struct ResponsesApiErrorResponse {
    error: Option<ResponsesApiError>,
}

#[derive(Deserialize)]
struct ResponsesApiError {
    code: Option<String>,
}

fn attempt_failure(
    eval: &Evaluator,
    attempt_id: Uuid,
    task: Task,
    trial_name: String,
    started_at: DateTime<Utc>,
    queue_wait: PhaseTiming,
    failure: &AttemptRunFailure,
) -> EvalFailure {
    let root = eval.directory().join(&trial_name);
    let model = failure
        .agent
        .as_ref()
        .map_or_else(|| MODEL.to_owned(), |agent| agent.model.clone());
    let effort = failure
        .agent
        .as_ref()
        .map_or_else(|| "unknown".to_owned(), |agent| agent.effort.clone());
    let exception = eval_exception(&failure.error, failure.occurred_at);
    EvalFailure {
        attempt_id,
        task_name: task.name().to_owned(),
        trial_name,
        exception,
        model,
        effort,
        environment: eval.attempt_environment(),
        started_at,
        finished_at: Utc::now(),
        timing: EvalFailureTiming {
            queue_wait,
            environment_setup: failure.environment_setup.clone(),
            environment_readiness: failure.environment_readiness.clone(),
            agent_setup: failure.agent_setup.clone(),
            agent_execution: failure.agent_execution.clone(),
            verifier: failure.verifier_timing.clone(),
        },
        agent: failure.agent.clone(),
        verifier: failure.verifier.clone(),
        cleanup: failure.cleanup.clone(),
        artifacts: EvalArtifacts {
            workspace: root.join("workspace"),
            verifier_output: root.join("verifier/test-stdout.txt"),
            directory: root,
        },
        task,
    }
}

fn eval_exception(error: &EvalError, occurred_at: DateTime<Utc>) -> EvalException {
    EvalException {
        kind: failure_kind(error),
        outcome: failure_outcome(error),
        message: error.to_string(),
        traceback: error_traceback(error),
        occurred_at,
    }
}

fn failure_outcome(error: &EvalError) -> EvalOutcome {
    match error {
        EvalError::Nanocodex(error) if is_safety_refusal(error) => EvalOutcome::SafetyRefusal,
        EvalError::Harness(error) if error.is_safety_refusal() => EvalOutcome::SafetyRefusal,
        EvalError::AgentTimeout(_) => EvalOutcome::AgentTimeout,
        _ => EvalOutcome::InfrastructureError,
    }
}

fn failure_kind(error: &EvalError) -> EvalExceptionKind {
    match error {
        EvalError::Nanocodex(error) if is_safety_refusal(error) => {
            EvalExceptionKind::AgentSafetyRefusal
        }
        EvalError::Harness(error) if error.is_safety_refusal() => {
            EvalExceptionKind::AgentSafetyRefusal
        }
        EvalError::Nanocodex(error)
            if error
                .responses_error()
                .is_some_and(|error| error.class() == "authorization") =>
        {
            EvalExceptionKind::AgentAuthentication
        }
        EvalError::AgentTimeout(_) => EvalExceptionKind::AgentTimeout,
        EvalError::VerifierTimeout(_) => EvalExceptionKind::VerifierTimeout,
        EvalError::AgentCleanup(_) => EvalExceptionKind::Cleanup,
        EvalError::Nanocodex(_)
        | EvalError::Harness(_)
        | EvalError::AgentEventsClosed
        | EvalError::AgentTerminal(_) => EvalExceptionKind::Agent,
        EvalError::AttemptVerifier(_)
        | EvalError::VerifierBootstrap(_)
        | EvalError::ParseReward(_) => EvalExceptionKind::Verifier,
        EvalError::UnsupportedNativeTask { .. }
        | EvalError::TaskPackage(_)
        | EvalError::OutputOverlapsTask { .. }
        | EvalError::AttemptAgent(_) => EvalExceptionKind::Environment,
        EvalError::Io(_) | EvalError::Json(_) => EvalExceptionKind::Internal,
    }
}

const fn verifier_workspace_usable_after_agent_error(error: &EvalError) -> bool {
    matches!(
        error,
        EvalError::Nanocodex(_)
            | EvalError::Harness(_)
            | EvalError::AgentTimeout(_)
            | EvalError::AgentEventsClosed
            | EvalError::AgentTerminal(_)
    )
}

fn reject_output_overlap(output: &Path, task: &Path) -> Result<(), EvalError> {
    if output.starts_with(task) || output_aliases_task_package(output, task)? {
        return Err(EvalError::OutputOverlapsTask {
            output: output.to_path_buf(),
            task: task.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn output_aliases_task_package(output: &Path, task: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    const PACKAGE_DIRECTORIES: [&str; 4] = ["environment", "tests", "solution", "steps"];

    // The output ancestry is application-owned and non-adversarial. Comparing
    // existing directory identities also catches accidental bind-mount aliases;
    // it is not intended to defend against a concurrent hostile path swap.
    let mut identities = std::collections::HashSet::<(u64, u64)>::new();
    let mut pending = vec![task.to_path_buf()];
    pending.extend(PACKAGE_DIRECTORIES.into_iter().map(|name| task.join(name)));
    while let Some(directory) = pending.pop() {
        let metadata = match fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "task package symlinks are unsupported while checking output ancestry: {}",
                    directory.display()
                ),
            ));
        }
        if !metadata.is_dir() || !identities.insert((metadata.dev(), metadata.ino())) {
            continue;
        }
        if directory != task {
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                let metadata = fs::symlink_metadata(entry.path())?;
                if metadata.file_type().is_symlink() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "task package symlinks are unsupported while checking output ancestry: {}",
                            entry.path().display()
                        ),
                    ));
                }
                if metadata.is_dir() {
                    pending.push(entry.path());
                }
            }
        }
    }

    for ancestor in output.ancestors() {
        match fs::metadata(ancestor) {
            Ok(metadata)
                if metadata.is_dir() && identities.contains(&(metadata.dev(), metadata.ino())) =>
            {
                return Ok(true);
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

#[cfg(not(unix))]
fn output_aliases_task_package(_output: &Path, _task: &Path) -> io::Result<bool> {
    Ok(false)
}

fn is_safety_refusal(error: &NanocodexError) -> bool {
    let Some(ResponsesError::Api { event }) = error.responses_error() else {
        return false;
    };
    serde_json::from_str::<ResponsesApiErrorEnvelope>(event)
        .ok()
        .and_then(|event| {
            event
                .error
                .or_else(|| event.response.and_then(|response| response.error))
                .and_then(|error| error.code)
        })
        .is_some_and(|code| code == "cyber_policy")
}

fn error_traceback(error: &dyn Error) -> String {
    let mut traceback = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        traceback.push_str("\nCaused by: ");
        traceback.push_str(&error.to_string());
        source = error.source();
    }
    traceback
}

fn attempt_span(eval: &Evaluator, task: &Task, attempt_id: Uuid, trial_name: &str) -> Span {
    info_span!(
        target: "nanocodex_eval",
        parent: None,
        "eval.attempt",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        eval.id = %eval.id(),
        eval.attempt.id = %attempt_id,
        eval.task.name = task.name(),
        eval.trial.name = trial_name,
        eval.task.image = task.image().reference(),
        eval.resource.cpus = task.resources().cpus,
        eval.resource.memory_mib = task.resources().memory_mb,
        eval.resource.storage_mib = task.resources().storage_mb,
        eval.resource.gpus = task.resources().gpus,
        eval.network = task.network().as_str(),
        eval.score.status = tracing::field::Empty,
        eval.reward.total = tracing::field::Empty,
        agent.model_calls = tracing::field::Empty,
        agent.tool_calls = tracing::field::Empty,
        agent.response_attempts = tracing::field::Empty,
        agent.response_retries = tracing::field::Empty,
        agent.runtime.completeness = tracing::field::Empty,
        gen_ai.usage.input_tokens = tracing::field::Empty,
        gen_ai.usage.cached_input_tokens = tracing::field::Empty,
        gen_ai.usage.cache_write_input_tokens = tracing::field::Empty,
        gen_ai.usage.output_tokens = tracing::field::Empty,
        gen_ai.usage.total_tokens = tracing::field::Empty,
        agent.warmup.duration_ns = tracing::field::Empty,
        agent.warmup.input_tokens = tracing::field::Empty,
        agent.warmup.cached_input_tokens = tracing::field::Empty,
        agent.warmup.cache_write_input_tokens = tracing::field::Empty,
        agent.warmup.output_tokens = tracing::field::Empty,
        agent.warmup.total_tokens = tracing::field::Empty,
        cost.usd = tracing::field::Empty,
        cost.status = tracing::field::Empty,
        eval.cleanup.failed = tracing::field::Empty,
        agent.cleanup.status = tracing::field::Empty,
        agent.cleanup.duration_ns = tracing::field::Empty,
        verifier.cleanup.status = tracing::field::Empty,
        verifier.cleanup.duration_ns = tracing::field::Empty,
        status = tracing::field::Empty,
        error.message = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
    )
}

fn record_attempt_result(
    span: &Span,
    started_at: Instant,
    result: &Result<EvalResult, AttemptRunFailure>,
) {
    let duration_ns = elapsed_ns(started_at);
    span.record("duration_ns", duration_ns);
    match result {
        Ok(result) => {
            record_attempt_success(span, result);
            span.in_scope(|| {
                info!(
                    target: "nanocodex_eval",
                    duration_ns,
                    score.status = eval_status(result.status),
                    "evaluation attempt completed"
                );
            });
        }
        Err(failure) => {
            record_cleanup(span, &failure.cleanup);
            if let Some(agent) = &failure.agent {
                record_agent_metrics(span, agent);
            }
            span.record("status", "failed");
            span.record("otel.status_code", "ERROR");
            span.record("error.message", tracing::field::display(&failure.error));
            span.in_scope(|| {
                info!(
                    target: "nanocodex_eval",
                    duration_ns,
                    error = %failure.error,
                    "evaluation attempt failed"
                );
            });
        }
    }
}

fn record_attempt_success(span: &Span, result: &EvalResult) {
    record_cleanup(span, &result.cleanup);
    if let Some(agent) = &result.agent {
        record_agent_metrics(span, agent);
    }
    span.record("status", "completed");
    span.record("eval.score.status", eval_status(result.status));
    span.record(
        "eval.reward.total",
        result.verifier.rewards.values().sum::<f64>(),
    );
    if let Some(exception) = &result.exception {
        span.record("otel.status_code", "ERROR");
        span.record("error.message", tracing::field::display(&exception.message));
    } else {
        span.record("otel.status_code", "OK");
    }
}

fn record_agent_metrics(span: &Span, agent: &AgentResult) {
    let usage = &agent.usage;
    let warmup = &agent.metadata.warmup_usage;
    span.record("agent.model_calls", agent.model_calls);
    span.record("agent.tool_calls", agent.tool_calls);
    span.record("agent.response_attempts", agent.metadata.response_attempts);
    span.record("agent.response_retries", agent.metadata.response_retries);
    span.record(
        "agent.runtime.completeness",
        measurement_completeness_label(agent.metadata.runtime_completeness),
    );
    span.record("cost.status", agent.metadata.cost_status.as_str());
    span.record("gen_ai.usage.input_tokens", usage.input_tokens);
    span.record(
        "gen_ai.usage.cached_input_tokens",
        usage.cached_input_tokens,
    );
    span.record(
        "gen_ai.usage.cache_write_input_tokens",
        usage.cache_write_input_tokens,
    );
    span.record("gen_ai.usage.output_tokens", usage.output_tokens);
    span.record("gen_ai.usage.total_tokens", usage.total_tokens);
    span.record("agent.warmup.input_tokens", warmup.input_tokens);
    span.record(
        "agent.warmup.cached_input_tokens",
        warmup.cached_input_tokens,
    );
    span.record(
        "agent.warmup.cache_write_input_tokens",
        warmup.cache_write_input_tokens,
    );
    span.record("agent.warmup.output_tokens", warmup.output_tokens);
    span.record("agent.warmup.total_tokens", warmup.total_tokens);
    span.record(
        "agent.warmup.duration_ns",
        agent.metadata.warmup_duration_ns,
    );
    if let Some(cost_usd) = agent.cost_usd {
        span.record("cost.usd", cost_usd);
    }
}

const fn measurement_completeness_label(
    completeness: crate::MeasurementCompleteness,
) -> &'static str {
    match completeness {
        crate::MeasurementCompleteness::Complete => "complete",
        crate::MeasurementCompleteness::ObservedLowerBound => "observed_lower_bound",
    }
}

fn record_cleanup(span: &Span, cleanup: &EvalCleanup) {
    span.record("eval.cleanup.failed", cleanup.is_failed());
    span.record("agent.cleanup.status", cleanup_status(&cleanup.agent));
    span.record(
        "agent.cleanup.duration_ns",
        cleanup.agent.timing.as_ref().map_or(0, phase_timing_ns),
    );
    span.record("verifier.cleanup.status", cleanup_status(&cleanup.verifier));
    span.record(
        "verifier.cleanup.duration_ns",
        cleanup.verifier.timing.as_ref().map_or(0, phase_timing_ns),
    );
}

const fn cleanup_status(cleanup: &CleanupPhase) -> &'static str {
    match cleanup.status {
        crate::CleanupStatus::NotRequired => "not_required",
        crate::CleanupStatus::Completed => "completed",
        crate::CleanupStatus::Failed => "failed",
    }
}

fn phase_timing_ns(timing: &PhaseTiming) -> u64 {
    u64::try_from(
        timing
            .finished_at
            .signed_duration_since(timing.started_at)
            .num_nanoseconds()
            .unwrap_or_default()
            .max(0),
    )
    .unwrap_or(u64::MAX)
}

const fn eval_status(status: EvalStatus) -> &'static str {
    match status {
        EvalStatus::Passed => "passed",
        EvalStatus::Failed => "failed",
    }
}

fn validate_attempt_environment(task: &Task, custom_backend: bool) -> Result<(), EvalError> {
    if task.requires_compose() && !custom_backend {
        return Err(EvalError::UnsupportedNativeTask {
            task: task.name().to_owned(),
            reason: "custom Docker Compose environments are not available in native mode",
        });
    }
    Ok(())
}

fn verifier_status(task: &Task, verifier: &crate::VerifierResult) -> EvalStatus {
    if task.verifier().scoring_policy().passes(&verifier.rewards) {
        EvalStatus::Passed
    } else {
        EvalStatus::Failed
    }
}

fn verifier_bootstrap_error(verifier: &VerifierExecution) -> Option<EvalError> {
    if verifier.result.rewards.values().all(|reward| *reward > 0.0)
        || verifier.stdout.contains("test session starts")
    {
        return None;
    }

    let evidence = format!("{}\n{}", verifier.stdout, verifier.stderr).to_ascii_lowercase();
    let signal = [
        "failed to download",
        "error sending request",
        "temporary failure in name resolution",
        "could not resolve host",
        "name or service not known",
        "network is unreachable",
        "connection timed out",
    ]
    .into_iter()
    .find(|signal| evidence.contains(signal))?;
    Some(EvalError::VerifierBootstrap(signal.to_owned()))
}

fn record_span_result<T, E>(span: &tracing::Span, started_at: Instant, result: &Result<T, E>)
where
    E: std::fmt::Display,
{
    let duration_ns = elapsed_ns(started_at);
    span.record("duration_ns", duration_ns);
    match result {
        Ok(_) => {
            span.record("status", "completed");
            span.record("otel.status_code", "OK");
            span.in_scope(|| {
                info!(
                    target: "nanocodex_eval",
                    duration_ns,
                    status = "completed",
                    "evaluation phase completed"
                );
            });
        }
        Err(error) => {
            span.record("status", "failed");
            span.record("otel.status_code", "ERROR");
            span.record("error.message", tracing::field::display(error));
            span.in_scope(|| {
                info!(
                    target: "nanocodex_eval",
                    duration_ns,
                    status = "failed",
                    error = %error,
                    "evaluation phase failed"
                );
            });
        }
    }
}

fn record_content(span: &tracing::Span, kind: &'static str, content: &str) {
    span.in_scope(|| {
        info!(
            target: "nanocodex_eval",
            content_kind = kind,
            content,
            "evaluation content"
        );
    });
}

fn elapsed_ns(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn trial_name(task: &Task, attempt_id: Uuid) -> String {
    let short_name = task.name().rsplit('/').next().unwrap_or(task.name());
    let compact_id = attempt_id.simple().to_string();
    format!("{short_name}__{compact_id}")
}

impl AgentResult {
    fn from_terminal(final_message: String, event: &AgentEvent) -> Result<Self, EvalError> {
        if !event.kind.is_terminal() {
            return Err(EvalError::AgentEventsClosed);
        }
        let metadata: AgentTerminalMetadata =
            serde_json::from_str(event.payload.get()).map_err(EvalError::AgentTerminal)?;
        let metadata = metadata.into_retained();
        Ok(Self {
            final_message,
            model: metadata.model.clone(),
            effort: metadata.effort.clone(),
            model_calls: metadata.model_calls,
            tool_calls: metadata.tool_calls,
            usage: metadata.usage.clone(),
            cost_usd: metadata.cost_usd,
            metadata,
        })
    }
}

#[derive(Deserialize)]
struct AgentTerminalMetadata {
    status: AgentStatus,
    model: String,
    effort: String,
    #[serde(default)]
    reasoning_mode: Option<String>,
    transport: String,
    orchestration: String,
    duration_ms: u64,
    duration_ns: u64,
    model_calls: u32,
    steers: u32,
    compactions: u32,
    tool_calls: u32,
    connection_attempts: u32,
    websocket_reconnects: u32,
    response_attempts: u32,
    response_retries: u32,
    connection_duration_ns: u64,
    retry_backoff_duration_ns: u64,
    model_duration_ns: u64,
    warmup_duration_ns: u64,
    tool_work_duration_ns: u64,
    tool_wall_duration_ns: u64,
    usage: UsageTotals,
    warmup_usage: UsageTotals,
    #[serde(default, rename = "last_response_id")]
    _last_response_id: Option<String>,
    cost_usd: Option<f64>,
    cost_status: String,
    #[serde(default)]
    estimated_cost: Option<nanocodex_oai_api::pricing::EstimatedUsdCost>,
}

impl AgentTerminalMetadata {
    fn into_retained(self) -> AgentMetadata {
        let runtime_completeness = if self.status == AgentStatus::Completed {
            crate::MeasurementCompleteness::Complete
        } else {
            crate::MeasurementCompleteness::ObservedLowerBound
        };
        AgentMetadata {
            status: self.status,
            model: self.model,
            effort: self.effort,
            reasoning_mode: self.reasoning_mode,
            transport: self.transport,
            orchestration: self.orchestration,
            runtime_completeness,
            duration_ms: self.duration_ms,
            duration_ns: self.duration_ns,
            model_calls: self.model_calls,
            steers: self.steers,
            compactions: self.compactions,
            tool_calls: self.tool_calls,
            connection_attempts: self.connection_attempts,
            websocket_reconnects: self.websocket_reconnects,
            response_attempts: self.response_attempts,
            response_retries: self.response_retries,
            connection_duration_ns: self.connection_duration_ns,
            retry_backoff_duration_ns: self.retry_backoff_duration_ns,
            model_duration_ns: self.model_duration_ns,
            warmup_duration_ns: self.warmup_duration_ns,
            tool_work_duration_ns: self.tool_work_duration_ns,
            tool_wall_duration_ns: self.tool_wall_duration_ns,
            usage: self.usage,
            warmup_usage: self.warmup_usage,
            cost_usd: self.cost_usd,
            cost_status: self.cost_status,
            estimated_cost: self.estimated_cost,
        }
    }
}
