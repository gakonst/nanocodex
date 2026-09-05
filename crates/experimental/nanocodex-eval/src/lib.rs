//! Typed evaluation contracts and durable worksets for Nanocodex agents.
//!
//! This crate owns task loading, durable profile worksets, typed events and
//! outcomes, plus one canonical native execution contract. Applications choose
//! one exact profile family; SQLite atomically claims one pre-materialized task
//! row and fences its terminal outcome.
//!
//! # Open a durable profile
//!
//! Work must first be pre-materialized with [`Evaluation::add`] or
//! [`Evaluation::add_profile`]. Opening a benchmark never creates rows.
//!
//! ```no_run
//! use nanocodex_eval::{Evaluation, EvaluationClaim};
//!
//! # async fn evaluate() -> Result<(), Box<dyn std::error::Error>> {
//! let evaluation = Evaluation::open(
//!     "nanocodex.toml",
//!     Some("local-smoke"),
//!     ".nanocodex/evals",
//! )?;
//! match evaluation.claim_next()? {
//!     EvaluationClaim::Run(claim) => {
//!         // Execute exactly this profile treatment and retain its evidence.
//!         let evidence = claim.output_directory().to_path_buf();
//!         claim.succeed(&evidence)?;
//!     }
//!     EvaluationClaim::Busy(_) | EvaluationClaim::Complete => {}
//! }
//! # Ok(())
//! # }
//! ```
//!
//! A running row is held by the worker process itself. The claim owner may
//! explicitly requeue infrastructure failures; dropping the claim or losing
//! the worker process records a terminal failure.

#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
// Retained-data readers remain portable; VM execution internals become
// intentionally unreachable when this target cannot run the VM backend.
#![cfg_attr(
    not(any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )),
    allow(dead_code, unused_imports)
)]

mod api;
/// Agent Trajectory Interchange Format projection and wire types.
pub mod atif;
mod capture_proxy;
mod cluster;
pub mod coordinator;
mod digest;
mod evaluation;
mod evaluator;
mod event;
mod execution;
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
/// Configured external harness execution inside evaluator-owned sandboxes.
pub mod harness;
mod harness_exec;
/// Content-addressed normalization of third-party datasets into native tasks.
pub mod import;
mod job;
/// Evaluator-owned model judge endpoint for isolated verifier processes.
pub mod judge;
mod native;
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod profile;
mod result;
mod task;
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
pub mod vm;
mod workset;

pub(crate) use atif::{
    AtifAgent, AtifAgentExtra, AtifObservation, AtifObservationExtra, AtifObservationResult,
    AtifSource, AtifStep, AtifToolCall, AtifToolCallExtra, AtifTrajectory,
};
pub(crate) use capture_proxy::{ResponsesCaptureProxy, ResponsesCaptureProxyConfig};
pub use evaluation::{
    CoordinateClaim, Evaluation, EvaluationBusy, EvaluationClaim, EvaluationCounts,
    EvaluationError, EvaluationFamilyStatus, EvaluationObserver, EvaluationSelector,
    EvaluationStatus, EvaluationTreatment, EvaluationWork,
};
pub use evaluator::{EvalError, EvalRun, Evaluator, EvaluatorBuilder};
pub use event::{
    EvalEvent, EvalEventAttempt, EvalEventKind, EvalEventStream, EvalEventStreamError, EvalEvents,
};
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
pub use execution::{CanonicalTaskRunner, validate_prepared_eval_host};
pub use execution::{ClaimedEvaluationTask, EvaluationExecution, EvaluationExecutionError};
pub(crate) use harness_exec::{
    HarnessCommandOutput, HarnessCommandRunner, HarnessCommandRunnerError, HarnessCommandStatus,
    HarnessExec, HarnessExecError,
};
pub use nanocodex_oai_api::{PromptMessage, PromptMessageRole};
pub use profile::{ResolvedHarness, ResolvedTask};
pub use result::{
    AgentMetadata, AgentResult, AgentStatus, CleanupDiagnostic, CleanupPhase, CleanupStatus,
    EvalArtifacts, EvalAttemptOutcome, EvalCleanup, EvalEnvironment, EvalException,
    EvalExceptionKind, EvalFailure, EvalFailureTiming, EvalOutcome, EvalResult, EvalStatus,
    EvalTiming, MeasurementCompleteness, PhaseTiming, UsageTotals, VerifierResult,
};
pub use task::{
    NetworkPolicy, OciImage, Resources, ScoringPolicy, Task, TaskArtifact, TaskLoadError,
    TaskOutput, Verifier, VerifierCollect, VerifierEnvironmentMode,
};
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
pub use vm::{CachePolicy, VmResources, VmResourcesBuilder, VmResourcesError};
pub use workset::{RecentAttemptCounts, RecentAttemptFailure};
