//! Canonical native execution contract for one durable evaluation claim.

use std::{
    error::Error,
    fmt::{self, Display, Formatter},
    path::{Path, PathBuf},
};

use tokio::io::AsyncWriteExt as _;

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
use nanocodex_agent::NanocodexBuilder;
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
use nanocodex_vm::tools::GuestRuntimeDisk;
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
use std::{fs, time::Instant};

use crate::{
    CoordinateClaim, EvalAttemptOutcome, EvalEventKind, EvalEventStream, EvalOutcome,
    EvaluationTreatment, Evaluator, ResolvedHarness, Task, atif::AtifBuilder,
};
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
use crate::{
    harness::{CAPTURE_ONLY_GUEST_RUNTIME, Harness, HarnessAuth},
    judge::JudgeRuntime,
    vm::{CachePolicy, VmBackend, VmResources},
};

#[cfg(target_arch = "aarch64")]
const VM_GUEST_TARGET: &str = "aarch64-unknown-linux-musl";
#[cfg(target_arch = "x86_64")]
const VM_GUEST_TARGET: &str = "x86_64-unknown-linux-musl";
#[cfg(target_arch = "aarch64")]
const VM_GUEST_ELF_MACHINE: u16 = 183;
#[cfg(target_arch = "x86_64")]
const VM_GUEST_ELF_MACHINE: u16 = 62;

type BoxError = Box<dyn Error + Send + Sync + 'static>;

/// One owned snapshot of a durable claim passed to the canonical executor.
///
/// The SQLite claim remains with the caller so only the durable scheduler can
/// accept the executor's result. Source adapters are no longer involved after
/// they normalize their inputs into this canonical task.
#[derive(Clone, Debug)]
pub struct ClaimedEvaluationTask {
    task: Task,
    task_selector: String,
    treatment: EvaluationTreatment,
    harness: Option<ResolvedHarness>,
    harnesses: Vec<ResolvedHarness>,
    output_directory: PathBuf,
}

/// One canonical execution result awaiting a fenced SQLite transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvaluationExecution {
    /// The verifier accepted the result and retained a complete record.
    Passed {
        /// Complete canonical attempt record.
        evidence: PathBuf,
    },
    /// The verifier rejected the result and retained a complete record.
    Failed {
        /// Complete canonical attempt record.
        evidence: PathBuf,
        /// Verifier rejection diagnostic.
        failure: String,
    },
    /// Infrastructure prevented a trustworthy verifier result.
    Retry {
        /// Partial canonical record, when one was produced.
        evidence: Option<PathBuf>,
        /// Infrastructure diagnostic.
        failure: String,
    },
}

/// Failure to invoke the canonical evaluation executor.
#[derive(Debug)]
pub struct EvaluationExecutionError {
    source: BoxError,
}

#[derive(Debug)]
struct ContextualExecutionError {
    context: String,
    source: BoxError,
}

#[derive(Debug)]
struct ExecutionInvariantError {
    message: String,
}

/// The one native runner over Nanocodex's canonical evaluator and libkrun VM.
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[derive(Clone)]
pub struct CanonicalTaskRunner {
    nanocodex: NanocodexBuilder,
    auth: HarnessAuth,
}

#[derive(Clone)]
struct EvaluatorRunner {
    evaluator: Evaluator,
}

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
struct PreparedVmHost {
    vmm: PathBuf,
    runtime: PathBuf,
    runtime_image: PathBuf,
    cache: PathBuf,
}

struct NativeEventRecording {
    atif: AtifBuilder,
    atif_error: Option<EvaluationExecutionError>,
}

trait ExecutionResultExt<T> {
    fn execution_context(self, context: impl Into<String>) -> Result<T, EvaluationExecutionError>;
}

impl ClaimedEvaluationTask {
    /// Snapshots the immutable execution inputs from a live SQLite claim.
    #[must_use]
    pub fn from_claim(claim: &CoordinateClaim) -> Self {
        Self::new(
            claim.task().clone(),
            claim.task_selector(),
            claim.treatment().clone(),
            claim.harness().cloned(),
            claim.harnesses().to_vec(),
            claim.output_directory(),
        )
    }

    /// Creates a task snapshot for a claim owned by a remote coordinator.
    #[must_use]
    pub fn new(
        task: Task,
        task_selector: impl Into<String>,
        treatment: EvaluationTreatment,
        harness: Option<ResolvedHarness>,
        harnesses: Vec<ResolvedHarness>,
        output_directory: impl Into<PathBuf>,
    ) -> Self {
        Self {
            task,
            task_selector: task_selector.into(),
            treatment,
            harness,
            harnesses,
            output_directory: output_directory.into(),
        }
    }

    fn into_parts(
        self,
    ) -> (
        Task,
        String,
        EvaluationTreatment,
        Option<ResolvedHarness>,
        Vec<ResolvedHarness>,
        PathBuf,
    ) {
        (
            self.task,
            self.task_selector,
            self.treatment,
            self.harness,
            self.harnesses,
            self.output_directory,
        )
    }
}

impl EvaluationExecutionError {
    fn context(context: impl Into<String>, error: impl Error + Send + Sync + 'static) -> Self {
        Self {
            source: Box::new(ContextualExecutionError {
                context: context.into(),
                source: Box::new(error),
            }),
        }
    }

    fn invariant(message: impl Into<String>) -> Self {
        Self {
            source: Box::new(ExecutionInvariantError {
                message: message.into(),
            }),
        }
    }
}

impl<T, E> ExecutionResultExt<T> for Result<T, E>
where
    E: Error + Send + Sync + 'static,
{
    fn execution_context(self, context: impl Into<String>) -> Result<T, EvaluationExecutionError> {
        self.map_err(|error| EvaluationExecutionError::context(context, error))
    }
}

impl EvaluatorRunner {
    const fn new(evaluator: Evaluator) -> Self {
        Self { evaluator }
    }

    async fn run_outcome(
        &self,
        task: ClaimedEvaluationTask,
    ) -> Result<EvalAttemptOutcome, EvaluationExecutionError> {
        let (task, _, _, _, _, _) = task.into_parts();
        let event_log = self.evaluator.directory().join("events.jsonl");
        let run = self.evaluator.task(task);
        let stream = run.events().subscribe();
        let recorder = tokio::spawn(async move { record_events(stream, &event_log).await });
        let outcome = run.await;
        let recording = recorder
            .await
            .execution_context("native event recorder task failed")??;
        let outcome = outcome.execution_context("canonical evaluator failed")?;
        retain_trajectory(&outcome, recording).await?;
        Ok(outcome)
    }

    fn classify_outcome(&self, outcome: &EvalAttemptOutcome) -> EvaluationExecution {
        let evidence = self.evaluator.directory().to_path_buf();
        match outcome.outcome() {
            EvalOutcome::Passed => EvaluationExecution::Passed { evidence },
            EvalOutcome::VerifierFailed | EvalOutcome::SafetyRefusal => {
                EvaluationExecution::Failed {
                    evidence,
                    failure: "verifier returned a failing score".to_owned(),
                }
            }
            EvalOutcome::AgentTimeout | EvalOutcome::InfrastructureError => {
                EvaluationExecution::Retry {
                    evidence: Some(evidence),
                    failure: outcome.exception().map_or_else(
                        || "evaluation attempt was not scored".to_owned(),
                        |exception| exception.traceback.clone(),
                    ),
                }
            }
        }
    }
}

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
impl CanonicalTaskRunner {
    /// Binds the canonical executor to one configured agent and credential.
    #[must_use]
    pub const fn new(nanocodex: NanocodexBuilder, auth: HarnessAuth) -> Self {
        Self { nanocodex, auth }
    }

    /// Executes, verifies, and retains exactly one canonical claimed task.
    pub async fn run(
        &self,
        claimed: ClaimedEvaluationTask,
    ) -> Result<EvaluationExecution, EvaluationExecutionError> {
        self.execute(claimed).await
    }

    async fn execute(
        &self,
        claimed: ClaimedEvaluationTask,
    ) -> Result<EvaluationExecution, EvaluationExecutionError> {
        let (task, selector, treatment, harness, harnesses, output) = claimed.into_parts();
        fs::create_dir_all(&output).execution_context(format!(
            "failed to create canonical output directory {}",
            output.display()
        ))?;
        let host = PreparedVmHost::open()?;
        let resources = prepare_resources(&task, &harnesses, &host).await?;
        let judge = JudgeRuntime::start(
            self.nanocodex
                .clone()
                .thinking(nanocodex_agent::Thinking::Low),
        )
        .await
        .execution_context("failed to start canonical verifier judge")?;
        let verifier_environment = judge.verifier_environment();
        let snapshot = ClaimedEvaluationTask::new(
            task.clone(),
            selector,
            treatment.clone(),
            harness.clone(),
            harnesses,
            output.clone(),
        );
        if treatment.harness == "nanocodex" {
            let backend = resources
                .backend_with(
                    VmBackend::builder()
                        .retain_passed_rootfs(false)
                        .retain_failed_rootfs(false)
                        .verifier_environment(verifier_environment),
                )
                .await
                .execution_context("failed to prepare canonical Nanocodex VM backend")?;
            let evaluator = Evaluator::builder(self.nanocodex.clone(), backend)
                .output_directory(&output)
                .build()
                .execution_context("failed to build canonical Nanocodex evaluator")?;
            let runner = EvaluatorRunner::new(evaluator);
            let outcome = runner.run_outcome(snapshot).await?;
            Ok(runner.classify_outcome(&outcome))
        } else {
            let configured = harness.ok_or_else(|| {
                EvaluationExecutionError::invariant(
                    "external harness lost its resolved configuration",
                )
            })?;
            let harness = Harness::new(
                self.nanocodex.clone(),
                task.clone(),
                &configured.command,
                &configured.guest_command,
                self.auth.clone(),
                resources,
            )
            .model(treatment.model)
            .output_directory(&output)
            .thinking(treatment.thinking)
            .web_search(treatment.web_search)
            .guest_memory_mb(task.resources().memory_mb)
            .arguments(configured.arguments)
            .environment(configured.environment.into_iter().collect())
            .verifier_environment(verifier_environment)
            .credentials(
                configured.home,
                configured.auth_file,
                configured.api_key_environment,
            )
            .api_upstream(configured.api_upstream)
            .version(configured.version)
            .name(configured.name)
            .prepare()
            .await
            .execution_context("failed to prepare configured external harness")?;
            let runner = EvaluatorRunner::new(harness.evaluator().clone());
            let outcome = runner.run_outcome(snapshot).await?;
            harness
                .retain_trajectory(&outcome)
                .await
                .execution_context("failed to retain external harness trajectory")?;
            Ok(runner.classify_outcome(&outcome))
        }
    }
}

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
impl PreparedVmHost {
    fn open() -> Result<Self, EvaluationExecutionError> {
        let started_at = Instant::now();
        let executable = std::env::current_exe()
            .execution_context("failed to locate the running Nanocodex executable")?;
        let vmm = fs::canonicalize(&executable).execution_context(format!(
            "failed to resolve the running Nanocodex executable {}",
            executable.display()
        ))?;
        validate_hypervisor_entitlement(&vmm)?;
        let runtime = installed_guest_runtime(&vmm)?;
        let runtime_bytes = fs::read(&runtime).execution_context(format!(
            "failed to read VM guest runtime {}",
            runtime.display()
        ))?;
        validate_vm_guest_elf(&runtime_bytes, &runtime)?;
        let cache = default_vm_cache()?;
        fs::create_dir_all(&cache)
            .execution_context(format!("failed to create VM cache {}", cache.display()))?;
        let runtime_disk =
            GuestRuntimeDisk::prepare(&runtime, &cache).execution_context(format!(
                "failed to prepare VM guest runtime disk from {} in {}",
                runtime.display(),
                cache.display()
            ))?;
        tracing::info!(
            target: "nanocodex_vm",
            duration_ns = u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
            vm_guest_target = VM_GUEST_TARGET,
            vm_guest_runtime_cache_status = ?runtime_disk.status(),
            vm_guest_runtime_digest = runtime_disk.digest(),
            "canonical VM guest runtime ready"
        );
        Ok(Self {
            vmm,
            runtime,
            runtime_image: runtime_disk.path().to_path_buf(),
            cache,
        })
    }
}

/// Validates and prepares the complete VM-backed evaluation host installation.
///
/// Controllers call this before admitting workers so a missing, malformed, or
/// incompatible guest runtime fails at the process boundary instead of being
/// rediscovered independently by every claimed task.
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
pub fn validate_prepared_eval_host() -> Result<(), EvaluationExecutionError> {
    PreparedVmHost::open().map(drop)
}

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
async fn prepare_resources(
    task: &Task,
    harnesses: &[ResolvedHarness],
    host: &PreparedVmHost,
) -> Result<VmResources, EvaluationExecutionError> {
    let mut builder = VmResources::builder(&host.vmm, &host.runtime_image)
        .task(task.clone())
        .cache_directory(&host.cache)
        .cache_policy(CachePolicy::Reuse)
        .image_preparation_concurrency(1)
        .require_gvproxy(!harnesses.is_empty());
    if !harnesses.is_empty() {
        builder = builder.guest_executable(&host.runtime, CAPTURE_ONLY_GUEST_RUNTIME);
    }
    for harness in harnesses {
        builder = builder.guest_executable(&harness.command, &harness.guest_command);
    }
    let resources = builder
        .prepare()
        .await
        .execution_context("failed to prepare canonical task VM resources")?;
    resources
        .backend()
        .await
        .execution_context("failed to initialize canonical task VM backend")?;
    Ok(resources)
}

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn installed_guest_runtime(vmm: &Path) -> Result<PathBuf, EvaluationExecutionError> {
    let directory = vmm.parent().ok_or_else(|| {
        EvaluationExecutionError::invariant(format!(
            "Nanocodex executable has no installation directory: {}",
            vmm.display()
        ))
    })?;
    let runtime = directory.join("nanocodex-vm-guest");
    if !runtime.is_file() {
        return Err(EvaluationExecutionError::invariant(format!(
            "prepared eval host is missing {}; install the matching {VM_GUEST_TARGET} \
             `nanocodex-vm-guest` beside the Nanocodex executable (source checkouts can run \
             `just build-eval-host`)",
            runtime.display()
        )));
    }
    fs::canonicalize(&runtime).execution_context(format!(
        "failed to resolve VM guest runtime {}",
        runtime.display()
    ))
}

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn default_vm_cache() -> Result<PathBuf, EvaluationExecutionError> {
    if let Some(home) = std::env::var_os("NANOCODEX_HOME").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(home).join("cache/vm"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| {
            EvaluationExecutionError::invariant("home directory is unavailable; set NANOCODEX_HOME")
        })?;
    Ok(PathBuf::from(home).join(".cache/nanocodex/vm"))
}

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn validate_vm_guest_elf(bytes: &[u8], path: &Path) -> Result<(), EvaluationExecutionError> {
    let header = bytes.get(..20).ok_or_else(|| {
        EvaluationExecutionError::invariant(format!(
            "VM guest runtime is too short to contain an ELF header: {}",
            path.display()
        ))
    })?;
    if &header[..4] != b"\x7fELF" {
        return Err(EvaluationExecutionError::invariant(format!(
            "VM guest runtime is not an ELF executable: {}",
            path.display()
        )));
    }
    let class = header[4];
    let byte_order = header[5];
    let machine = u16::from_le_bytes([header[18], header[19]]);
    if class != 2 || byte_order != 1 || machine != VM_GUEST_ELF_MACHINE {
        return Err(EvaluationExecutionError::invariant(format!(
            "VM guest runtime {} has ELF class {class}, byte order {byte_order}, and e_machine \
             {machine}; this host requires the matching {VM_GUEST_TARGET} runtime (64-bit \
             little-endian e_machine {VM_GUEST_ELF_MACHINE})",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_hypervisor_entitlement(vmm: &Path) -> Result<(), EvaluationExecutionError> {
    let output = std::process::Command::new("/usr/bin/codesign")
        .args(["-d", "--entitlements", "-"])
        .arg(vmm)
        .output()
        .execution_context("failed to inspect the Nanocodex code signature")?;
    let mut report = output.stdout;
    report.extend_from_slice(&output.stderr);
    if !output.status.success()
        || !String::from_utf8_lossy(&report).contains("com.apple.security.hypervisor")
    {
        return Err(EvaluationExecutionError::invariant(format!(
            "prepared eval host lacks the macOS `com.apple.security.hypervisor` entitlement: \
             {} (source checkouts can run `just build-eval-host`)",
            vmm.display()
        )));
    }
    Ok(())
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
const fn validate_hypervisor_entitlement(_vmm: &Path) -> Result<(), EvaluationExecutionError> {
    Ok(())
}

async fn record_events(
    mut stream: EvalEventStream,
    path: &Path,
) -> Result<NativeEventRecording, EvaluationExecutionError> {
    let mut output = tokio::fs::File::create(path)
        .await
        .execution_context(format!(
            "failed to create evaluator event log {}",
            path.display()
        ))?;
    let mut atif = AtifBuilder::default();
    let mut atif_error = None;
    while let Some(event) = stream
        .recv()
        .await
        .execution_context("failed to receive canonical evaluator event")?
    {
        if let EvalEventKind::Agent(agent_event) = &event.kind
            && atif_error.is_none()
            && let Err(error) = atif.apply(agent_event)
        {
            atif_error = Some(EvaluationExecutionError::context(
                format!(
                    "failed to project agent event sequence {} into ATIF",
                    event.sequence
                ),
                error,
            ));
        }
        let mut encoded = serde_json::to_vec(event.as_ref())
            .execution_context("failed to encode canonical evaluator event")?;
        encoded.push(b'\n');
        output.write_all(&encoded).await.execution_context(format!(
            "failed to write evaluator event log {}",
            path.display()
        ))?;
    }
    output.flush().await.execution_context(format!(
        "failed to flush evaluator event log {}",
        path.display()
    ))?;
    output.sync_all().await.execution_context(format!(
        "failed to sync evaluator event log {}",
        path.display()
    ))?;
    Ok(NativeEventRecording { atif, atif_error })
}

async fn retain_trajectory(
    outcome: &EvalAttemptOutcome,
    recording: NativeEventRecording,
) -> Result<(), EvaluationExecutionError> {
    if let Some(error) = recording.atif_error {
        return Err(error);
    }
    let trajectory = match outcome.agent() {
        Some(agent) => recording.atif.finish(outcome.task(), agent),
        None => recording.atif.finish_failure(outcome.task()),
    };
    let path = outcome.artifacts().directory.join("agent/trajectory.json");
    let parent = path.parent().ok_or_else(|| {
        EvaluationExecutionError::invariant(format!(
            "trajectory path has no parent: {}",
            path.display()
        ))
    })?;
    tokio::fs::create_dir_all(parent)
        .await
        .execution_context(format!(
            "failed to create trajectory directory {}",
            parent.display()
        ))?;
    let mut encoded = serde_json::to_vec_pretty(&trajectory)
        .execution_context("failed to encode canonical ATIF trajectory")?;
    encoded.push(b'\n');
    let mut output = tokio::fs::File::create(&path)
        .await
        .execution_context(format!("failed to create trajectory {}", path.display()))?;
    output
        .write_all(&encoded)
        .await
        .execution_context(format!("failed to write trajectory {}", path.display()))?;
    output
        .flush()
        .await
        .execution_context(format!("failed to flush trajectory {}", path.display()))?;
    output
        .sync_all()
        .await
        .execution_context(format!("failed to sync trajectory {}", path.display()))?;
    Ok(())
}

impl Display for EvaluationExecutionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.source, formatter)
    }
}

impl Error for EvaluationExecutionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

impl Display for ContextualExecutionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.context)
    }
}

impl Error for ContextualExecutionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

impl Display for ExecutionInvariantError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ExecutionInvariantError {}
