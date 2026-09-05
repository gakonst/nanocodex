use std::{collections::BTreeMap, error::Error, path::PathBuf};

use chrono::{DateTime, Utc};
use nanocodex_oai_api::pricing::EstimatedUsdCost;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Task;

/// Execution environment used for one evaluation attempt.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalEnvironment {
    /// Host execution retained for focused tests and published-record decoding.
    /// Public evaluator construction does not select this environment.
    Native,
    /// Agent tools and verification run in a retained libkrun microVM.
    MicroVm,
}

/// Terminal score classification for one attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalStatus {
    /// Every verifier reward was positive.
    Passed,
    /// At least one verifier reward was zero or negative.
    Failed,
}

/// Stable primary lifecycle outcome for retry policy.
///
/// [`Self::Passed`] and [`Self::VerifierFailed`] describe attempts without a
/// lifecycle exception. A scored attempt can instead retain
/// [`Self::AgentTimeout`] while its independent [`EvalStatus`] records the
/// verifier result.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalOutcome {
    /// Verification completed with every reward positive.
    Passed,
    /// Verification completed with at least one non-positive reward.
    VerifierFailed,
    /// The model provider rejected the attempt for a safety policy.
    SafetyRefusal,
    /// Agent execution exceeded its task deadline.
    AgentTimeout,
    /// The attempt did not produce trustworthy verifier evidence.
    InfrastructureError,
}

impl EvalOutcome {
    /// Returns whether this outcome alone denotes a scored attempt.
    ///
    /// Use [`EvalAttemptOutcome::scored`] when inspecting a complete attempt:
    /// a lifecycle exception can coexist with verifier evidence.
    #[must_use]
    pub const fn is_scored(self) -> bool {
        matches!(self, Self::Passed | Self::VerifierFailed)
    }
}

/// Stable classification for a lifecycle exception.
///
/// An exception is independent from scoring: a healthy verifier may still
/// produce a score after an agent refusal, timeout, or execution error.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalExceptionKind {
    /// The model provider rejected the attempt for a safety policy.
    AgentSafetyRefusal,
    /// Agent authentication failed.
    AgentAuthentication,
    /// Agent execution exceeded the task deadline.
    AgentTimeout,
    /// Verifier execution exceeded its deadline.
    VerifierTimeout,
    /// Agent setup or execution failed.
    Agent,
    /// Verifier setup or execution failed.
    Verifier,
    /// Explicit agent or verifier resource cleanup failed.
    Cleanup,
    /// Attempt workspace or environment setup failed.
    Environment,
    /// The evaluation runtime violated an internal invariant.
    Internal,
}

/// One typed lifecycle exception retained independently from verifier score.
#[derive(Clone, Debug, Serialize)]
pub struct EvalException {
    /// Stable failure classification.
    pub kind: EvalExceptionKind,
    /// Stable semantic outcome used by retry policy.
    pub outcome: EvalOutcome,
    /// Human-readable error message.
    pub message: String,
    /// Complete formatted error chain.
    pub traceback: String,
    /// Time at which the primary exception was observed.
    pub occurred_at: DateTime<Utc>,
}

/// Terminal health for one explicit cleanup boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupStatus {
    /// This attempt did not own a cleanup boundary for the phase.
    NotRequired,
    /// Cleanup completed and all owned resources were joined.
    Completed,
    /// Cleanup failed after it was attempted.
    Failed,
}

/// Complete error information for a failed cleanup boundary.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CleanupDiagnostic {
    /// Human-readable cleanup error.
    pub message: String,
    /// Complete formatted cleanup error chain.
    pub traceback: String,
}

/// Health and disjoint timing for one cleanup boundary.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CleanupPhase {
    /// Terminal cleanup health.
    pub status: CleanupStatus,
    /// Exact cleanup interval when cleanup was attempted.
    pub timing: Option<PhaseTiming>,
    /// Complete error information when cleanup failed.
    pub diagnostic: Option<CleanupDiagnostic>,
}

impl CleanupPhase {
    /// Constructs a phase for which no cleanup boundary exists.
    #[must_use]
    pub const fn not_required() -> Self {
        Self {
            status: CleanupStatus::NotRequired,
            timing: None,
            diagnostic: None,
        }
    }

    /// Records cleanup that completed after `started_at`.
    #[must_use]
    pub fn completed(started_at: DateTime<Utc>) -> Self {
        Self {
            status: CleanupStatus::Completed,
            timing: Some(PhaseTiming::finished(started_at)),
            diagnostic: None,
        }
    }

    /// Records failed cleanup and its complete error chain.
    #[must_use]
    pub fn failed(started_at: DateTime<Utc>, error: &(dyn Error + 'static)) -> Self {
        Self {
            status: CleanupStatus::Failed,
            timing: Some(PhaseTiming::finished(started_at)),
            diagnostic: Some(CleanupDiagnostic {
                message: error.to_string(),
                traceback: format_error_chain(error),
            }),
        }
    }

    /// Returns whether cleanup was attempted and failed.
    #[must_use]
    pub const fn is_failed(&self) -> bool {
        matches!(self.status, CleanupStatus::Failed)
    }
}

impl Default for CleanupPhase {
    fn default() -> Self {
        Self::not_required()
    }
}

/// Cleanup health kept orthogonal to the attempt's score outcome.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct EvalCleanup {
    /// Agent driver and all model/tool resources.
    pub agent: CleanupPhase,
    /// Attempt-owned verifier environment.
    pub verifier: CleanupPhase,
}

impl EvalCleanup {
    /// Returns whether either cleanup boundary failed.
    #[must_use]
    pub const fn is_failed(&self) -> bool {
        self.agent.is_failed() || self.verifier.is_failed()
    }
}

/// Typed terminal output for an errored or refused attempt.
#[derive(Clone, Debug, Serialize)]
pub struct EvalFailure {
    /// `UUIDv7` identity shared with the attempt's agent session.
    pub attempt_id: Uuid,
    /// Stable task name from the task manifest.
    pub task_name: String,
    /// Filesystem-safe unique trial name.
    pub trial_name: String,
    /// Primary lifecycle exception. Flattening preserves the retained JSON
    /// shape used before score and exception became independent axes.
    #[serde(flatten)]
    pub exception: EvalException,
    /// Model selected for the failed attempt.
    pub model: String,
    /// Reasoning effort selected for the failed attempt.
    pub effort: String,
    /// Execution environment selected for the failed attempt.
    pub environment: EvalEnvironment,
    /// Time at which the attempt began.
    pub started_at: DateTime<Utc>,
    /// Time at which all retained phases and cleanup became terminal.
    pub finished_at: DateTime<Utc>,
    /// Completed phase intervals retained before the failure.
    pub timing: EvalFailureTiming,
    /// Partial terminal agent metrics when the agent lifecycle emitted them.
    pub agent: Option<AgentResult>,
    /// Verifier evidence observed before an unscored infrastructure failure.
    pub verifier: Option<VerifierResult>,
    /// Explicit cleanup health independent from score classification.
    pub cleanup: EvalCleanup,
    /// Retained attempt artifact paths.
    pub artifacts: EvalArtifacts,
    #[serde(skip)]
    pub(crate) task: Task,
}

/// Complete terminal output for one accepted evaluation attempt.
///
/// Scored verifier results and unscored lifecycle failures are returned through
/// the same independently awaitable value. Consumers never need the optional
/// event stream to retain partial usage, cleanup health, or failure evidence.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "score_status", content = "attempt", rename_all = "snake_case")]
pub enum EvalAttemptOutcome {
    /// The verifier produced a score.
    Scored(EvalResult),
    /// The attempt ended without trustworthy verifier evidence.
    Unscored(EvalFailure),
}

/// Typed scored result contained by [`EvalAttemptOutcome::Scored`].
#[derive(Clone, Debug, Serialize)]
pub struct EvalResult {
    /// `UUIDv7` identity shared with the attempt's agent session.
    pub attempt_id: Uuid,
    /// Stable task name from the task manifest.
    pub task_name: String,
    /// Filesystem-safe unique trial name.
    pub trial_name: String,
    /// Verifier-derived pass/fail classification.
    pub status: EvalStatus,
    /// Stable primary lifecycle outcome used by retry policy. Verifier-derived
    /// score classification remains available independently in [`Self::status`].
    pub outcome: EvalOutcome,
    /// Execution environment used by this attempt.
    pub environment: EvalEnvironment,
    /// Typed terminal agent output and usage, when cancellation or failure
    /// still produced a retained terminal snapshot.
    pub agent: Option<AgentResult>,
    /// Verifier exit code and component rewards.
    pub verifier: VerifierResult,
    /// Agent lifecycle exception retained independently from verifier score.
    pub exception: Option<EvalException>,
    /// Attempt phase timestamps.
    pub timing: EvalTiming,
    /// Explicit cleanup health independent from the verifier score.
    pub cleanup: EvalCleanup,
    /// Retained attempt artifact paths.
    pub artifacts: EvalArtifacts,
    #[serde(skip)]
    pub(crate) task: Task,
}

impl EvalAttemptOutcome {
    /// Returns the stable semantic outcome.
    #[must_use]
    pub const fn outcome(&self) -> EvalOutcome {
        match self {
            Self::Scored(result) => result.outcome,
            Self::Unscored(failure) => failure.exception.outcome,
        }
    }

    /// Returns the primary lifecycle exception, when one was observed.
    #[must_use]
    pub const fn exception(&self) -> Option<&EvalException> {
        match self {
            Self::Scored(result) => result.exception.as_ref(),
            Self::Unscored(failure) => Some(&failure.exception),
        }
    }

    /// Returns the stable attempt identity.
    #[must_use]
    pub const fn attempt_id(&self) -> Uuid {
        match self {
            Self::Scored(result) => result.attempt_id,
            Self::Unscored(failure) => failure.attempt_id,
        }
    }

    /// Returns the task name.
    #[must_use]
    pub fn task_name(&self) -> &str {
        match self {
            Self::Scored(result) => &result.task_name,
            Self::Unscored(failure) => &failure.task_name,
        }
    }

    /// Returns the unique trial name.
    #[must_use]
    pub fn trial_name(&self) -> &str {
        match self {
            Self::Scored(result) => &result.trial_name,
            Self::Unscored(failure) => &failure.trial_name,
        }
    }

    /// Returns the scored verifier result, when one exists.
    #[must_use]
    pub const fn scored(&self) -> Option<&EvalResult> {
        match self {
            Self::Scored(result) => Some(result),
            Self::Unscored(_) => None,
        }
    }

    /// Returns the unscored failure, when one exists.
    #[must_use]
    pub const fn unscored(&self) -> Option<&EvalFailure> {
        match self {
            Self::Scored(_) => None,
            Self::Unscored(failure) => Some(failure),
        }
    }

    /// Returns the immutable task definition used by this attempt.
    #[must_use]
    pub const fn task(&self) -> &Task {
        match self {
            Self::Scored(result) => result.task(),
            Self::Unscored(failure) => failure.task(),
        }
    }

    /// Returns the terminal agent output, when the attempt produced one.
    #[must_use]
    pub const fn agent(&self) -> Option<&AgentResult> {
        match self {
            Self::Scored(result) => result.agent.as_ref(),
            Self::Unscored(failure) => failure.agent.as_ref(),
        }
    }

    /// Returns the artifacts retained for this attempt.
    #[must_use]
    pub const fn artifacts(&self) -> &EvalArtifacts {
        match self {
            Self::Scored(result) => &result.artifacts,
            Self::Unscored(failure) => &failure.artifacts,
        }
    }
}

impl EvalResult {
    /// The immutable task definition used by this attempt.
    #[must_use]
    pub const fn task(&self) -> &Task {
        &self.task
    }
}

impl EvalFailure {
    /// The immutable task definition used by this attempt.
    #[must_use]
    pub const fn task(&self) -> &Task {
        &self.task
    }

    /// Returns the stable lifecycle exception classification.
    #[must_use]
    pub const fn kind(&self) -> EvalExceptionKind {
        self.exception.kind
    }

    /// Returns the stable semantic outcome used by retry policy.
    #[must_use]
    pub const fn outcome(&self) -> EvalOutcome {
        self.exception.outcome
    }

    /// Returns the human-readable lifecycle error.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.exception.message
    }

    /// Returns the complete formatted lifecycle error chain.
    #[must_use]
    pub fn traceback(&self) -> &str {
        &self.exception.traceback
    }

    /// Returns when the primary lifecycle exception was observed.
    #[must_use]
    pub const fn occurred_at(&self) -> DateTime<Utc> {
        self.exception.occurred_at
    }
}

/// Terminal agent output and aggregate runtime metadata.
#[derive(Clone, Debug, Serialize)]
pub struct AgentResult {
    /// Final assistant message.
    pub final_message: String,
    /// Model that produced the terminal result.
    pub model: String,
    /// Reasoning effort used by the agent.
    pub effort: String,
    /// Logical model-call count.
    pub model_calls: u32,
    /// Tool-call count.
    pub tool_calls: u32,
    /// Aggregate provider usage, excluding warmup.
    pub usage: UsageTotals,
    /// Estimated aggregate USD cost when provider usage can be priced.
    pub cost_usd: Option<f64>,
    /// Complete typed terminal event metadata.
    pub metadata: AgentMetadata,
}

/// Typed metadata emitted by Nanocodex's terminal event.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentMetadata {
    /// Agent lifecycle terminal status.
    pub status: AgentStatus,
    /// Selected model.
    pub model: String,
    /// Selected reasoning effort.
    pub effort: String,
    /// Responses API reasoning-mode spelling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_mode: Option<String>,
    /// Responses transport spelling.
    pub transport: String,
    /// Agent orchestration spelling.
    pub orchestration: String,
    /// Whether runtime counters and durations are exact or observed lower
    /// bounds reconstructed across cancellation or failure.
    pub runtime_completeness: MeasurementCompleteness,
    /// Rounded millisecond duration for display-oriented consumers.
    pub duration_ms: u64,
    /// Exact measured duration in nanoseconds.
    pub duration_ns: u64,
    /// Logical model calls.
    pub model_calls: u32,
    /// Steering messages accepted during the attempt.
    pub steers: u32,
    /// Context compactions completed during the attempt.
    pub compactions: u32,
    /// Tool calls executed during the attempt.
    pub tool_calls: u32,
    /// Responses connection attempts.
    pub connection_attempts: u32,
    /// Successful WebSocket replacements.
    pub websocket_reconnects: u32,
    /// Complete Responses transport attempts.
    pub response_attempts: u32,
    /// Retried Responses attempts.
    pub response_retries: u32,
    /// Time spent connecting to the Responses API.
    pub connection_duration_ns: u64,
    /// Time spent in owned retry backoff.
    pub retry_backoff_duration_ns: u64,
    /// Time spent waiting on model calls.
    pub model_duration_ns: u64,
    /// Time spent priming the prompt cache.
    pub warmup_duration_ns: u64,
    /// Sum of actual tool execution time.
    pub tool_work_duration_ns: u64,
    /// Tool wall time including overlap and scheduling.
    pub tool_wall_duration_ns: u64,
    /// Aggregate provider usage for task execution.
    pub usage: UsageTotals,
    /// Provider usage consumed by cache warmup.
    pub warmup_usage: UsageTotals,
    /// Estimated USD cost from provider usage and the built-in pricing catalog.
    pub cost_usd: Option<f64>,
    /// Stable explanation of whether cost is available.
    pub cost_status: String,
    /// Exact aggregate estimate and input/cache/output composition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<EstimatedUsdCost>,
}

/// Completeness of a retained numeric measurement.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementCompleteness {
    /// The producer observed the complete measurement interval.
    #[default]
    Complete,
    /// The producer retained observed work, but cancellation or failure may
    /// have omitted additional work.
    ObservedLowerBound,
}

/// Terminal state reported by the agent lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// The agent completed normally.
    Completed,
    /// The agent failed.
    Failed,
    /// The attempt was cancelled.
    Cancelled,
}

/// Aggregate provider token usage.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct UsageTotals {
    /// Total input tokens.
    pub input_tokens: u64,
    /// Input tokens served from provider cache.
    pub cached_input_tokens: u64,
    /// Provider-reported cache-write input tokens.
    pub cache_write_input_tokens: u64,
    /// Total output tokens.
    pub output_tokens: u64,
    /// Provider-reported reasoning output tokens.
    pub reasoning_output_tokens: u64,
    /// Total input plus output tokens.
    pub total_tokens: u64,
}

/// Terminal output from the task verifier.
#[derive(Clone, Debug, Serialize)]
pub struct VerifierResult {
    /// Process exit code.
    pub exit_code: i32,
    /// Named verifier rewards in deterministic key order.
    pub rewards: BTreeMap<String, f64>,
}

/// Wall-clock boundaries for each attempt phase.
#[derive(Clone, Debug, Serialize)]
pub struct EvalTiming {
    /// Time at which attempt setup began.
    pub started_at: DateTime<Utc>,
    /// Time at which the terminal result became durable.
    pub finished_at: DateTime<Utc>,
    /// Interval between invocation and attempt setup.
    pub queue_wait: PhaseTiming,
    /// Disposable environment preparation interval.
    pub environment_setup: PhaseTiming,
    /// Attempt backend readiness interval, including VM boot and guest handshake.
    pub environment_readiness: PhaseTiming,
    /// Agent construction interval.
    pub agent_setup: PhaseTiming,
    /// Agent execution interval.
    pub agent_execution: PhaseTiming,
    /// Verifier execution interval.
    pub verifier: PhaseTiming,
}

/// Completed attempt phases retained for an unscored terminal failure.
#[derive(Clone, Debug, Serialize)]
pub struct EvalFailureTiming {
    /// Time between invocation and attempt setup.
    pub queue_wait: PhaseTiming,
    /// Disposable environment preparation interval, when it completed.
    pub environment_setup: Option<PhaseTiming>,
    /// Backend readiness interval, when it completed.
    pub environment_readiness: Option<PhaseTiming>,
    /// Agent construction interval, when it completed.
    pub agent_setup: Option<PhaseTiming>,
    /// Agent execution interval, ending before agent cleanup.
    pub agent_execution: Option<PhaseTiming>,
    /// Verifier execution interval, ending before verifier cleanup.
    pub verifier: Option<PhaseTiming>,
}

/// UTC start and finish timestamps for one attempt phase.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PhaseTiming {
    /// Phase start.
    pub started_at: DateTime<Utc>,
    /// Phase finish.
    pub finished_at: DateTime<Utc>,
}

impl PhaseTiming {
    /// Records a phase that began at `started_at` and finished now.
    #[must_use]
    pub fn finished(started_at: DateTime<Utc>) -> Self {
        Self {
            started_at,
            finished_at: Utc::now(),
        }
    }
}

/// Durable paths retained for an attempt.
#[derive(Clone, Debug, Serialize)]
pub struct EvalArtifacts {
    /// Attempt root containing all retained files.
    pub directory: PathBuf,
    /// Disposable workspace presented to the agent.
    pub workspace: PathBuf,
    /// Captured verifier output file.
    pub verifier_output: PathBuf,
}

fn format_error_chain(error: &(dyn Error + 'static)) -> String {
    let mut traceback = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        traceback.push_str("\ncaused by: ");
        traceback.push_str(&error.to_string());
        source = error.source();
    }
    traceback
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    #[test]
    fn terminal_metadata_requires_runtime_completeness() {
        let encoded = terminal_metadata("completed");
        let error = serde_json::from_value::<super::AgentMetadata>(encoded).unwrap_err();
        assert!(error.to_string().contains("runtime_completeness"));

        let mut encoded = terminal_metadata("failed");
        encoded["runtime_completeness"] = json!("observed_lower_bound");
        let metadata: super::AgentMetadata = serde_json::from_value(encoded).unwrap();
        assert_eq!(
            metadata.runtime_completeness,
            super::MeasurementCompleteness::ObservedLowerBound
        );
    }

    fn terminal_metadata(status: &str) -> Value {
        json!({
            "status": status,
            "model": "gpt-5.6-sol",
            "effort": "medium",
            "transport": "responses_websocket_v2",
            "orchestration": "agent",
            "duration_ms": 1,
            "duration_ns": 1_000_000,
            "model_calls": 1,
            "steers": 0,
            "compactions": 0,
            "tool_calls": 0,
            "connection_attempts": 1,
            "websocket_reconnects": 0,
            "response_attempts": 1,
            "response_retries": 0,
            "connection_duration_ns": 1,
            "retry_backoff_duration_ns": 0,
            "model_duration_ns": 1,
            "warmup_duration_ns": 0,
            "tool_work_duration_ns": 0,
            "tool_wall_duration_ns": 0,
            "usage": {
                "input_tokens": 1,
                "cached_input_tokens": 0,
                "cache_write_input_tokens": 0,
                "output_tokens": 1,
                "reasoning_output_tokens": 0,
                "total_tokens": 2,
            },
            "warmup_usage": {
                "input_tokens": 0,
                "cached_input_tokens": 0,
                "cache_write_input_tokens": 0,
                "output_tokens": 0,
                "reasoning_output_tokens": 0,
                "total_tokens": 0,
            },
            "cost_usd": null,
            "cost_status": "usage_not_reported",
        })
    }
}
