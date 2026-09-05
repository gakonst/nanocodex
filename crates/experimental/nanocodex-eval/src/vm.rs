//! VM-backed evaluation attempts.
//!
//! This module composes the evaluator lifecycle with `nanocodex-vm`. Callers
//! prepare task images and one guest runtime, configure a [`VmBackend`], and
//! pass it to [`Evaluator::builder`]. Every admitted attempt receives
//! a fresh writable root disk, an isolated guest tool session, and a verifier
//! that owns cleanup of the same environment.

use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fs,
    future::Future,
    io,
    io::{Read as _, Write as _},
    net::{Ipv4Addr, TcpListener},
    num::ParseFloatError,
    os::unix::fs::PermissionsExt as _,
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use arcbox_ext4::{
    Formatter, Reader,
    constants::{file_mode, make_mode},
};
use chrono::{DateTime, Utc};
use jiff::{Timestamp, tz::TimeZone};
use nanocodex_agent::{ExecutionEnvironment, NanocodexBuilder};
use nanocodex_tools::{Tools, ToolsBuildError, standard::UpdatePlanTool};
use nanocodex_vm::{
    host::{
        BlockDevice, GuestCommand, Gvproxy as GvproxyProcess, GvproxyError as VmGvproxyError,
        Network, OverlayDiskError, VmConfig, create_sparse_overlay_disk, overlay_guest_command,
    },
    tools::{VmCommandOutput, VmCommandPartialOutput, VmToolSession},
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{
    process::Command,
    sync::{OnceCell as AsyncOnceCell, Semaphore},
};
use tracing::{info, info_span, warn};

pub use nanocodex_vm::{
    host::SharedDirectory,
    image::{
        CachePolicy, DiskStatus, ImageError, PreparedRootDisk, VmImageBuilder,
        reflink_or_sparse_copy,
    },
    tools::{
        GuestRuntimeDisk, GuestRuntimeDiskStatus, VmCommand, VmToolSessionError,
        VmToolSessionHandle,
    },
};

use crate::{
    CleanupPhase, EvalEnvironment, Evaluator, EvaluatorBuilder, HarnessExec, NetworkPolicy, Task,
    TaskLoadError, TaskOutput, VerifierEnvironmentMode, VerifierResult,
    evaluator::{
        AttemptAgent, AttemptVerification, AttemptVerificationFailure, AttemptVerifier, EvalAttempt,
    },
};

const EMBEDDED_GUEST_TOOL_RUNTIME: &str = "/usr/local/bin/nanocodex-vm-guest";
const BLOCK_GUEST_TOOL_RUNTIME: &str = "/run/nanoeval/nanocodex-vm-guest";
const GUEST_RUNTIME_BLOCK_ID: &str = "nanoeval-runtime";
const GUEST_RUNTIME_BLOCK_DEVICE: &str = "/dev/vdb";
const GUEST_RUNTIME_MOUNT: &str = "/run/nanoeval";
const DEFAULT_VM_CACHE: &str = ".cache/vm";
const DEFAULT_KRUNFW_DIRECTORY: &str = ".cache/libkrunfw/libkrunfw";
#[cfg(target_os = "linux")]
const KRUNFW_LIBRARY_FILENAME: &str = "libkrunfw.so.5";
#[cfg(target_os = "macos")]
const KRUNFW_LIBRARY_FILENAME: &str = "libkrunfw.5.dylib";
#[cfg(target_os = "linux")]
const KRUNFW_LIBRARY_PATH_ENVIRONMENT: &str = "LD_LIBRARY_PATH";
#[cfg(target_os = "macos")]
const KRUNFW_LIBRARY_PATH_ENVIRONMENT: &str = "DYLD_LIBRARY_PATH";
#[cfg(target_arch = "aarch64")]
const VM_GUEST_TARGET: &str = "aarch64-unknown-linux-musl";
#[cfg(target_arch = "x86_64")]
const VM_GUEST_TARGET: &str = "x86_64-unknown-linux-musl";
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
compile_error!("Evaluator VM guests are only supported on aarch64 and x86_64 hosts");
const VERIFIER_CACHE_VERSION: u32 = 2;
const MINIMUM_VERIFIER_CACHE_DISK_BYTES: u64 = 512 * 1024 * 1024;
const MAXIMUM_VERIFIER_CACHE_DISK_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const VERIFIER_SETUP_MARKER: &str = "# Check if we're in a valid working directory";
const VERIFIER_CACHE_BLOCK_ID: &str = "nanoeval-verifier-cache";
const VERIFIER_CACHE_BLOCK_DEVICE: &str = "/dev/vdc";
const OVERLAY_VERIFIER_CACHE_BLOCK_DEVICE: &str = "/dev/vdd";
const VERIFIER_CACHE_MOUNT: &str = "/run/nanoeval-verifier-cache";
const CACHED_VERIFIER_SCRIPT: &str = "/tmp/nanoeval-verifier.sh";
const PRE_ARTIFACTS_GUEST_SCRIPT: &str = "/tmp/nanocodex-pre-artifacts.sh";
const GUEST_PUBLIC_RESOLV_CONF: &str =
    "nameserver 192.168.127.1\\nnameserver 1.1.1.1\\noptions timeout:2 attempts:5\\n";
const DEFAULT_IMAGE_NETWORK_RETRIES: usize = 2;
const DEFAULT_IMAGE_PREPARATION_CONCURRENCY: usize = 4;
const IMAGE_NETWORK_RETRY_BASE_DELAY: Duration = Duration::from_secs(2);
const VERIFIER_NETWORK_RETRIES: usize = 4;
const VERIFIER_NETWORK_RETRY_BASE_DELAY: Duration = Duration::from_secs(2);
const BYTES_PER_MIB: u64 = 1024 * 1024;
const GVPROXY_VERSION: &str = "v0.8.9";
const EVAL_IMAGE_RUN_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const GUEST_PROJECT_INSTRUCTIONS_TIMEOUT: Duration = Duration::from_secs(5);
const GUEST_PROJECT_INSTRUCTIONS_MAX_BYTES: usize = 32 * 1024;
const GUEST_PROJECT_INSTRUCTION_PATHS_MAX_BYTES: usize = 1024 * 1024;
const GUEST_PROJECT_INSTRUCTION_PATHS_SCRIPT: &str = r#"
workspace=${1%/}
[ -n "$workspace" ] || workspace=/
case "$workspace" in
    /*) ;;
    *) exit 64 ;;
esac

cursor=$workspace
project_root=
while :; do
    if [ -e "$cursor/.git" ]; then
        project_root=$cursor
        break
    fi
    [ "$cursor" = / ] && break
    cursor=${cursor%/*}
    [ -n "$cursor" ] || cursor=/
done
[ -n "$project_root" ] || project_root=$workspace

cursor=$workspace
while :; do
    for filename in AGENTS.override.md AGENTS.md; do
        if [ "$cursor" = / ]; then
            candidate=/$filename
        else
            candidate=$cursor/$filename
        fi
        if [ -f "$candidate" ]; then
            printf '%s\000' "$candidate"
            break
        fi
    done
    [ "$cursor" = "$project_root" ] && break
    parent=${cursor%/*}
    [ -n "$parent" ] || parent=/
    [ "$parent" != "$cursor" ] || exit 65
    cursor=$parent
done
"#;
// `vm-run-config` is a thin executor for `VmProcessConfig`. Bump this identity
// whenever that execution boundary can change Dockerfile build output. Agent,
// evaluator, capture, or reporting changes must not invalidate task images.
const EVAL_VMM_BUILD_CACHE_IDENTITY: &str = "nanocodex-eval-vm-process-v1";
const DEFAULT_GUEST_TIMEZONE: &str = "Etc/UTC";
const ZONEINFO_PREFIXES: [&str; 4] = [
    "/usr/share/zoneinfo/",
    "../usr/share/zoneinfo/",
    "/etc/zoneinfo/",
    "../etc/zoneinfo/",
];

type PreparedEnvironmentCell = Arc<AsyncOnceCell<Result<VmEnvironment, Arc<str>>>>;

#[derive(Clone, Debug)]
struct VmGuestExecutable {
    source: PathBuf,
    guest_path: String,
}

fn effective_guest_memory_mb(declared_memory_mb: u64, max_guest_memory_mb: Option<u64>) -> u64 {
    max_guest_memory_mb
        .map_or(declared_memory_mb, |limit| declared_memory_mb.min(limit))
        .clamp(1, u64::from(u32::MAX))
}

/// Prepared VM resources shared by every attempt in one evaluation run.
///
/// Use [`VmResources::builder`] to select tasks and deliberate cache policy.
/// Image materialization, network helper discovery, task-to-environment
/// mapping, and backend configuration remain owned by this type.
pub struct VmResources {
    vmm: PathBuf,
    runtime_image: PathBuf,
    tasks: Vec<Task>,
    environments: BTreeMap<PathBuf, PreparedEnvironmentCell>,
    environment_source: VmEnvironmentSource,
    preparation_slots: Arc<Semaphore>,
    max_guest_memory_mb: Option<u64>,
    gvproxy: Option<PathBuf>,
    verifier_cache: PathBuf,
}

/// Deliberate policy for preparing [`VmResources`].
pub struct VmResourcesBuilder {
    vmm: PathBuf,
    runtime_image: PathBuf,
    tasks: Vec<Task>,
    rootfs: Option<PathBuf>,
    cache: PathBuf,
    cache_policy: CachePolicy,
    max_guest_memory_mb: Option<u64>,
    image_network_retries: usize,
    image_preparation_concurrency: usize,
    gvproxy: Option<PathBuf>,
    require_gvproxy: bool,
    guest_executables: Vec<VmGuestExecutable>,
}

#[derive(Clone)]
enum VmEnvironmentSource {
    Rootfs(VmEnvironment),
    Image {
        cache: PathBuf,
        policy: CachePolicy,
        builder: VmImageBuilder,
        network_retries: usize,
        guest_executables: Arc<[VmGuestExecutable]>,
    },
}

impl VmResources {
    /// Prepares a default VM backend for every task in this resource set.
    ///
    /// # Errors
    ///
    /// Returns an error when task environments cannot be prepared.
    pub async fn backend(&self) -> Result<VmBackend, VmResourcesError> {
        self.backend_with(VmBackend::builder()).await
    }

    /// Starts a resource recipe around one VMM executable and guest-runtime disk.
    #[must_use]
    pub fn builder(
        vmm: impl Into<PathBuf>,
        runtime_image: impl Into<PathBuf>,
    ) -> VmResourcesBuilder {
        VmResourcesBuilder {
            vmm: vmm.into(),
            runtime_image: runtime_image.into(),
            tasks: Vec::new(),
            rootfs: None,
            cache: PathBuf::from(DEFAULT_VM_CACHE),
            cache_policy: CachePolicy::Reuse,
            max_guest_memory_mb: None,
            image_network_retries: DEFAULT_IMAGE_NETWORK_RETRIES,
            image_preparation_concurrency: DEFAULT_IMAGE_PREPARATION_CONCURRENCY,
            gvproxy: None,
            require_gvproxy: false,
            guest_executables: Vec::new(),
        }
    }

    /// Configures a fresh backend from these prepared resources.
    ///
    /// # Errors
    ///
    /// Returns an error when immutable backend configuration fails.
    pub async fn backend_with(
        &self,
        builder: VmBackendBuilder,
    ) -> Result<VmBackend, VmResourcesError> {
        self.backend_for_tasks(builder, &self.tasks, None).await
    }

    pub(crate) async fn backend_for_task_with_guest_memory(
        &self,
        builder: VmBackendBuilder,
        task: &Task,
        guest_memory_mb: u64,
    ) -> Result<VmBackend, VmResourcesError> {
        self.backend_for_tasks(builder, std::slice::from_ref(task), Some(guest_memory_mb))
            .await
    }

    async fn backend_for_tasks(
        &self,
        builder: VmBackendBuilder,
        tasks: &[Task],
        guest_memory_mb: Option<u64>,
    ) -> Result<VmBackend, VmResourcesError> {
        let backend = builder.build();
        self.configure_for_tasks(&backend, tasks, guest_memory_mb)
            .await?;
        Ok(backend)
    }

    /// Installs these resources into an existing unconfigured backend.
    ///
    /// This form supports evaluators that create their durable job directory
    /// before image preparation. The backend is still configured exactly once.
    ///
    /// # Errors
    ///
    /// Returns an error when immutable backend configuration fails.
    pub async fn configure(&self, backend: &VmBackend) -> Result<(), VmResourcesError> {
        self.configure_for_tasks(backend, &self.tasks, None).await
    }

    async fn configure_for_tasks(
        &self,
        backend: &VmBackend,
        tasks: &[Task],
        guest_memory_mb: Option<u64>,
    ) -> Result<(), VmResourcesError> {
        let environments = self.prepare_tasks(tasks).await?;
        let mut configuration = VmBackendConfiguration::builder(&self.vmm, &self.runtime_image)
            .environments(environments)
            .verifier_cache(&self.verifier_cache);
        if let Some(max_guest_memory_mb) = guest_memory_mb.or(self.max_guest_memory_mb) {
            configuration = configuration.max_guest_memory_mb(max_guest_memory_mb);
        }
        if let Some(gvproxy) = &self.gvproxy {
            configuration = configuration.gvproxy(gvproxy);
        }
        backend.configure(configuration.build())?;
        Ok(())
    }

    /// Prepares and returns one task environment through its shared
    /// single-flight cell.
    ///
    /// This detailed accessor is intended for custom guest harnesses such as
    /// external harness. Normal Nanocodex evaluators only need
    /// [`Self::backend`].
    pub(crate) async fn environment(&self, task: &Task) -> Result<VmEnvironment, VmResourcesError> {
        let cell = self
            .environments
            .get(task.root())
            .ok_or_else(|| VmResourcesError::UnknownTask(task.root().to_path_buf()))?;
        let task = task.clone();
        let task_name = task.name().to_owned();
        let task_to_prepare = task.clone();
        let source = self.environment_source.clone();
        let slots = Arc::clone(&self.preparation_slots);
        cell.get_or_init(|| async move {
            let permit = slots.acquire_owned().await.map_err(|error| {
                Arc::<str>::from(format!("image preparation scheduler closed: {error}"))
            })?;
            let result = prepare_vm_environment(&task_to_prepare, &source)
                .await
                .map_err(|error| Arc::<str>::from(format!("{error:#}")));
            drop(permit);
            result
        })
        .await
        .clone()
        .map_err(|message| VmResourcesError::TaskPreparation {
            task: task_name,
            message,
        })
    }

    async fn prepare_tasks(
        &self,
        tasks: &[Task],
    ) -> Result<BTreeMap<PathBuf, VmEnvironment>, VmResourcesError> {
        let mut preparations = futures_util::stream::FuturesUnordered::new();
        for task in tasks {
            preparations.push(async move { (task.clone(), self.environment(task).await) });
        }
        let mut environments = BTreeMap::new();
        while let Some((task, environment)) = futures_util::StreamExt::next(&mut preparations).await
        {
            environments.insert(task.root().to_path_buf(), environment?);
        }
        Ok(environments)
    }
}

impl VmResourcesBuilder {
    /// Adds one task to this VM run.
    #[must_use]
    pub fn task(mut self, task: Task) -> Self {
        self.tasks.push(task);
        self
    }

    /// Adds every task to this VM run.
    #[must_use]
    pub fn tasks(mut self, tasks: impl IntoIterator<Item = Task>) -> Self {
        self.tasks.extend(tasks);
        self
    }

    /// Uses one already prepared root filesystem for every selected task.
    ///
    /// A raw ext4 image uses `/app`; a directory root uses `/workspace`.
    #[must_use]
    pub fn rootfs(mut self, rootfs: impl Into<PathBuf>) -> Self {
        self.rootfs = Some(rootfs.into());
        self
    }

    /// Selects the content-addressed image and verifier-cache directory.
    #[must_use]
    pub fn cache_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.cache = directory.into();
        self
    }

    /// Selects whether OCI image references may reuse their local resolution.
    #[must_use]
    pub const fn cache_policy(mut self, policy: CachePolicy) -> Self {
        self.cache_policy = policy;
        self
    }

    /// Caps guest RAM for each attempt without modifying the benchmark task.
    ///
    /// This is an evaluator allocation policy. Tasks declaring less memory
    /// retain their declaration, and task metadata remains unchanged in
    /// retained evidence.
    #[must_use]
    pub const fn max_guest_memory_mb(mut self, memory_mb: u64) -> Self {
        self.max_guest_memory_mb = Some(memory_mb);
        self
    }

    /// Sets whole-image retries after a recognized transient build-network failure.
    ///
    /// Each retry starts again from the immutable task inputs and content
    /// cache. Deterministic Dockerfile failures are never retried. The default
    /// is two retries.
    #[must_use]
    pub const fn image_network_retries(mut self, retries: usize) -> Self {
        self.image_network_retries = retries;
        self
    }

    /// Bounds concurrent cold task-image materialization.
    ///
    /// Warm cache hits still join the same task-local single-flight cell. The
    /// default is four independent image preparations.
    #[must_use]
    pub const fn image_preparation_concurrency(mut self, concurrency: usize) -> Self {
        self.image_preparation_concurrency = concurrency;
        self
    }

    /// Pins the gvproxy executable used by tasks that request public network.
    ///
    /// When omitted, preparation discovers an installed executable or fetches
    /// the pinned evaluator release into the VM cache.
    #[must_use]
    pub fn gvproxy(mut self, executable: impl Into<PathBuf>) -> Self {
        self.gvproxy = Some(executable.into());
        self
    }

    /// Prepares gvproxy even when the benchmark itself declares no public network.
    ///
    /// External harnesses need this evaluator-owned path to their host capture
    /// proxy. Whether an attempt actually attaches to it remains backend policy.
    #[must_use]
    pub const fn require_gvproxy(mut self, required: bool) -> Self {
        self.require_gvproxy = required;
        self
    }

    /// Installs one host executable into every immutable prepared task image.
    #[must_use]
    pub fn guest_executable(
        mut self,
        source: impl Into<PathBuf>,
        guest_path: impl Into<String>,
    ) -> Self {
        self.guest_executables.push(VmGuestExecutable {
            source: source.into(),
            guest_path: guest_path.into(),
        });
        self
    }

    /// Discovers shared VM resources and installs lazy task-image recipes.
    ///
    /// Task images materialize through bounded single-flight cells when first
    /// requested. Explicit evaluator configuration still resolves all tasks
    /// selected for that backend before admitting its attempts.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty task set, unsupported Compose topology,
    /// invalid overrides, image preparation failures, or network helper
    /// failures.
    pub async fn prepare(self) -> Result<VmResources, VmResourcesError> {
        if self.tasks.is_empty() {
            return Err(VmResourcesError::NoTasks);
        }
        if self.max_guest_memory_mb == Some(0) {
            return Err(VmResourcesError::InvalidMemory);
        }
        if self.image_preparation_concurrency == 0 {
            return Err(VmResourcesError::InvalidPreparationConcurrency);
        }
        if let Some(task) = self.tasks.iter().find(|task| task.requires_compose()) {
            return Err(VmResourcesError::Compose(task.name().to_owned()));
        }
        for executable in &self.guest_executables {
            if !executable.source.is_file() {
                return Err(VmResourcesError::InvalidGuestExecutable(
                    executable.source.clone(),
                ));
            }
            if !valid_guest_executable_path(&executable.guest_path) {
                return Err(VmResourcesError::InvalidGuestExecutablePath(
                    executable.guest_path.clone(),
                ));
            }
        }
        let environment_source = if let Some(rootfs) = self.rootfs {
            if !self.guest_executables.is_empty() {
                return Err(VmResourcesError::GuestExecutablesWithRootfs);
            }
            if !rootfs.exists() {
                return Err(VmResourcesError::InvalidRootfs(rootfs));
            }
            let workspace = if rootfs.is_file() {
                "/app"
            } else {
                "/workspace"
            };
            let timezone = guest_timezone(&rootfs);
            VmEnvironmentSource::Rootfs(
                VmEnvironment::new(rootfs, workspace, "bash").timezone(timezone),
            )
        } else {
            VmEnvironmentSource::Image {
                cache: self.cache.clone(),
                policy: self.cache_policy,
                builder: image_builder(&self.vmm, &self.runtime_image),
                network_retries: self.image_network_retries,
                guest_executables: self.guest_executables.into(),
            }
        };
        let environments = self
            .tasks
            .iter()
            .map(|task| (task.root().to_path_buf(), Arc::new(AsyncOnceCell::new())))
            .collect();
        let public_network = self.require_gvproxy
            || self.tasks.iter().any(|task| {
                task.network() == NetworkPolicy::Public
                    || task.verifier().network() == NetworkPolicy::Public
            });
        let gvproxy = if public_network {
            match self.gvproxy {
                Some(path) if path.is_file() => Some(path),
                Some(path) => return Err(VmResourcesError::InvalidGvproxy(path)),
                None => Some(prepare_gvproxy(&self.cache).await?),
            }
        } else {
            None
        };
        Ok(VmResources {
            vmm: self.vmm,
            runtime_image: self.runtime_image,
            tasks: self.tasks,
            environments,
            environment_source,
            preparation_slots: Arc::new(Semaphore::new(self.image_preparation_concurrency)),
            max_guest_memory_mb: self.max_guest_memory_mb,
            gvproxy,
            verifier_cache: self.cache,
        })
    }
}

/// Failure while preparing or installing one VM evaluation resource set.
#[derive(Debug, thiserror::Error)]
pub enum VmResourcesError {
    /// No task was selected.
    #[error("a VM evaluation requires at least one task")]
    NoTasks,

    /// The eval-only guest-memory cap was zero.
    #[error("VM evaluation guest memory must be greater than zero")]
    InvalidMemory,

    /// The cold-image preparation bound was zero.
    #[error("VM image preparation concurrency must be greater than zero")]
    InvalidPreparationConcurrency,

    /// A task outside the selected resource set was requested.
    #[error("task {0} was not selected for this VM resource set")]
    UnknownTask(PathBuf),

    /// One task's immutable environment failed its single-flight preparation.
    #[error("failed to prepare VM environment for task {task}: {message}")]
    TaskPreparation {
        /// Stable task name.
        task: String,
        /// Shared preparation diagnostic returned to every waiter.
        message: Arc<str>,
    },

    /// The single-guest backend cannot reproduce a Compose topology.
    #[error(
        "task {0} requires a custom Docker Compose topology; the single-guest eval backend does not implement Compose tasks"
    )]
    Compose(String),

    /// A root filesystem override did not exist.
    #[error("VM rootfs override does not exist: {0}")]
    InvalidRootfs(PathBuf),

    /// A pinned network helper was not a regular file.
    #[error("gvproxy override does not name a file: {0}")]
    InvalidGvproxy(PathBuf),

    /// A prepared guest executable was missing.
    #[error("guest executable does not name a file: {0}")]
    InvalidGuestExecutable(PathBuf),

    /// A prepared guest executable destination was not absolute.
    #[error("guest executable destination must be absolute: {0}")]
    InvalidGuestExecutablePath(String),

    /// An already prepared rootfs cannot be extended immutably.
    #[error("guest executables cannot be installed into an explicit rootfs override")]
    GuestExecutablesWithRootfs,

    /// The pinned network helper is unavailable for this host.
    #[error("gvproxy is not published for {os}/{architecture}")]
    UnsupportedPlatform {
        /// Host operating system.
        os: &'static str,
        /// Host architecture.
        architecture: &'static str,
    },

    /// Fetching the pinned network helper failed.
    #[error("failed to download gvproxy: curl exited with {0}")]
    NetworkDownload(std::process::ExitStatus),

    /// The fetched network helper did not match its pinned digest.
    #[error("downloaded gvproxy digest was {actual}, expected {expected}")]
    NetworkDigest {
        /// Pinned digest.
        expected: &'static str,
        /// Observed digest.
        actual: String,
    },

    /// Task package loading or validation failed.
    #[error(transparent)]
    Task(#[from] TaskLoadError),

    /// OCI-to-ext4 image preparation failed.
    #[error(transparent)]
    Image(#[from] ImageError),

    /// VM backend configuration failed.
    #[error(transparent)]
    Configure(#[from] VmBackendConfigureError),

    /// VM attempt or verifier-cache preparation failed.
    #[error(transparent)]
    Attempt(#[from] VmAttemptError),

    /// Host filesystem or subprocess I/O failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Creates the evaluator's canonical image builder for explicit cache warming.
///
/// Normal evaluation consumers should use [`VmResources::builder`]. This
/// detailed constructor exists for preparation commands that need to report
/// individual image cache outcomes without admitting attempts.
#[must_use]
pub fn image_builder(vmm: &Path, runtime_image: &Path) -> VmImageBuilder {
    let builder = VmImageBuilder::new(vmm, runtime_image)
        .vmm_args(["vm-run-config", "--config"])
        .vmm_build_cache_identity(EVAL_VMM_BUILD_CACHE_IDENTITY)
        .prefer_ipv4()
        .run_timeout(EVAL_IMAGE_RUN_TIMEOUT);
    let firmware = Path::new(DEFAULT_KRUNFW_DIRECTORY);
    if firmware.join(KRUNFW_LIBRARY_FILENAME).is_file() {
        builder.firmware_directory(firmware)
    } else {
        builder
    }
}

async fn prepare_vm_environment(
    task: &Task,
    source: &VmEnvironmentSource,
) -> Result<VmEnvironment, VmResourcesError> {
    let (cache, policy, builder, network_retries, guest_executables) = match source {
        VmEnvironmentSource::Rootfs(environment) => {
            task.validate_package()?;
            return Ok(environment.clone());
        }
        VmEnvironmentSource::Image {
            cache,
            policy,
            builder,
            network_retries,
            guest_executables,
        } => (cache, policy, builder, network_retries, guest_executables),
    };
    task.validate_package()?;
    let prepared = prepare_image_with_network_retries(
        task.name(),
        "task",
        *network_retries,
        || {
            prepare_task_image_with_guest_executables(
                builder,
                task,
                cache,
                *policy,
                guest_executables,
            )
        },
        tokio::time::sleep,
    )
    .await?;
    task.validate_package()?;
    let verifier = if task.verifier().environment_mode() == VerifierEnvironmentMode::Separate {
        let verifier = prepare_image_with_network_retries(
            task.name(),
            "verifier",
            *network_retries,
            || prepare_verifier_image(builder, task, cache, *policy),
            tokio::time::sleep,
        )
        .await?;
        task.validate_package()?;
        info!(
            target: "nanocodex_eval",
            task_name = task.name(),
            oci_manifest_digest = verifier.manifest_digest(),
            oci_manifest_source = verifier.manifest_source().as_str(),
            vm_rootfs_cache_status = verifier.disk_status().as_str(),
            vm_rootfs_path = %verifier.path().display(),
            "separate verifier VM root disk ready"
        );
        Some(
            VmVerifierEnvironment::new(verifier.path(), verifier.workdir(), verifier.shell())
                .environment(verifier.environment().clone()),
        )
    } else {
        None
    };
    info!(
        target: "nanocodex_eval",
        task_name = task.name(),
        oci_manifest_digest = prepared.manifest_digest(),
        oci_manifest_source = prepared.manifest_source().as_str(),
        vm_rootfs_cache_status = prepared.disk_status().as_str(),
        vm_rootfs_path = %prepared.path().display(),
        "VM root disk ready"
    );
    let environment = VmEnvironment::new(prepared.path(), prepared.workdir(), prepared.shell())
        .environment(prepared.environment().clone())
        .timezone(guest_timezone(prepared.path()));
    Ok(verifier.map_or(environment.clone(), |verifier| {
        environment.verifier(verifier)
    }))
}

async fn prepare_image_with_network_retries<T, F, Fut, S, Sleep>(
    task_name: &str,
    image_kind: &'static str,
    max_retries: usize,
    mut prepare: F,
    mut sleep: S,
) -> Result<T, ImageError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ImageError>>,
    S: FnMut(Duration) -> Sleep,
    Sleep: Future<Output = ()>,
{
    let mut retry = 0;
    loop {
        match prepare().await {
            Ok(prepared) => return Ok(prepared),
            Err(error) if retry < max_retries && image_build_network_failed(&error) => {
                let delay = image_network_retry_delay(retry);
                warn!(
                    target: "nanocodex_eval",
                    task_name,
                    image_kind,
                    retry = retry + 1,
                    max_retries,
                    retry_delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                    error = %error,
                    "VM image preparation hit a transient network failure; retrying"
                );
                sleep(delay).await;
                retry += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

fn image_build_network_failed(error: &ImageError) -> bool {
    let ImageError::BuildStep { stdout, stderr, .. } = error else {
        return false;
    };
    let contains = |needle: &str| stdout.contains(needle) || stderr.contains(needle);
    contains("Could not resolve host")
        || contains("Temporary failure resolving")
        || contains("failed to lookup address information")
        || contains("Name or service not known")
        || contains("Network is unreachable")
        || contains("No route to host")
        || contains("Host is unreachable")
}

const fn image_network_retry_delay(retry: usize) -> Duration {
    let exponent = if retry > 8 { 8 } else { retry };
    IMAGE_NETWORK_RETRY_BASE_DELAY.saturating_mul(1_u32 << exponent)
}

fn guest_timezone(rootfs: &Path) -> String {
    let timezone = if rootfs.is_file() {
        ext4_timezone(rootfs)
    } else {
        directory_timezone(rootfs)
    };
    timezone.unwrap_or_else(|| DEFAULT_GUEST_TIMEZONE.to_owned())
}

fn ext4_timezone(rootfs: &Path) -> Option<String> {
    let mut reader = Reader::new(rootfs).ok()?;
    let link_timezone = reader
        .stat_no_follow("/etc/localtime")
        .ok()
        .and_then(|(_, inode)| {
            let size = usize::try_from(inode.file_size()).ok()?;
            if !inode.is_link() || size > inode.block.len() {
                return None;
            }
            std::str::from_utf8(&inode.block[..size])
                .ok()
                .and_then(timezone_from_link)
        });
    link_timezone.or_else(|| {
        reader
            .read_file("/etc/timezone", 0, None)
            .ok()
            .and_then(|contents| String::from_utf8(contents).ok())
            .and_then(timezone_from_file)
    })
}

fn directory_timezone(rootfs: &Path) -> Option<String> {
    fs::read_link(rootfs.join("etc/localtime"))
        .ok()
        .and_then(|target| target.into_os_string().into_string().ok())
        .and_then(|target| timezone_from_link(&target))
        .or_else(|| {
            fs::read_to_string(rootfs.join("etc/timezone"))
                .ok()
                .and_then(timezone_from_file)
        })
}

fn timezone_from_link(target: &str) -> Option<String> {
    ZONEINFO_PREFIXES
        .iter()
        .find_map(|prefix| target.strip_prefix(prefix).map(ToOwned::to_owned))
        .filter(|timezone| !timezone.is_empty())
}

fn timezone_from_file(contents: String) -> Option<String> {
    let timezone = contents.trim();
    (!timezone.is_empty()).then(|| timezone.to_owned())
}

fn current_date(timezone: &str) -> String {
    current_date_at(Timestamp::now(), timezone)
}

fn current_date_at(timestamp: Timestamp, timezone: &str) -> String {
    let timezone_name = timezone.trim_start_matches('/');
    let timezone = match TimeZone::get(timezone_name) {
        Ok(timezone) => timezone,
        Err(_) => TimeZone::UTC,
    };
    timestamp.to_zoned(timezone).date().to_string()
}

async fn prepare_gvproxy(cache: &Path) -> Result<PathBuf, VmResourcesError> {
    for name in ["NANOCODEX_EVAL_GVPROXY", "NANOEVAL_GVPROXY"] {
        let Some(path) = env::var_os(name).filter(|path| !path.is_empty()) else {
            continue;
        };
        let path = PathBuf::from(path);
        return path
            .is_file()
            .then_some(path.clone())
            .ok_or(VmResourcesError::InvalidGvproxy(path));
    }
    if let Some(path) = find_on_path("gvproxy") {
        return Ok(path);
    }
    let artifact = gvproxy_artifact()?;
    let directory = cache.join("gvproxy").join(GVPROXY_VERSION);
    let binary = directory.join("gvproxy");
    if binary.is_file() && file_digest(&binary)? == artifact.digest {
        return Ok(binary);
    }
    fs::create_dir_all(&directory)?;
    let temporary = directory.join(format!("gvproxy.{}.tmp", std::process::id()));
    let status = Command::new("/usr/bin/curl")
        .arg("--fail")
        .arg("--location")
        .arg("--silent")
        .arg("--show-error")
        .arg("--output")
        .arg(&temporary)
        .arg(artifact.url)
        .status()
        .await?;
    if !status.success() {
        return Err(VmResourcesError::NetworkDownload(status));
    }
    let actual = file_digest(&temporary)?;
    if actual != artifact.digest {
        let _ = fs::remove_file(&temporary);
        return Err(VmResourcesError::NetworkDigest {
            expected: artifact.digest,
            actual,
        });
    }
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))?;
    fs::rename(temporary, &binary)?;
    Ok(binary)
}

struct GvproxyArtifact {
    url: &'static str,
    digest: &'static str,
}

fn gvproxy_artifact() -> Result<GvproxyArtifact, VmResourcesError> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64" | "x86_64") => Ok(GvproxyArtifact {
            url: "https://github.com/containers/gvisor-tap-vsock/releases/download/v0.8.9/gvproxy-darwin",
            digest: "c6f7b4bc7f21bf810b5cf54e04d979b014c5d96472a03a9e97fe62a00940067c",
        }),
        ("linux", "aarch64") => Ok(GvproxyArtifact {
            url: "https://github.com/containers/gvisor-tap-vsock/releases/download/v0.8.9/gvproxy-linux-arm64",
            digest: "6ecca02839254c9a0cc184bba7aac63755a22d7ed10d455b852528a99d7f7d4b",
        }),
        ("linux", "x86_64") => Ok(GvproxyArtifact {
            url: "https://github.com/containers/gvisor-tap-vsock/releases/download/v0.8.9/gvproxy-linux-amd64",
            digest: "3011c5629c9138d2050fb23c510e09ae53e30ec52e6a9ab85632bc1550e8ef63",
        }),
        (os, architecture) => Err(VmResourcesError::UnsupportedPlatform { os, architecture }),
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH")
        .into_iter()
        .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}

fn file_digest(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

/// Builds the task's declared OCI environment into a reusable ext4 root disk.
///
/// # Errors
///
/// Returns an error when the task environment cannot be materialized or the
/// VM image builder cannot prepare the root disk.
pub async fn prepare_task_image(
    builder: &VmImageBuilder,
    task: &Task,
    cache: &Path,
    policy: CachePolicy,
) -> Result<PreparedRootDisk, ImageError> {
    prepare_task_image_with_guest_executables(builder, task, cache, policy, &[]).await
}

async fn prepare_task_image_with_guest_executables(
    builder: &VmImageBuilder,
    task: &Task,
    cache: &Path,
    policy: CachePolicy,
    guest_executables: &[VmGuestExecutable],
) -> Result<PreparedRootDisk, ImageError> {
    let context = tempfile::tempdir()?;
    task.materialize_environment(context.path())
        .map_err(io::Error::other)?;
    let installed_bytes = materialize_guest_executables(context.path(), guest_executables)?;
    let image_bytes = task
        .resources()
        .storage_mb
        .saturating_mul(BYTES_PER_MIB)
        .saturating_add(installed_bytes);
    builder
        .prepare(context.path(), image_bytes, cache, policy)
        .await
}

fn materialize_guest_executables(
    context: &Path,
    executables: &[VmGuestExecutable],
) -> Result<u64, io::Error> {
    if executables.is_empty() {
        return Ok(0);
    }
    let directory = context.join(".nanocodex/guest-executables");
    fs::create_dir_all(&directory)?;
    let dockerfile_path = context.join("Dockerfile");
    let mut dockerfile = fs::OpenOptions::new().append(true).open(&dockerfile_path)?;
    let mut installed_bytes = 0_u64;
    for (index, executable) in executables.iter().enumerate() {
        let name = index.to_string();
        let staged = directory.join(&name);
        reflink_or_sparse_copy(&executable.source, &staged)?;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
        installed_bytes = installed_bytes.saturating_add(staged.metadata()?.len());
        let source = format!(".nanocodex/guest-executables/{name}");
        writeln!(dockerfile, "\nCOPY {source} {}", executable.guest_path)?;
    }
    dockerfile.sync_all()?;
    Ok(installed_bytes)
}

fn valid_guest_executable_path(path: &str) -> bool {
    Path::new(path).is_absolute()
        && !path.chars().any(char::is_whitespace)
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

/// Builds a task's separate verifier environment into a reusable ext4 root disk.
///
/// # Errors
///
/// Returns an error when verifier files cannot be materialized or the VM image
/// builder cannot prepare the root disk.
pub async fn prepare_verifier_image(
    builder: &VmImageBuilder,
    task: &Task,
    cache: &Path,
    policy: CachePolicy,
) -> Result<PreparedRootDisk, ImageError> {
    let context = tempfile::tempdir()?;
    task.materialize_verifier_files(context.path())
        .map_err(io::Error::other)?;
    builder
        .prepare(
            context.path(),
            task.resources().storage_mb.saturating_mul(BYTES_PER_MIB),
            cache,
            policy,
        )
        .await
}

/// One prepared task root and its guest-visible process configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmEnvironment {
    rootfs: PathBuf,
    workspace: String,
    environment: BTreeMap<String, String>,
    shell: String,
    timezone: String,
    verifier: Option<VmVerifierEnvironment>,
}

impl VmEnvironment {
    /// Creates a prepared task environment.
    pub fn new(
        rootfs: impl Into<PathBuf>,
        workspace: impl Into<String>,
        shell: impl Into<String>,
    ) -> Self {
        Self {
            rootfs: rootfs.into(),
            workspace: workspace.into(),
            environment: BTreeMap::new(),
            shell: shell.into(),
            timezone: DEFAULT_GUEST_TIMEZONE.to_owned(),
            verifier: None,
        }
    }

    /// Sets the complete environment inherited by guest commands.
    #[must_use]
    pub fn environment(mut self, environment: impl IntoIterator<Item = (String, String)>) -> Self {
        self.environment = environment.into_iter().collect();
        self
    }

    /// Sets the guest timezone described to the model.
    #[must_use]
    pub fn timezone(mut self, timezone: impl Into<String>) -> Self {
        self.timezone = timezone.into();
        self
    }

    /// Uses an independent prepared root disk for verification.
    #[must_use]
    pub fn verifier(mut self, verifier: VmVerifierEnvironment) -> Self {
        self.verifier = Some(verifier);
        self
    }

    /// Returns the guest-visible task workspace.
    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// Returns the complete environment for an attempt driver running in this
    /// guest, including task and verifier variables.
    #[must_use]
    pub fn guest_environment(&self, task: &Task) -> BTreeMap<String, String> {
        let mut environment = self.environment.clone();
        environment.extend(base_guest_environment(task, &self.workspace));
        environment
    }
}

/// A separately prepared verifier guest environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmVerifierEnvironment {
    rootfs: PathBuf,
    workspace: String,
    environment: BTreeMap<String, String>,
    shell: String,
}

impl VmVerifierEnvironment {
    /// Creates a prepared verifier environment.
    pub fn new(
        rootfs: impl Into<PathBuf>,
        workspace: impl Into<String>,
        shell: impl Into<String>,
    ) -> Self {
        Self {
            rootfs: rootfs.into(),
            workspace: workspace.into(),
            environment: BTreeMap::new(),
            shell: shell.into(),
        }
    }

    /// Sets the complete environment inherited by verifier commands.
    #[must_use]
    pub fn environment(mut self, environment: impl IntoIterator<Item = (String, String)>) -> Self {
        self.environment = environment.into_iter().collect();
        self
    }
}

/// Immutable VM resources installed into a [`VmBackend`].
pub struct VmBackendConfiguration {
    environments: BTreeMap<PathBuf, VmEnvironment>,
    runtime_image: PathBuf,
    vmm: PathBuf,
    max_guest_memory_mb: Option<u64>,
    gvproxy: Option<PathBuf>,
    verifier_cache: PathBuf,
}

/// Builder for one immutable VM backend configuration.
pub struct VmBackendConfigurationBuilder {
    configuration: VmBackendConfiguration,
}

impl VmBackendConfiguration {
    /// Starts a configuration with the VMM executable and guest-runtime disk.
    pub fn builder(
        vmm: impl Into<PathBuf>,
        runtime_image: impl Into<PathBuf>,
    ) -> VmBackendConfigurationBuilder {
        VmBackendConfigurationBuilder {
            configuration: Self {
                environments: BTreeMap::new(),
                runtime_image: runtime_image.into(),
                vmm: vmm.into(),
                max_guest_memory_mb: None,
                gvproxy: None,
                verifier_cache: PathBuf::from(DEFAULT_VM_CACHE),
            },
        }
    }
}

impl VmBackendConfigurationBuilder {
    /// Adds the prepared environment selected for one task package root.
    #[must_use]
    pub fn environment(
        mut self,
        task_root: impl Into<PathBuf>,
        environment: VmEnvironment,
    ) -> Self {
        self.configuration
            .environments
            .insert(task_root.into(), environment);
        self
    }

    /// Adds every prepared task-root-to-environment mapping.
    #[must_use]
    pub fn environments(
        mut self,
        environments: impl IntoIterator<Item = (PathBuf, VmEnvironment)>,
    ) -> Self {
        self.configuration.environments.extend(environments);
        self
    }

    /// Selects the gvproxy executable used by public-network attempts.
    #[must_use]
    pub fn gvproxy(mut self, binary: impl Into<PathBuf>) -> Self {
        self.configuration.gvproxy = Some(binary.into());
        self
    }

    /// Caps guest RAM for attempts created from this backend.
    #[must_use]
    pub const fn max_guest_memory_mb(mut self, memory_mb: u64) -> Self {
        self.configuration.max_guest_memory_mb = Some(memory_mb);
        self
    }

    /// Selects the persistent verifier dependency-cache directory.
    #[must_use]
    pub fn verifier_cache(mut self, directory: impl Into<PathBuf>) -> Self {
        self.configuration.verifier_cache = directory.into();
        self
    }

    /// Finishes the immutable configuration.
    #[must_use]
    pub fn build(self) -> VmBackendConfiguration {
        self.configuration
    }
}

/// A cloneable attempt factory backed by prepared libkrun environments.
///
/// The backend can be installed before its immutable configuration is known.
/// This lets the evaluator create its durable job directory first; callers
/// must call [`Self::configure`] before admitting attempts.
#[derive(Clone)]
pub struct VmBackend {
    configuration: Arc<OnceLock<VmBackendConfiguration>>,
    retain_passed_rootfs: bool,
    retain_failed_rootfs: bool,
    web_search: bool,
    shared_directories: Arc<[SharedDirectory]>,
    verifier_environment: Arc<BTreeMap<String, String>>,
    force_agent_network: bool,
}

/// Deliberate policy for a [`VmBackend`].
pub struct VmBackendBuilder {
    retain_passed_rootfs: bool,
    retain_failed_rootfs: bool,
    web_search: bool,
    shared_directories: Vec<SharedDirectory>,
    verifier_environment: BTreeMap<String, String>,
    force_agent_network: bool,
}

impl Default for VmBackendBuilder {
    fn default() -> Self {
        Self {
            retain_passed_rootfs: false,
            retain_failed_rootfs: true,
            web_search: false,
            shared_directories: Vec::new(),
            verifier_environment: BTreeMap::new(),
            force_agent_network: false,
        }
    }
}

impl VmBackend {
    /// Starts a VM backend with no prepared resources.
    #[must_use]
    pub fn builder() -> VmBackendBuilder {
        VmBackendBuilder::default()
    }

    /// Installs the immutable resources used by every subsequent attempt.
    ///
    /// # Errors
    ///
    /// Returns [`VmBackendConfigureError::AlreadyConfigured`] when called more
    /// than once.
    pub fn configure(
        &self,
        configuration: VmBackendConfiguration,
    ) -> Result<(), VmBackendConfigureError> {
        self.configuration
            .set(configuration)
            .map_err(|_| VmBackendConfigureError::AlreadyConfigured)
    }

    /// Materializes one fresh attempt and starts its guest tool session.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend is not configured, the task has no
    /// prepared environment, or the attempt environment cannot be created.
    pub(crate) fn attempt(&self, attempt: EvalAttempt<'_>) -> Result<VmAttempt, VmAttemptError> {
        let configuration = self.configuration()?;
        let environment = configuration
            .environments
            .get(attempt.task().root())
            .ok_or_else(|| {
                VmAttemptError::MissingPreparedEnvironment(attempt.task().root().to_path_buf())
            })?;
        vm_attempt(
            environment,
            VmAttemptHost {
                runtime_image: &configuration.runtime_image,
                vmm: &configuration.vmm,
                max_guest_memory_mb: configuration.max_guest_memory_mb,
                gvproxy: configuration.gvproxy.as_deref(),
                verifier_cache: &configuration.verifier_cache,
                retain_passed_rootfs: self.retain_passed_rootfs,
                retain_failed_rootfs: self.retain_failed_rootfs,
                web_search: self.web_search,
                shared_directories: &self.shared_directories,
                verifier_environment: &self.verifier_environment,
                force_agent_network: self.force_agent_network,
            },
            attempt,
        )
    }

    fn configuration(&self) -> Result<&VmBackendConfiguration, VmAttemptError> {
        self.configuration
            .get()
            .ok_or(VmAttemptError::RunResourcesNotPrepared)
    }
}

impl VmBackendBuilder {
    /// Keeps writable root disks for passed attempts.
    #[must_use]
    pub const fn retain_passed_rootfs(mut self, retain: bool) -> Self {
        self.retain_passed_rootfs = retain;
        self
    }

    /// Keeps writable root disks for failed or interrupted attempts.
    #[must_use]
    pub const fn retain_failed_rootfs(mut self, retain: bool) -> Self {
        self.retain_failed_rootfs = retain;
        self
    }

    /// Exposes standalone web search to Nanocodex attempts.
    #[must_use]
    pub const fn web_search(mut self, enabled: bool) -> Self {
        self.web_search = enabled;
        self
    }

    /// Adds a host directory exposed to every guest attempt.
    #[must_use]
    pub fn shared_directory(mut self, directory: SharedDirectory) -> Self {
        self.shared_directories.push(directory);
        self
    }

    /// Adds run-scoped values visible only to verifier commands.
    #[must_use]
    pub fn verifier_environment(
        mut self,
        environment: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        self.verifier_environment.extend(environment);
        self
    }

    /// Attaches the agent VM to the prepared network independently of the task's
    /// public-network policy.
    ///
    /// External harnesses use this only for their evaluator-owned capture proxy;
    /// the verifier continues to follow the benchmark's declared policy.
    #[must_use]
    pub const fn force_agent_network(mut self, enabled: bool) -> Self {
        self.force_agent_network = enabled;
        self
    }

    /// Builds a cloneable backend handle.
    #[must_use]
    pub fn build(self) -> VmBackend {
        VmBackend {
            configuration: Arc::new(OnceLock::new()),
            retain_passed_rootfs: self.retain_passed_rootfs,
            retain_failed_rootfs: self.retain_failed_rootfs,
            web_search: self.web_search,
            shared_directories: self.shared_directories.into(),
            verifier_environment: Arc::new(self.verifier_environment),
            force_agent_network: self.force_agent_network,
        }
    }
}

impl Evaluator {
    /// Starts a VM-backed evaluator builder from a reusable Nanocodex recipe.
    ///
    /// Every attempt receives an independent agent session, disposable host
    /// workspace, and isolated guest environment. The backend also fixes the
    /// durable execution identity to [`EvalEnvironment::MicroVm`].
    #[must_use]
    pub fn builder(nanocodex: NanocodexBuilder, backend: VmBackend) -> EvaluatorBuilder {
        Self::new_builder(nanocodex).vm(backend)
    }
}

impl EvaluatorBuilder {
    /// Runs every evaluator attempt through the configured VM backend.
    ///
    /// This also fixes the durable result environment to
    /// [`EvalEnvironment::MicroVm`].
    #[must_use]
    pub(crate) fn vm(self, backend: VmBackend) -> Self {
        self.vm_with(backend, |_attempt, builder, runtime| {
            runtime.nanocodex(builder)
        })
    }

    /// Runs a custom attempt driver inside the configured VM backend.
    ///
    /// The factory receives the immutable attempt metadata, fresh Nanocodex
    /// recipe, and materialized VM attempt. Stock-Codex harness adapters
    /// use this boundary to execute a guest binary while retaining the same
    /// evaluator-owned verifier and cleanup lifecycle.
    ///
    /// This also fixes the durable result environment to
    /// [`EvalEnvironment::MicroVm`].
    #[must_use]
    pub(crate) fn vm_with<F>(self, backend: VmBackend, factory: F) -> Self
    where
        F: for<'a> Fn(
                EvalAttempt<'a>,
                NanocodexBuilder,
                VmAttempt,
            ) -> Result<AttemptAgent, VmAttemptError>
            + Send
            + Sync
            + 'static,
    {
        self.attempt_environment(EvalEnvironment::MicroVm)
            .attempt_agent(move |attempt, builder| {
                let runtime = backend.attempt(attempt)?;
                factory(attempt, builder, runtime)
            })
    }
}

/// Failure to install immutable resources into a VM backend more than once.
#[derive(Debug, thiserror::Error)]
pub enum VmBackendConfigureError {
    /// The backend already has an immutable configuration.
    #[error("VM backend resources were already configured")]
    AlreadyConfigured,
}

#[derive(Clone, Copy)]
struct VmAttemptHost<'a> {
    runtime_image: &'a Path,
    vmm: &'a Path,
    max_guest_memory_mb: Option<u64>,
    gvproxy: Option<&'a Path>,
    verifier_cache: &'a Path,
    retain_passed_rootfs: bool,
    retain_failed_rootfs: bool,
    web_search: bool,
    shared_directories: &'a [SharedDirectory],
    verifier_environment: &'a Arc<BTreeMap<String, String>>,
    force_agent_network: bool,
}

struct AttemptGvproxy {
    process: GvproxyProcess,
    _directory: TempDir,
}

impl AttemptGvproxy {
    fn spawn(binary: &Path, log: &Path) -> Result<Self, VmAttemptError> {
        Self::spawn_with(binary, log, GvproxyProcess::spawn)
    }

    #[cfg(target_os = "linux")]
    fn spawn_capture_only(
        binary: &Path,
        wrapper: &Path,
        port: u16,
        log: &Path,
    ) -> Result<Self, VmAttemptError> {
        Self::spawn_with(binary, log, |binary, state_directory, log| {
            GvproxyProcess::spawn_capture_only(binary, state_directory, log, wrapper, port)
        })
    }

    fn spawn_with(
        binary: &Path,
        log: &Path,
        spawn: impl FnOnce(&Path, &Path, &Path) -> Result<GvproxyProcess, VmGvproxyError>,
    ) -> Result<Self, VmAttemptError> {
        // gvproxy uses filesystem Unix sockets, so keep their paths below the
        // platform limit even when the caller gives workers a long TMPDIR.
        let directory = tempfile::Builder::new()
            .prefix("ncx-gvp-")
            .tempdir_in("/tmp")?;
        let process = spawn(binary, directory.path(), log)?;
        Ok(Self {
            process,
            _directory: directory,
        })
    }

    fn socket(&self) -> &Path {
        self.process.network_socket()
    }
}

// The attempt lifecycle below is intentionally private except for
// `VmAttempt`. Its public methods expose only the agent/verifier composition
// needed by Nanocodex and external harness evaluator arms.
/// Failure to configure, materialize, execute, verify, or clean up a VM attempt.
#[derive(Debug, thiserror::Error)]
pub enum VmAttemptError {
    /// The evaluator admitted an attempt before immutable VM resources were installed.
    #[error("VM run resources were not prepared before attempt admission")]
    RunResourcesNotPrepared,

    /// No prepared environment was registered for a task package root.
    #[error("no VM environment was prepared for task root {0}")]
    MissingPreparedEnvironment(PathBuf),

    /// The owned agent guest session was already consumed.
    #[error("the agent VM session was already finished")]
    AgentSessionAlreadyFinished,

    /// A directory-backed rootfs template was expected.
    #[error("rootfs template is not a directory: {0}")]
    InvalidRootfs(PathBuf),

    /// The prepared guest-runtime artifact was not a regular file.
    #[error("rootfs template does not contain the guest tool runtime: {0}")]
    MissingGuestRuntime(PathBuf),

    /// A public-network task was configured without a gvproxy executable.
    #[error("the task requires public networking but gvproxy was not prepared")]
    NetworkBackendNotPrepared,

    /// Capture-only external harness networking requires Linux Landlock.
    #[error("capture-only external harness networking is only supported on Linux hosts")]
    CaptureOnlyNetworkUnsupported,

    /// Materializing the task root would overwrite attempt-owned data.
    #[error("rootfs entry collides with attempt data: {0}")]
    Collision(PathBuf),

    /// Host filesystem or subprocess I/O failed.
    #[error(transparent)]
    Io(#[from] io::Error),

    /// The typed guest tool session failed.
    #[error(transparent)]
    Session(#[from] VmToolSessionError),

    /// The attempt tool registry could not be built.
    #[error(transparent)]
    Tools(#[from] ToolsBuildError),

    /// The task package changed or could not be materialized.
    #[error(transparent)]
    TaskPackage(#[from] TaskLoadError),

    /// A verifier reward was not a valid floating-point number.
    #[error(transparent)]
    ParseReward(#[from] ParseFloatError),

    /// A verifier-cache ext4 image could not be created.
    #[error(transparent)]
    Ext4(#[from] arcbox_ext4::error::FormatError),

    /// A sparse writable guest OverlayFS layer could not be created.
    #[error(transparent)]
    OverlayDisk(#[from] OverlayDiskError),

    /// The isolated userspace network process failed.
    #[error(transparent)]
    Network(#[from] VmGvproxyError),
}

/// One materialized VM attempt with its guest session and owned verifier.
pub(crate) struct VmAttempt {
    tools: Tools,
    timezone: String,
    verifier: VmVerifier,
    capture_listener: Option<TcpListener>,
}

impl VmAttempt {
    pub(crate) const fn take_capture_listener(&mut self) -> Option<TcpListener> {
        self.capture_listener.take()
    }

    /// Returns a cheap handle for guest commands used by a custom attempt driver.
    ///
    /// # Errors
    ///
    /// Returns an error after the owned guest session has been consumed.
    pub(crate) fn session_handle(&self) -> Result<VmToolSessionHandle, VmAttemptError> {
        self.verifier
            .agent_session
            .as_ref()
            .ok_or(VmAttemptError::AgentSessionAlreadyFinished)
            .map(VmToolSession::handle)
    }

    /// Attaches the guest tools, readiness handshake, and verifier to Nanocodex.
    ///
    /// # Errors
    ///
    /// Returns an error after the owned guest session has been consumed.
    pub(crate) fn nanocodex(
        self,
        builder: NanocodexBuilder,
    ) -> Result<AttemptAgent, VmAttemptError> {
        let readiness = self.session_handle()?;
        let context_session = readiness.clone();
        let guest_workspace = self.verifier.launch.workspace.clone();
        let current_date = current_date(&self.timezone);
        let timezone = self.timezone;
        let builder = builder.tools(self.tools);
        Ok(AttemptAgent::preparing_nanocodex(async move {
            let project_instructions =
                load_guest_project_instructions(&context_session, &guest_workspace).await?;
            let mut environment = ExecutionEnvironment::new(current_date, timezone);
            if let Some(instructions) = project_instructions {
                environment = environment.project_instructions(instructions);
            }
            Ok::<_, VmAttemptError>(builder.execution_environment(environment))
        })
        .ready(async move { readiness.ready().await })
        .verifier(self.verifier))
    }

    /// Attaches the owned VM verifier to a external harness attempt driver.
    #[must_use]
    pub(crate) fn harness(self, harness: HarnessExec) -> AttemptAgent {
        AttemptAgent::harness(harness).verifier(self.verifier)
    }
}

async fn load_guest_project_instructions(
    session: &VmToolSessionHandle,
    workspace: &str,
) -> Result<Option<String>, VmAttemptError> {
    let discovery = session
        .command(
            VmCommand::new("/bin/sh")
                .arg("-c")
                .arg(GUEST_PROJECT_INSTRUCTION_PATHS_SCRIPT)
                .arg("nanocodex-agents-md")
                .arg(workspace)
                .current_directory(workspace)
                .timeout(GUEST_PROJECT_INSTRUCTIONS_TIMEOUT)
                .max_output_bytes(GUEST_PROJECT_INSTRUCTION_PATHS_MAX_BYTES),
        )
        .await?;
    if discovery.exit_code != 0 {
        return Err(io::Error::other(format!(
            "guest AGENTS.md discovery exited {}: {}",
            discovery.exit_code,
            String::from_utf8_lossy(&discovery.stderr).trim()
        ))
        .into());
    }

    let mut paths = discovery
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            std::str::from_utf8(path)
                .map(str::to_owned)
                .map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("guest AGENTS.md path was not UTF-8: {error}"),
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.reverse();

    let mut remaining = GUEST_PROJECT_INSTRUCTIONS_MAX_BYTES;
    let mut documents = Vec::new();
    for path in paths {
        if remaining == 0 {
            break;
        }
        let mut contents = session.read_file(&path).await?;
        let truncated = contents.len() > remaining;
        contents.truncate(remaining);
        let included_bytes = contents.len();
        if truncated {
            warn!(
                path,
                remaining_bytes = remaining,
                "guest project doc exceeds remaining budget; truncating"
            );
        }
        let contents = String::from_utf8_lossy(&contents).into_owned();
        if !contents.trim().is_empty() {
            remaining = remaining.saturating_sub(included_bytes);
            documents.push(contents);
        }
    }
    Ok((!documents.is_empty()).then(|| documents.join("\n\n")))
}

struct VmVerifier {
    agent_session: Option<VmToolSession>,
    launch: VmLaunch,
    separate_launch: Option<VmLaunch>,
    cache: Option<VerifierCache>,
    attempt_cache: Option<AttemptVerifierCache>,
    retain_passed_rootfs: bool,
    retain_failed_rootfs: bool,
    root_disks_finalized: bool,
    artifact_directory: PathBuf,
    verifier_environment: Arc<BTreeMap<String, String>>,
    _network: Option<AttemptGvproxy>,
    _verifier_network: Option<AttemptGvproxy>,
}

struct VmAttemptSetupGuard {
    root_disks: Vec<PathBuf>,
    attempt_cache: Option<PathBuf>,
    retain_failed_rootfs: bool,
    armed: bool,
}

#[derive(Clone)]
struct VmLaunch {
    root: VmLaunchRoot,
    workspace: String,
    shell: String,
    runtime_image: PathBuf,
    vmm: PathBuf,
    cpus: u32,
    memory_mib: u64,
    resolver_configuration: String,
    environment: BTreeMap<String, String>,
    network_socket: Option<PathBuf>,
    shared_directories: Vec<SharedDirectory>,
}

#[derive(Clone)]
enum VmLaunchRoot {
    Directory(PathBuf),
    Ext4(PathBuf),
    OverlayExt4 { lower: PathBuf, upper: PathBuf },
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum AttemptRootPolicy {
    Retainable,
    DisposableOverlay,
}

struct VerifierCache {
    root: PathBuf,
    key: String,
    status: &'static str,
    cacheable_start: usize,
    cacheable_end: usize,
    skip_setup: bool,
    disk_bytes: u64,
}

struct AttemptVerifierCache {
    disk: PathBuf,
    skip_setup: bool,
}

fn vm_attempt(
    environment: &VmEnvironment,
    host: VmAttemptHost<'_>,
    attempt: EvalAttempt<'_>,
) -> Result<VmAttempt, VmAttemptError> {
    let guest_memory_mb = effective_guest_memory_mb(
        attempt.task().resources().memory_mb,
        host.max_guest_memory_mb,
    );
    let span = info_span!(
        target: "nanocodex_eval",
        "vm.attempt.setup",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        eval.task.name = attempt.task().name(),
        vm.rootfs.template = %environment.rootfs.display(),
        vm.rootfs.destination = %attempt.directory().display(),
        vm.cpu.count = attempt.task().resources().cpus,
        vm.memory.declared_mib = attempt.task().resources().memory_mb,
        vm.memory_mib = guest_memory_mb,
        status = tracing::field::Empty,
        error.message = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
    );
    let started_at = Instant::now();
    let result = span.in_scope(|| vm_attempt_inner(environment, host, attempt));
    record_operation(&span, started_at, &result);
    result
}

fn vm_attempt_inner(
    environment: &VmEnvironment,
    host: VmAttemptHost<'_>,
    attempt: EvalAttempt<'_>,
) -> Result<VmAttempt, VmAttemptError> {
    attempt.task().validate_package()?;
    let template = &environment.rootfs;
    let verifier_cache = if environment.verifier.is_some() {
        None
    } else {
        prepare_verifier_cache(template, attempt.task(), host.verifier_cache)?
    };
    let root_policy = if host.retain_passed_rootfs || host.retain_failed_rootfs {
        AttemptRootPolicy::Retainable
    } else {
        AttemptRootPolicy::DisposableOverlay
    };
    let root = materialize_attempt_root(
        template,
        host.runtime_image,
        attempt.directory(),
        "rootfs",
        root_policy,
    )?;
    let mut setup_guard = VmAttemptSetupGuard::new(host.retain_failed_rootfs);
    if let Some(disk) = root.writable_disk() {
        setup_guard.track_root_disk(disk.to_path_buf());
    }
    let agent_network = if host.force_agent_network {
        NetworkPolicy::Public
    } else {
        attempt.task().network()
    };
    let capture_listener = (host.force_agent_network
        && attempt.task().network() == NetworkPolicy::Disabled)
        .then(|| TcpListener::bind((Ipv4Addr::LOCALHOST, 0)))
        .transpose()?;
    let network_log = attempt.directory().join("vm").join("gvproxy.log");
    let network = if let Some(listener) = &capture_listener {
        spawn_capture_only_attempt_network(
            host.gvproxy,
            host.vmm,
            listener.local_addr()?.port(),
            &network_log,
        )?
    } else {
        spawn_attempt_network(agent_network, host.gvproxy, &network_log)?
    };
    let launch = VmLaunch {
        root,
        workspace: environment.workspace.clone(),
        shell: environment.shell.clone(),
        runtime_image: host.runtime_image.to_path_buf(),
        vmm: host.vmm.to_path_buf(),
        cpus: attempt.task().resources().cpus.clamp(1, u32::from(u8::MAX)),
        memory_mib: effective_guest_memory_mb(
            attempt.task().resources().memory_mb,
            host.max_guest_memory_mb,
        ),
        resolver_configuration: network
            .as_ref()
            .map_or_else(String::new, |_| GUEST_PUBLIC_RESOLV_CONF.to_owned()),
        environment: environment.environment.clone(),
        network_socket: network
            .as_ref()
            .map(|network| network.socket().to_path_buf()),
        shared_directories: host.shared_directories.to_vec(),
    };
    let verifier_network = if environment.verifier.is_some() {
        spawn_attempt_network(
            attempt.task().verifier().network(),
            host.gvproxy,
            &attempt.directory().join("verifier-vm").join("gvproxy.log"),
        )?
    } else {
        None
    };
    let separate_launch = prepare_separate_verifier_launch(
        environment,
        host,
        attempt,
        root_policy,
        verifier_network.as_ref(),
    )?;
    if let Some(separate) = &separate_launch
        && let Some(disk) = separate.root.writable_disk()
    {
        setup_guard.track_root_disk(disk.to_path_buf());
    }
    let verifier_directory = attempt.directory().join("verifier");
    fs::create_dir_all(&verifier_directory)?;
    let attempt_cache = verifier_cache
        .as_ref()
        .map(|cache| cache.materialize(&verifier_directory))
        .transpose()?;
    if let Some(cache) = &attempt_cache {
        setup_guard.track_attempt_cache(cache.disk.clone());
    }
    let session = launch.spawn(attempt_cache.as_ref())?;
    let vm = session.tools();
    let tools = Tools::builder()
        .without_defaults()
        .web_search(host.web_search)
        .image_generation(true)
        .working_directory(environment.workspace.clone())
        .default_shell(environment.shell.as_str())
        .tool(vm.exec_command_tool())
        .tool(vm.write_stdin_tool())
        .tool(vm.apply_patch_tool())
        .tool(vm.view_image_tool())
        .tool(UpdatePlanTool::new())
        .build()
        .map_err(VmAttemptError::from)?;
    let verifier = VmVerifier {
        agent_session: Some(session),
        launch,
        separate_launch,
        cache: verifier_cache,
        attempt_cache,
        retain_passed_rootfs: host.retain_passed_rootfs,
        retain_failed_rootfs: host.retain_failed_rootfs,
        root_disks_finalized: false,
        artifact_directory: attempt.directory().to_path_buf(),
        verifier_environment: Arc::clone(host.verifier_environment),
        _network: network,
        _verifier_network: verifier_network,
    };
    setup_guard.disarm();
    Ok(VmAttempt {
        tools,
        timezone: environment.timezone.clone(),
        verifier,
        capture_listener,
    })
}

#[cfg(target_os = "linux")]
fn spawn_capture_only_attempt_network(
    gvproxy: Option<&Path>,
    vmm: &Path,
    port: u16,
    log: &Path,
) -> Result<Option<AttemptGvproxy>, VmAttemptError> {
    let binary = gvproxy.ok_or(VmAttemptError::NetworkBackendNotPrepared)?;
    let wrapper = vmm.with_file_name("nanocodex-vm-guest");
    AttemptGvproxy::spawn_capture_only(binary, &wrapper, port, log).map(Some)
}

#[cfg(target_os = "macos")]
const fn spawn_capture_only_attempt_network(
    _gvproxy: Option<&Path>,
    _vmm: &Path,
    _port: u16,
    _log: &Path,
) -> Result<Option<AttemptGvproxy>, VmAttemptError> {
    Err(VmAttemptError::CaptureOnlyNetworkUnsupported)
}

impl VmAttemptSetupGuard {
    const fn new(retain_failed_rootfs: bool) -> Self {
        Self {
            root_disks: Vec::new(),
            attempt_cache: None,
            retain_failed_rootfs,
            armed: true,
        }
    }

    fn track_root_disk(&mut self, path: PathBuf) {
        self.root_disks.push(path);
    }

    fn track_attempt_cache(&mut self, path: PathBuf) {
        self.attempt_cache = Some(path);
    }

    const fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for VmAttemptSetupGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(path) = &self.attempt_cache {
            let _ = fs::remove_file(path);
        }
        if !self.retain_failed_rootfs {
            for path in &self.root_disks {
                let _ = remove_rootfs(path);
            }
        }
    }
}

fn materialize_attempt_root(
    template: &Path,
    runtime_image: &Path,
    attempt_directory: &Path,
    disk_stem: &str,
    policy: AttemptRootPolicy,
) -> Result<VmLaunchRoot, VmAttemptError> {
    if template.is_file() {
        if !runtime_image.is_file() {
            return Err(VmAttemptError::MissingGuestRuntime(
                runtime_image.to_path_buf(),
            ));
        }
        return match policy {
            AttemptRootPolicy::Retainable => {
                let root = attempt_directory.join(format!("{disk_stem}.ext4"));
                reflink_or_sparse_copy(template, &root)?;
                Ok(VmLaunchRoot::Ext4(root))
            }
            AttemptRootPolicy::DisposableOverlay => {
                let upper = attempt_directory.join(format!("{disk_stem}.upper.ext4"));
                create_sparse_overlay_disk(&upper, fs::metadata(template)?.len())?;
                Ok(VmLaunchRoot::OverlayExt4 {
                    lower: template.to_path_buf(),
                    upper,
                })
            }
        };
    }

    if !runtime_image.is_file() {
        return Err(VmAttemptError::MissingGuestRuntime(
            runtime_image.to_path_buf(),
        ));
    }
    let span = info_span!(
        target: "nanocodex_eval",
        "vm.rootfs.materialize",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        source = %template.display(),
        destination = %attempt_directory.display(),
        status = tracing::field::Empty,
        error.message = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
    );
    let started_at = Instant::now();
    let result = span.in_scope(|| materialize_rootfs(template, attempt_directory));
    record_operation(&span, started_at, &result);
    result?;
    let guest_runtime = attempt_directory.join(EMBEDDED_GUEST_TOOL_RUNTIME.trim_start_matches('/'));
    let guest_parent = guest_runtime
        .parent()
        .ok_or_else(|| VmAttemptError::Collision(guest_runtime.clone()))?;
    let attempt_root = fs::canonicalize(attempt_directory)?;
    let guest_parent = fs::canonicalize(guest_parent)?;
    if !guest_parent.starts_with(&attempt_root) {
        return Err(VmAttemptError::Collision(guest_parent));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&guest_parent)?;
    io::copy(&mut fs::File::open(runtime_image)?, &mut temporary)?;
    #[cfg(unix)]
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o755))?;
    temporary
        .persist(&guest_runtime)
        .map_err(|error| error.error)?;
    Ok(VmLaunchRoot::Directory(attempt_directory.to_path_buf()))
}

fn prepare_separate_verifier_launch(
    environment: &VmEnvironment,
    host: VmAttemptHost<'_>,
    attempt: EvalAttempt<'_>,
    root_policy: AttemptRootPolicy,
    network: Option<&AttemptGvproxy>,
) -> Result<Option<VmLaunch>, VmAttemptError> {
    environment
        .verifier
        .as_ref()
        .map(|verifier| {
            let root = materialize_attempt_root(
                &verifier.rootfs,
                host.runtime_image,
                attempt.directory(),
                "verifier-rootfs",
                root_policy,
            )?;
            Ok(VmLaunch {
                root,
                workspace: verifier.workspace.clone(),
                shell: verifier.shell.clone(),
                runtime_image: host.runtime_image.to_path_buf(),
                vmm: host.vmm.to_path_buf(),
                cpus: attempt.task().resources().cpus.clamp(1, u32::from(u8::MAX)),
                memory_mib: effective_guest_memory_mb(
                    attempt.task().resources().memory_mb,
                    host.max_guest_memory_mb,
                ),
                resolver_configuration: network
                    .map_or_else(String::new, |_| GUEST_PUBLIC_RESOLV_CONF.to_owned()),
                environment: verifier.environment.clone(),
                network_socket: network.map(|network| network.socket().to_path_buf()),
                shared_directories: Vec::new(),
            })
        })
        .transpose()
}

fn prepare_verifier_cache(
    template: &Path,
    task: &Task,
    cache: &Path,
) -> Result<Option<VerifierCache>, VmAttemptError> {
    let prepared = template
        .is_file()
        .then(|| VerifierCache::prepare(template, task, cache))
        .transpose()
        .map(Option::flatten)?;
    let Some(prepared) = prepared else {
        return Ok(None);
    };
    if prepared.status == "hit" {
        return Ok(Some(prepared));
    }
    info!(
        target: "nanocodex_eval",
        task_name = task.name(),
        verifier_cache_key = prepared.key,
        "running the canonical cold verifier without a cache disk"
    );
    Ok(None)
}

fn spawn_attempt_network(
    policy: NetworkPolicy,
    gvproxy: Option<&Path>,
    log: &Path,
) -> Result<Option<AttemptGvproxy>, VmAttemptError> {
    match policy {
        NetworkPolicy::Public => {
            let binary = gvproxy.ok_or(VmAttemptError::NetworkBackendNotPrepared)?;
            AttemptGvproxy::spawn(binary, log).map(Some)
        }
        NetworkPolicy::Disabled => Ok(None),
    }
}

impl VmLaunch {
    fn spawn(
        &self,
        verifier_cache: Option<&AttemptVerifierCache>,
    ) -> Result<VmToolSession, VmAttemptError> {
        let mut command = Command::new(&self.vmm);
        nanocodex_vm::terminate_child_with_parent(command.as_std_mut());
        // libkrun creates its gvproxy client socket beneath TMPDIR. Worker
        // scratch paths are intentionally private but can exceed Unix socket
        // limits, so give only the VMM process the platform's short temp root.
        command.env("TMPDIR", "/tmp");
        let firmware = Path::new(DEFAULT_KRUNFW_DIRECTORY);
        if firmware.join(KRUNFW_LIBRARY_FILENAME).is_file() {
            command.env(KRUNFW_LIBRARY_PATH_ENVIRONMENT, firmware.canonicalize()?);
        }
        command.args(["vm-run-config", "--config"]);

        let network = if let Some(socket) = &self.network_socket {
            Network::gvproxy(socket)
        } else {
            Network::Disabled
        };
        let mut vm = match &self.root {
            VmLaunchRoot::Directory(root) => VmConfig::new(root),
            VmLaunchRoot::Ext4(root) => VmConfig::ext4(root),
            VmLaunchRoot::OverlayExt4 { lower, upper } => {
                VmConfig::overlay_ext4(&self.runtime_image, lower, upper)
            }
        }
        .cpus(u8::try_from(self.cpus).unwrap_or(u8::MAX))
        .memory_mib(u32::try_from(self.memory_mib).unwrap_or(u32::MAX))
        .network(network);
        for directory in &self.shared_directories {
            vm = vm.shared_directory(directory.clone());
        }
        if matches!(self.root, VmLaunchRoot::Ext4(_)) {
            vm = vm.block_device(BlockDevice::read_only(
                GUEST_RUNTIME_BLOCK_ID,
                &self.runtime_image,
            ));
        }
        if !matches!(self.root, VmLaunchRoot::Directory(_))
            && let Some(cache) = verifier_cache
        {
            vm = vm.block_device(BlockDevice::read_write(
                VERIFIER_CACHE_BLOCK_ID,
                &cache.disk,
            ));
        }

        let mut guest = match &self.root {
            VmLaunchRoot::Directory(_) => {
                GuestCommand::new(EMBEDDED_GUEST_TOOL_RUNTIME).arg(&self.workspace)
            }
            VmLaunchRoot::Ext4(_) => {
                GuestCommand::new("/bin/sh")
                    .arg("-c")
                    .arg(vm_guest_bootstrap_script(
                        &self.workspace,
                        &self.resolver_configuration,
                    ))
            }
            VmLaunchRoot::OverlayExt4 { .. } => {
                overlay_guest_command(&self.workspace, &self.resolver_configuration)
            }
        };
        for (name, value) in &self.environment {
            guest = guest.env(name, value);
        }
        VmToolSession::spawn_vm(command, vm, guest).map_err(Into::into)
    }

    const fn verifier_cache_block_device(&self) -> &'static str {
        match self.root {
            VmLaunchRoot::OverlayExt4 { .. } => OVERLAY_VERIFIER_CACHE_BLOCK_DEVICE,
            VmLaunchRoot::Directory(_) | VmLaunchRoot::Ext4(_) => VERIFIER_CACHE_BLOCK_DEVICE,
        }
    }
}

impl VmLaunchRoot {
    fn writable_disk(&self) -> Option<&Path> {
        match self {
            Self::Directory(_) => None,
            Self::Ext4(root) => Some(root),
            Self::OverlayExt4 { upper, .. } => Some(upper),
        }
    }
}

fn vm_guest_bootstrap_script(workspace: &str, resolver_configuration: &str) -> String {
    let workspace = shell_word_without_double_quotes(workspace);
    let resolver_configuration = shell_word_without_double_quotes(resolver_configuration);
    format!(
        "set -eu; rm -f /etc/resolv.conf; printf %b {resolver_configuration} > /etc/resolv.conf; \
         mkdir -p -- {workspace} /logs/verifier {GUEST_RUNTIME_MOUNT}; \
         mount -t ext4 -o ro {GUEST_RUNTIME_BLOCK_DEVICE} {GUEST_RUNTIME_MOUNT}; \
         exec {BLOCK_GUEST_TOOL_RUNTIME} {workspace}"
    )
}

fn shell_word_without_double_quotes(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len().saturating_add(2));
    quoted.push('\'');
    for character in value.chars() {
        match character {
            '\'' => quoted.push_str("'\\''"),
            // libkrun cannot carry a literal double quote in an argv entry.
            // Synthesize it only after the wrapper shell starts.
            '"' => quoted.push_str("'$(printf '\\042')'"),
            character => quoted.push(character),
        }
    }
    quoted.push('\'');
    quoted
}

impl VerifierCache {
    fn prepare(template: &Path, task: &Task, cache: &Path) -> Result<Option<Self>, VmAttemptError> {
        let script = task.verifier_script_bytes()?;
        let Some(setup) = recognized_verifier_setup(&script) else {
            info!(
                target: "nanocodex_eval",
                task_name = task.name(),
                verifier_cache_status = "unsupported",
                "canonical verifier will use the cold dependency path"
            );
            return Ok(None);
        };
        let template_identity = template
            .file_name()
            .ok_or_else(|| io::Error::other("VM root disk template has no file name"))?;
        let disk_bytes = task
            .resources()
            .storage_mb
            .saturating_mul(1024 * 1024)
            .clamp(
                MINIMUM_VERIFIER_CACHE_DISK_BYTES,
                MAXIMUM_VERIFIER_CACHE_DISK_BYTES,
            );
        let key = verifier_cache_key(
            template_identity,
            &script[setup.cacheable_start..setup.cacheable_end],
            disk_bytes,
        );
        let root = cache.join("verifiers").join(&key);
        let disk = root.join("cache.ext4");
        let status = if disk.is_file() && verifier_cache_populated(&disk)? {
            "hit"
        } else {
            "miss"
        };
        info!(
            target: "nanocodex_eval",
            task_name = task.name(),
            verifier_cache_key = key,
            verifier_cache_status = status,
            verifier_cache_path = %root.display(),
            "post-agent verifier dependency cache ready"
        );
        Ok(Some(Self {
            root,
            key,
            status,
            cacheable_start: setup.cacheable_start,
            cacheable_end: setup.cacheable_end,
            skip_setup: setup.skip_setup,
            disk_bytes,
        }))
    }

    fn materialize(
        &self,
        verifier_directory: &Path,
    ) -> Result<AttemptVerifierCache, VmAttemptError> {
        let disk = verifier_directory.join("cache.ext4");
        let hit = self.is_ready()?;
        if hit {
            reflink_or_sparse_copy(&self.root.join("cache.ext4"), &disk)?;
        } else {
            format_verifier_cache_disk(&disk, self.disk_bytes)?;
        }
        Ok(AttemptVerifierCache {
            disk,
            skip_setup: hit && self.skip_setup,
        })
    }

    fn is_ready(&self) -> io::Result<bool> {
        let disk = self.root.join("cache.ext4");
        Ok(disk.is_file() && verifier_cache_populated(&disk)?)
    }

    fn mark_ready(&self, attempt: &AttemptVerifierCache) -> io::Result<bool> {
        if attempt.skip_setup || !verifier_cache_populated(&attempt.disk)? {
            return Ok(false);
        }
        fs::create_dir_all(&self.root)?;
        let target = self.root.join("cache.ext4");
        let mut identity = Sha256::new();
        identity.update(attempt.disk.as_os_str().as_encoded_bytes());
        let temporary = self
            .root
            .join(format!("cache.{}.tmp", hex::encode(identity.finalize())));
        reflink_or_sparse_copy(&attempt.disk, &temporary)?;
        match fs::hard_link(&temporary, &target) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                fs::remove_file(&temporary)?;
                return Err(error);
            }
        }
        fs::remove_file(temporary)?;
        Ok(true)
    }
}

fn verifier_cache_key(
    template_identity: &OsStr,
    cacheable_script: &[u8],
    disk_bytes: u64,
) -> String {
    let mut digest = Sha256::new();
    digest.update(VERIFIER_CACHE_VERSION.to_le_bytes());
    digest.update(VM_GUEST_TARGET.as_bytes());
    digest.update(template_identity.as_encoded_bytes());
    digest.update(cacheable_script);
    digest.update(disk_bytes.to_le_bytes());
    hex::encode(digest.finalize())
}

fn format_verifier_cache_disk(path: &Path, disk_bytes: u64) -> Result<(), VmAttemptError> {
    let mut formatter = Formatter::new(path, 4_096, disk_bytes)?;
    for directory in ["apt-archives", "apt-lists", "uv-cache", "uv-home"] {
        formatter.create(
            &format!("/{directory}"),
            make_mode(file_mode::S_IFDIR, 0o755),
            None,
            None,
            None,
            Some(0),
            Some(0),
            None,
        )?;
    }
    formatter.close()?;
    Ok(())
}

fn verifier_cache_populated(disk: &Path) -> io::Result<bool> {
    let mut reader = Reader::new(disk).map_err(io::Error::other)?;
    Ok(reader.exists("/uv-home/bin/env") && reader.exists("/uv-home/bin/uv"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RecognizedVerifierSetup {
    cacheable_start: usize,
    cacheable_end: usize,
    skip_setup: bool,
}

fn recognized_verifier_setup(script: &[u8]) -> Option<RecognizedVerifierSetup> {
    let script = std::str::from_utf8(script).ok()?;
    let marker = script.find(VERIFIER_SETUP_MARKER)?;
    let setup = &script[..marker];
    let commands = setup
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>();
    let canonical = [
        "apt-get update",
        "apt-get install -y curl",
        "curl -LsSf https://astral.sh/uv/0.9.5/install.sh | sh",
        "source $HOME/.local/bin/env",
    ];
    let has_pinned_uv_bootstrap = commands
        .windows(2)
        .any(|commands| commands == &canonical[2..]);
    if !has_pinned_uv_bootstrap {
        return None;
    }
    let cacheable_start = script
        .strip_prefix("#!")
        .and_then(|script| script.find('\n'))
        .map_or(0, |offset| offset + 3);
    Some(RecognizedVerifierSetup {
        cacheable_start,
        cacheable_end: marker,
        skip_setup: commands == canonical,
    })
}

fn cached_verifier_script(script: &[u8], setup: RecognizedVerifierSetup) -> Vec<u8> {
    let mut cached = Vec::with_capacity(script.len());
    cached.extend_from_slice(&script[..setup.cacheable_start]);
    cached.extend_from_slice(b"\nsource /root/.local/bin/env\n");
    cached.extend_from_slice(&script[setup.cacheable_end..]);
    cached
}

fn verifier_bootstrap_network_failed(output: &VmCommandOutput) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let contains = |needle: &str| stdout.contains(needle) || stderr.contains(needle);
    let dependency_runner_missing = contains("uvx: command not found")
        || contains("/root/.local/bin/env: No such file or directory");
    let dns_failed = contains("Temporary failure resolving") || contains("Could not resolve host");
    let network_failed = dns_failed
        || contains("failed to download https://github.com/astral-sh/uv/")
        || contains("The requested URL returned error: 502")
        || contains("The requested URL returned error: 503")
        || contains("The requested URL returned error: 504");
    let apt_bootstrap_failed = dns_failed
        && (contains("deb.debian.org")
            || contains("archive.ubuntu.com")
            || contains("security.ubuntu.com"));
    apt_bootstrap_failed || dependency_runner_missing && network_failed
}

async fn read_verifier_rewards(
    session: &VmToolSession,
) -> Result<(&'static str, Vec<u8>, BTreeMap<String, f64>), VmAttemptError> {
    if let Ok(bytes) = session.read_file("/logs/verifier/reward.json").await {
        let rewards = serde_json::from_slice::<BTreeMap<String, f64>>(&bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid verifier reward.json: {error}"),
            )
        })?;
        validate_verifier_rewards(&rewards)?;
        return Ok(("reward.json", bytes, rewards));
    }
    let bytes = session.read_file("/logs/verifier/reward.txt").await?;
    let reward = String::from_utf8_lossy(&bytes).trim().parse::<f64>()?;
    let rewards = BTreeMap::from([("reward".to_owned(), reward)]);
    validate_verifier_rewards(&rewards)?;
    Ok(("reward.txt", bytes, rewards))
}

fn validate_verifier_rewards(rewards: &BTreeMap<String, f64>) -> Result<(), VmAttemptError> {
    if rewards.is_empty()
        || rewards
            .iter()
            .any(|(name, reward)| name.trim().is_empty() || !reward.is_finite())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "verifier rewards must contain non-empty names and finite numeric values",
        )
        .into());
    }
    Ok(())
}

impl AttemptVerifier for VmVerifier {
    fn verify<'a>(
        &'a mut self,
        task: &'a Task,
        attempt: EvalAttempt<'a>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<AttemptVerification, AttemptVerificationFailure>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move { self.verify_inner(task, attempt).await })
    }

    fn shutdown(&mut self) -> Pin<Box<dyn Future<Output = CleanupPhase> + Send + '_>> {
        Box::pin(async move { self.shutdown_before_verification().await })
    }
}

impl VmVerifier {
    async fn stage_verifier_logs(
        session: &VmToolSession,
        destination: &Path,
    ) -> Result<(), VmAttemptError> {
        let listed = session
            .command(
                VmCommand::new("/bin/sh")
                    .arg("-c")
                    .arg("find /logs/verifier -type f -printf '%P\\0'")
                    .max_output_bytes(1024 * 1024)
                    .timeout(Duration::from_secs(30)),
            )
            .await?;
        if listed.exit_code != 0 {
            return Err(io::Error::other(format!(
                "listing verifier evidence exited {}: {}",
                listed.exit_code,
                String::from_utf8_lossy(&listed.stderr)
            ))
            .into());
        }
        for encoded in listed
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let relative = std::str::from_utf8(encoded)
                .map_err(|_| io::Error::other("verifier evidence path is not UTF-8"))?;
            let relative = Path::new(relative);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                return Err(io::Error::other(format!(
                    "verifier evidence path is unsafe: {}",
                    relative.display()
                ))
                .into());
            }
            let target = destination.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let guest = Path::new("/logs/verifier")
                .join(relative)
                .to_string_lossy()
                .into_owned();
            fs::write(target, session.read_file(guest).await?)?;
        }
        Ok(())
    }

    async fn run_pre_artifacts(
        &self,
        session: &VmToolSession,
        task: &Task,
        launch: &VmLaunch,
    ) -> Result<(), VmAttemptError> {
        let Some(script) = task.pre_artifacts_script_bytes()? else {
            return Ok(());
        };
        session
            .write_file(PRE_ARTIFACTS_GUEST_SCRIPT, script, 0o700)
            .await?;
        let output = session
            .command(
                VmCommand::new(PRE_ARTIFACTS_GUEST_SCRIPT)
                    .current_directory(&launch.workspace)
                    .environment(base_guest_environment(task, &launch.workspace))
                    .timeout(task.verifier().timeout()),
            )
            .await?;
        fs::write(
            self.artifact_directory.join("pre-artifacts-stdout.log"),
            &output.stdout,
        )?;
        fs::write(
            self.artifact_directory.join("pre-artifacts-stderr.log"),
            &output.stderr,
        )?;
        if output.exit_code != 0 {
            return Err(io::Error::other(format!(
                "pre-artifact capture exited {}: {}",
                output.exit_code,
                String::from_utf8_lossy(&output.stderr)
            ))
            .into());
        }
        Ok(())
    }

    async fn collect_artifacts(
        &self,
        session: &VmToolSession,
        task: &Task,
        launch: &VmLaunch,
    ) -> Result<Option<Vec<u8>>, VmAttemptError> {
        self.run_pre_artifacts(session, task, launch).await?;
        for collect in task.verifier().collect() {
            let output = session
                .command(
                    VmCommand::new("/bin/sh")
                        .arg("-c")
                        .arg(collect.command())
                        .current_directory(&launch.workspace)
                        .environment(base_guest_environment(task, &launch.workspace))
                        .timeout(task.verifier().timeout()),
                )
                .await?;
            if output.exit_code != 0 {
                return Err(io::Error::other(format!(
                    "verifier artifact collection exited {}: {}",
                    output.exit_code,
                    String::from_utf8_lossy(&output.stderr)
                ))
                .into());
            }
        }
        if task.artifacts().is_empty() {
            return Ok(None);
        }

        let mut command = VmCommand::new("/bin/tar");
        for argument in artifact_archive_arguments(task)? {
            command = command.arg(argument);
        }
        let output = session
            .command(command.timeout(task.verifier().timeout()))
            .await?;
        if output.exit_code != 0 {
            return Err(io::Error::other(format!(
                "artifact archive exited {}: {}",
                output.exit_code,
                String::from_utf8_lossy(&output.stderr)
            ))
            .into());
        }
        session
            .read_file("/tmp/nanoeval-artifacts.tar")
            .await
            .map(Some)
            .map_err(Into::into)
    }

    async fn stage_artifacts(
        session: &VmToolSession,
        artifacts: Option<Vec<u8>>,
    ) -> Result<(), VmAttemptError> {
        let Some(artifacts) = artifacts else {
            return Ok(());
        };
        session
            .write_file("/tmp/nanoeval-artifacts.tar", artifacts, 0o600)
            .await?;
        let output = session
            .command(
                VmCommand::new("/bin/tar")
                    .arg("-C")
                    .arg("/")
                    .arg("-xf")
                    .arg("/tmp/nanoeval-artifacts.tar")
                    .timeout(Duration::from_mins(10)),
            )
            .await?;
        if output.exit_code != 0 {
            return Err(io::Error::other(format!(
                "artifact extraction exited {}: {}",
                output.exit_code,
                String::from_utf8_lossy(&output.stderr)
            ))
            .into());
        }
        Ok(())
    }

    async fn verify_inner(
        &mut self,
        task: &Task,
        attempt: EvalAttempt<'_>,
    ) -> Result<AttemptVerification, AttemptVerificationFailure> {
        if let Err(error) = task.validate_package() {
            let occurred_at = Utc::now();
            let cleanup = self.shutdown_before_verification().await;
            return Err(AttemptVerificationFailure::observed_at(
                error,
                occurred_at,
                cleanup,
            ));
        }
        let verifier_directory = attempt.directory().join("verifier");
        if let Err(error) = fs::create_dir_all(&verifier_directory) {
            let occurred_at = Utc::now();
            let cleanup = self.shutdown_before_verification().await;
            return Err(AttemptVerificationFailure::observed_at(
                error,
                occurred_at,
                cleanup,
            ));
        }
        let (verifier_launch, verifier_session) = self.start_verifier_session(task).await?;
        let verification = async {
            if task.output() == TaskOutput::FinalMessage {
                verifier_session
                    .write_file(
                        format!("{}/answer.txt", verifier_launch.workspace),
                        attempt
                            .final_message()
                            .unwrap_or_default()
                            .as_bytes()
                            .to_vec(),
                        0o600,
                    )
                    .await?;
            }
            let command =
                self.verifier_command(task, &verifier_launch, self.attempt_cache.as_ref())?;
            let (output, verifier_timed_out) = self
                .execute_verifier_with_network_retries(&verifier_session, &verifier_launch, command)
                .await?;
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let combined = match (stdout.is_empty(), stderr.is_empty()) {
                (_, true) => stdout.clone(),
                (true, false) => stderr.clone(),
                (false, false) => format!("{stdout}\n{stderr}"),
            };
            fs::write(verifier_directory.join("test-stdout.txt"), combined)?;
            let (reward_name, reward_bytes, rewards) = if verifier_timed_out {
                (
                    "reward.txt",
                    b"0\n".to_vec(),
                    BTreeMap::from([("reward".to_owned(), 0.0)]),
                )
            } else {
                read_verifier_rewards(&verifier_session).await?
            };
            Self::stage_verifier_logs(&verifier_session, &verifier_directory).await?;
            fs::write(verifier_directory.join(reward_name), &reward_bytes)?;
            if let Ok(ctrf) = verifier_session.read_file("/logs/verifier/ctrf.json").await {
                fs::write(verifier_directory.join("ctrf.json"), ctrf)?;
            }
            let answer_path = format!("{}/answer.txt", verifier_launch.workspace);
            if let Ok(answer) = verifier_session.read_file(answer_path).await {
                fs::write(attempt.workspace().join("answer.txt"), answer)?;
            }
            task.validate_package()?;
            Ok::<_, VmAttemptError>((output, stdout, stderr, rewards))
        }
        .await;
        let verification_error_at = verification.as_ref().err().map(|_| Utc::now());
        let cleanup_started = Utc::now();
        let shutdown = verifier_session.shutdown().await;
        let (output, stdout, stderr, rewards) = match verification {
            Ok(verification) => verification,
            Err(primary) => {
                let cleanup = self.cleanup_after_shutdown(cleanup_started, shutdown, false);
                return Err(AttemptVerificationFailure::observed_at(
                    primary,
                    verification_error_at.unwrap_or(cleanup_started),
                    cleanup,
                ));
            }
        };
        let cleanup = match shutdown {
            Ok(()) => {
                let cache_cleanup = self.finish_verifier_cache();
                let disk_cleanup = self.remove_disposable_root_disks(
                    task.verifier().scoring_policy().passes(&rewards),
                );
                match cache_cleanup.and(disk_cleanup) {
                    Ok(()) => CleanupPhase::completed(cleanup_started),
                    Err(error) => CleanupPhase::failed(cleanup_started, &error),
                }
            }
            Err(error) => {
                if let Err(cache_error) = self.try_remove_attempt_cache() {
                    warn!(
                        target: "nanocodex_eval",
                        error = %cache_error,
                        primary_error = %error,
                        "verifier cache cleanup also failed after VM shutdown failure"
                    );
                }
                if let Err(disk_error) = self
                    .remove_disposable_root_disks(task.verifier().scoring_policy().passes(&rewards))
                {
                    warn!(
                        target: "nanocodex_eval",
                        error = %disk_error,
                        primary_error = %error,
                        "VM root disk cleanup also failed after VM shutdown failure"
                    );
                }
                CleanupPhase::failed(cleanup_started, &error)
            }
        };
        Ok(AttemptVerification {
            result: VerifierResult {
                exit_code: output.exit_code,
                rewards,
            },
            stdout,
            stderr,
            cleanup,
        })
    }

    async fn start_verifier_session(
        &mut self,
        task: &Task,
    ) -> Result<(VmLaunch, VmToolSession), AttemptVerificationFailure> {
        let Some(agent_session) = self.agent_session.take() else {
            return Err(AttemptVerificationFailure::new(
                VmAttemptError::AgentSessionAlreadyFinished,
                CleanupPhase::not_required(),
            ));
        };
        if let Err(primary) = agent_session.terminate_tool_processes().await {
            let occurred_at = Utc::now();
            let cleanup = self.cleanup_session(Some(&agent_session)).await;
            return Err(AttemptVerificationFailure::observed_at(
                primary,
                occurred_at,
                cleanup,
            ));
        }
        let launch = self
            .separate_launch
            .clone()
            .unwrap_or_else(|| self.launch.clone());
        let session = if self.separate_launch.is_some() {
            let artifacts = match self
                .collect_artifacts(&agent_session, task, &self.launch)
                .await
            {
                Ok(artifacts) => artifacts,
                Err(primary) => {
                    let occurred_at = Utc::now();
                    let cleanup = self.cleanup_session(Some(&agent_session)).await;
                    return Err(AttemptVerificationFailure::observed_at(
                        primary,
                        occurred_at,
                        cleanup,
                    ));
                }
            };
            let cleanup_started = Utc::now();
            if let Err(primary) = agent_session.shutdown().await {
                let occurred_at = Utc::now();
                if let Err(cache_error) = self.try_remove_attempt_cache() {
                    warn!(
                        target: "nanocodex_eval",
                        error = %cache_error,
                        primary_error = %primary,
                        "verifier cache cleanup also failed after VM shutdown failure"
                    );
                }
                if let Err(disk_error) = self.remove_disposable_root_disks(false) {
                    warn!(
                        target: "nanocodex_eval",
                        error = %disk_error,
                        primary_error = %primary,
                        "VM root disk cleanup also failed after VM shutdown failure"
                    );
                }
                let cleanup = CleanupPhase::failed(cleanup_started, &primary);
                return Err(AttemptVerificationFailure::observed_at(
                    primary,
                    occurred_at,
                    cleanup,
                ));
            }
            let session = match launch.spawn(None) {
                Ok(session) => session,
                Err(primary) => {
                    let occurred_at = Utc::now();
                    let cleanup = self.cleanup_after_shutdown(cleanup_started, Ok(()), false);
                    return Err(AttemptVerificationFailure::observed_at(
                        primary,
                        occurred_at,
                        cleanup,
                    ));
                }
            };
            if let Err(primary) = Self::stage_artifacts(&session, artifacts).await {
                let occurred_at = Utc::now();
                let cleanup = self.cleanup_session(Some(&session)).await;
                return Err(AttemptVerificationFailure::observed_at(
                    primary,
                    occurred_at,
                    cleanup,
                ));
            }
            session
        } else {
            agent_session
        };
        let setup = async {
            // Verifier files are execution inputs, not image contents. Stage
            // them for both retained agent environments and freshly launched
            // isolated verifier environments.
            let tests = tempfile::tempdir()?;
            task.materialize_verifier_files(tests.path())?;
            Self::copy_directory(&session, tests.path(), tests.path(), Path::new("/tests")).await?;
            session
                .write_file("/logs/verifier/.nanoeval", Vec::new(), 0o600)
                .await?;
            if self.attempt_cache.is_some() {
                self.mount_verifier_cache(&session).await?;
            }
            self.stage_cached_verifier(&session, task).await
        }
        .await;
        if let Err(primary) = setup {
            let occurred_at = Utc::now();
            let cleanup = self.cleanup_session(Some(&session)).await;
            return Err(AttemptVerificationFailure::observed_at(
                primary,
                occurred_at,
                cleanup,
            ));
        }
        Ok((launch, session))
    }

    async fn shutdown_before_verification(&mut self) -> CleanupPhase {
        let session = self.agent_session.take();
        self.cleanup_session(session.as_ref()).await
    }

    async fn cleanup_session(&mut self, session: Option<&VmToolSession>) -> CleanupPhase {
        if session.is_none() && self.attempt_cache.is_none() && self.retain_failed_rootfs {
            return CleanupPhase::not_required();
        }
        let cleanup_started = Utc::now();
        let shutdown = match session {
            Some(session) => session.shutdown().await,
            None => Ok(()),
        };
        self.cleanup_after_shutdown(cleanup_started, shutdown, false)
    }

    fn cleanup_after_shutdown(
        &mut self,
        cleanup_started: DateTime<Utc>,
        shutdown: Result<(), VmToolSessionError>,
        commit_cache: bool,
    ) -> CleanupPhase {
        let cache_cleanup = if commit_cache {
            self.finish_verifier_cache()
        } else {
            self.try_remove_attempt_cache()
        };
        let disk_cleanup = self.remove_disposable_root_disks(false);
        let resource_cleanup = cache_cleanup.and(disk_cleanup);
        match (shutdown, resource_cleanup) {
            (Ok(()), Ok(())) => CleanupPhase::completed(cleanup_started),
            (Err(primary), secondary) => {
                if let Err(secondary) = secondary {
                    warn!(
                        target: "nanocodex_eval",
                        error = %secondary,
                        primary_error = %primary,
                        "verifier cache cleanup also failed after VM shutdown failure"
                    );
                }
                CleanupPhase::failed(cleanup_started, &primary)
            }
            (Ok(()), Err(error)) => CleanupPhase::failed(cleanup_started, &error),
        }
    }

    fn finish_verifier_cache(&mut self) -> Result<(), VmAttemptError> {
        if let (Some(cache), Some(attempt_cache)) = (&self.cache, &self.attempt_cache)
            && !attempt_cache.skip_setup
        {
            if cache.mark_ready(attempt_cache)? {
                info!(
                    target: "nanocodex_eval",
                    verifier_cache_key = cache.key,
                    verifier_cache_previous_status = cache.status,
                    "post-agent verifier dependency cache committed"
                );
            } else {
                warn!(
                    target: "nanocodex_eval",
                    verifier_cache_key = cache.key,
                    "verifier dependency cache remained incomplete"
                );
            }
        }
        if let Some(attempt_cache) = self.attempt_cache.take() {
            fs::remove_file(attempt_cache.disk)?;
        }
        Ok(())
    }

    fn try_remove_attempt_cache(&mut self) -> Result<(), VmAttemptError> {
        let Some(attempt_cache) = self.attempt_cache.take() else {
            return Ok(());
        };
        match fs::remove_file(&attempt_cache.disk) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn remove_attempt_cache(&mut self) {
        if let Err(error) = self.try_remove_attempt_cache() {
            warn!(
                target: "nanocodex_eval",
                %error,
                "failed to remove disposable attempt verifier cache"
            );
        }
    }

    fn remove_disposable_root_disks(&mut self, passed: bool) -> Result<(), VmAttemptError> {
        let retain = if passed {
            self.retain_passed_rootfs
        } else {
            self.retain_failed_rootfs
        };
        if retain {
            self.root_disks_finalized = true;
            return Ok(());
        }

        let mut failures = Vec::new();
        for launch in std::iter::once(&self.launch).chain(self.separate_launch.as_ref()) {
            let Some(root) = launch.root.writable_disk() else {
                continue;
            };
            match remove_rootfs(root) {
                Ok(true) => info!(
                    target: "nanocodex_eval",
                    vm_rootfs_path = %root.display(),
                    vm_attempt_passed = passed,
                    "removed disposable attempt VM root disk"
                ),
                Ok(false) => {}
                Err(error) => {
                    warn!(
                        target: "nanocodex_eval",
                        vm_rootfs_path = %root.display(),
                        vm_attempt_passed = passed,
                        %error,
                        "failed to remove disposable attempt VM root disk"
                    );
                    failures.push(format!("{}: {error}", root.display()));
                }
            }
        }
        if failures.is_empty() {
            self.root_disks_finalized = true;
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "failed to remove disposable attempt VM root disks: {}",
                failures.join("; ")
            ))
            .into())
        }
    }

    async fn execute_verifier_command(
        session: &VmToolSession,
        command: VmCommand,
    ) -> Result<(VmCommandOutput, bool), VmAttemptError> {
        match session.command(command).await {
            Ok(output) => Ok((output, false)),
            Err(VmToolSessionError::GuestTimeout { timeout, output }) => {
                Ok((verifier_timeout_output(timeout, output), true))
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn execute_verifier_with_network_retries(
        &self,
        session: &VmToolSession,
        launch: &VmLaunch,
        command: VmCommand,
    ) -> Result<(VmCommandOutput, bool), VmAttemptError> {
        for retry in 0..=VERIFIER_NETWORK_RETRIES {
            restore_verifier_resolver(session, launch).await?;
            let result = Self::execute_verifier_command(session, command.clone()).await?;
            if result.1
                || retry == VERIFIER_NETWORK_RETRIES
                || !verifier_bootstrap_network_failed(&result.0)
            {
                return Ok(result);
            }
            let delay = verifier_network_retry_delay(retry);
            warn!(
                target: "nanocodex_eval",
                retry = retry + 1,
                max_retries = VERIFIER_NETWORK_RETRIES,
                retry_delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                "canonical verifier dependency bootstrap hit a transient network failure; retrying"
            );
            tokio::time::sleep(delay).await;
        }
        unreachable!("the verifier retry loop always returns")
    }

    async fn stage_cached_verifier(
        &self,
        session: &VmToolSession,
        task: &Task,
    ) -> Result<(), VmAttemptError> {
        if !self
            .attempt_cache
            .as_ref()
            .is_some_and(|cache| cache.skip_setup)
        {
            return Ok(());
        }
        let cache = self
            .cache
            .as_ref()
            .ok_or_else(|| io::Error::other("verifier cache metadata is missing"))?;
        let script = task.verifier_script_bytes()?;
        let cached = cached_verifier_script(
            &script,
            RecognizedVerifierSetup {
                cacheable_start: cache.cacheable_start,
                cacheable_end: cache.cacheable_end,
                skip_setup: cache.skip_setup,
            },
        );
        session
            .write_file(CACHED_VERIFIER_SCRIPT, cached, 0o700)
            .await?;
        Ok(())
    }

    async fn mount_verifier_cache(&self, session: &VmToolSession) -> Result<(), VmAttemptError> {
        mount_verifier_cache(session, self.launch.verifier_cache_block_device()).await
    }

    fn verifier_command(
        &self,
        task: &Task,
        launch: &VmLaunch,
        attempt_cache: Option<&AttemptVerifierCache>,
    ) -> Result<VmCommand, VmAttemptError> {
        let skip_setup = attempt_cache.is_some_and(|cache| cache.skip_setup);
        let mut command = if skip_setup {
            let cache = self
                .cache
                .as_ref()
                .ok_or_else(|| io::Error::other("verifier cache metadata is missing"))?;
            info!(
                target: "nanocodex_eval",
                verifier_cache_key = cache.key,
                verifier_setup_bytes_skipped = cache.cacheable_end - cache.cacheable_start,
                verifier_system_setup_bytes = cache.cacheable_start,
                "running canonical verifier with only persisted setup omitted"
            );
            VmCommand::new(verifier_shell(&launch.shell, skip_setup)).arg(CACHED_VERIFIER_SCRIPT)
        } else {
            VmCommand::new(verifier_shell(&launch.shell, skip_setup)).arg("/tests/test.sh")
        };
        command = command
            .current_directory(&launch.workspace)
            .environment(verifier_guest_environment(
                task,
                &launch.workspace,
                &self.verifier_environment,
            ))
            .timeout(task.verifier().timeout());
        Ok(command)
    }

    fn copy_directory<'a>(
        session: &'a VmToolSession,
        root: &'a Path,
        directory: &'a Path,
        destination: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<(), VmAttemptError>> + Send + 'a>> {
        Box::pin(async move {
            let relative = directory.strip_prefix(root).map_err(io::Error::other)?;
            let guest_directory = destination.join(relative).to_string_lossy().into_owned();
            let directory_mode =
                std::os::unix::fs::PermissionsExt::mode(&fs::metadata(directory)?.permissions())
                    & 0o7777;
            session
                .create_directory(&guest_directory, 0o700, None)
                .await?;
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                let path = entry.path();
                let relative = path.strip_prefix(root).map_err(io::Error::other)?;
                let guest = destination.join(relative).to_string_lossy().into_owned();
                let file_type = entry.file_type()?;
                if file_type.is_dir() {
                    Self::copy_directory(session, root, &path, destination).await?;
                } else if file_type.is_file() {
                    let mode =
                        std::os::unix::fs::PermissionsExt::mode(&entry.metadata()?.permissions())
                            & 0o7777;
                    session
                        .write_file_with_mtime(guest.as_str(), fs::read(path)?, mode, 0)
                        .await?;
                } else {
                    return Err(VmAttemptError::Collision(path));
                }
            }
            session
                .create_directory(&guest_directory, directory_mode, Some(0))
                .await?;
            Ok(())
        })
    }
}

fn artifact_archive_arguments(task: &Task) -> Result<Vec<String>, VmAttemptError> {
    let mut arguments = vec![
        "-C".to_owned(),
        "/".to_owned(),
        "-cf".to_owned(),
        "/tmp/nanoeval-artifacts.tar".to_owned(),
    ];
    let mut sources = Vec::with_capacity(task.artifacts().len());
    for artifact in task.artifacts() {
        if let Some(service) = artifact.service() {
            return Err(io::Error::other(format!(
                "artifact {} belongs to unsupported service {service:?}",
                artifact.source().display()
            ))
            .into());
        }
        let relative = artifact.source().strip_prefix("/").map_err(|_| {
            io::Error::other(format!(
                "artifact path must be absolute: {}",
                artifact.source().display()
            ))
        })?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(io::Error::other(format!(
                "artifact path is not a safe guest path: {}",
                artifact.source().display()
            ))
            .into());
        }
        for excluded in artifact.exclude() {
            arguments.push(format!(
                "--exclude={}/{}",
                relative.to_string_lossy(),
                excluded.to_string_lossy()
            ));
        }
        sources.push(
            relative
                .to_str()
                .ok_or_else(|| {
                    io::Error::other(format!(
                        "artifact path is not UTF-8: {}",
                        artifact.source().display()
                    ))
                })?
                .to_owned(),
        );
    }
    arguments.push("--".to_owned());
    arguments.extend(sources);
    Ok(arguments)
}

impl Drop for VmVerifier {
    fn drop(&mut self) {
        self.remove_attempt_cache();
        if !self.root_disks_finalized
            && let Err(error) = self.remove_disposable_root_disks(false)
        {
            warn!(
                target: "nanocodex_eval",
                %error,
                "failed to remove disposable attempt VM root disks on drop"
            );
        }
    }
}

const fn verifier_network_retry_delay(retry: usize) -> Duration {
    let exponent = if retry > 8 { 8 } else { retry };
    VERIFIER_NETWORK_RETRY_BASE_DELAY.saturating_mul(1_u32 << exponent)
}

async fn restore_verifier_resolver(
    session: &VmToolSession,
    launch: &VmLaunch,
) -> Result<(), VmAttemptError> {
    if launch.resolver_configuration.is_empty() {
        return Ok(());
    }
    let output = session
        .command(
            VmCommand::new("/bin/sh")
                .arg("-c")
                .arg(format!(
                    "rm -f /etc/resolv.conf && printf '{}' > /etc/resolv.conf",
                    launch.resolver_configuration
                ))
                .timeout(Duration::from_secs(10)),
        )
        .await?;
    if output.exit_code != 0 {
        return Err(io::Error::other(format!(
            "restoring verifier DNS configuration exited {}: {}",
            output.exit_code,
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }
    Ok(())
}

async fn mount_verifier_cache(
    session: &VmToolSession,
    block_device: &str,
) -> Result<(), VmAttemptError> {
    let output = session
        .command(
            VmCommand::new("/bin/sh")
                .arg("-c")
                .arg(format!(
                    "mkdir -p {VERIFIER_CACHE_MOUNT} /var/cache/apt/archives /var/lib/apt/lists /root/.cache/uv /root/.local && mount -t ext4 {block_device} {VERIFIER_CACHE_MOUNT} && mount --bind {VERIFIER_CACHE_MOUNT}/apt-archives /var/cache/apt/archives && mount --bind {VERIFIER_CACHE_MOUNT}/apt-lists /var/lib/apt/lists && mount --bind {VERIFIER_CACHE_MOUNT}/uv-cache /root/.cache/uv && mount --bind {VERIFIER_CACHE_MOUNT}/uv-home /root/.local"
                ))
                .timeout(Duration::from_secs(30)),
        )
        .await?;
    if output.exit_code != 0 {
        return Err(io::Error::other(format!(
            "mounting verifier cache exited {}: {}",
            output.exit_code,
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }
    Ok(())
}

fn remove_rootfs(rootfs: &Path) -> io::Result<bool> {
    if !rootfs.is_file() {
        return Ok(false);
    }
    fs::remove_file(rootfs)?;
    Ok(true)
}

fn verifier_timeout_output(
    timeout: Duration,
    mut output: VmCommandPartialOutput,
) -> VmCommandOutput {
    output.stderr.extend_from_slice(
        format!(
            "\ncanonical verifier exceeded its {timeout:?} deadline; \
             the candidate is scored with reward 0\n"
        )
        .as_bytes(),
    );
    VmCommandOutput {
        exit_code: 124,
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

const fn verifier_shell(configured: &str, skip_setup: bool) -> &str {
    if skip_setup { "/bin/bash" } else { configured }
}

fn base_guest_environment(task: &Task, workspace: &str) -> Vec<(String, String)> {
    let mut environment = BTreeMap::from([
        (
            "PATH".to_owned(),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_owned(),
        ),
        ("HOME".to_owned(), "/root".to_owned()),
        ("NANOCODEX_EVAL_WORKSPACE".to_owned(), workspace.to_owned()),
        (
            "NANOCODEX_EVAL_VERIFIER_LOGS".to_owned(),
            "/logs/verifier".to_owned(),
        ),
        // Retained tasks from the temporary Nanoeval repository still
        // consume these names.
        ("NANOEVAL_WORKSPACE".to_owned(), workspace.to_owned()),
        (
            "NANOEVAL_VERIFIER_LOGS".to_owned(),
            "/logs/verifier".to_owned(),
        ),
    ]);
    environment.extend(task.environment().clone());
    environment.extend(task.verifier().environment().clone());
    environment.into_iter().collect()
}

fn verifier_guest_environment(
    task: &Task,
    workspace: &str,
    runtime: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let mut environment = base_guest_environment(task, workspace)
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    environment.extend(runtime.clone());
    environment.into_iter().collect()
}

fn record_operation<T, E>(span: &tracing::Span, started_at: Instant, result: &Result<T, E>)
where
    E: std::fmt::Display,
{
    let duration_ns = u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
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
                    "VM attempt operation completed"
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
                    "VM attempt operation failed"
                );
            });
        }
    }
}

fn materialize_rootfs(source: &Path, destination: &Path) -> Result<(), VmAttemptError> {
    if !source.is_dir() {
        return Err(VmAttemptError::InvalidRootfs(source.to_path_buf()));
    }
    copy_root_entries(source, destination, true)
}

fn copy_root_entries(source: &Path, destination: &Path, root: bool) -> Result<(), VmAttemptError> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if root && matches!(entry.file_name().to_str(), Some("workspace" | "verifier")) {
            continue;
        }
        let source = entry.path();
        let target = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.file_type().is_symlink() {
            if target.exists() || fs::symlink_metadata(&target).is_ok() {
                return Err(VmAttemptError::Collision(target));
            }
            std::os::unix::fs::symlink(fs::read_link(source)?, target)?;
        } else if metadata.is_dir() {
            if target.exists() && !target.is_dir() {
                return Err(VmAttemptError::Collision(target));
            }
            fs::create_dir_all(&target)?;
            copy_root_entries(&source, &target, false)?;
        } else if metadata.is_file() {
            if target.exists() {
                return Err(VmAttemptError::Collision(target));
            }
            fs::copy(source, target)?;
        } else {
            return Err(VmAttemptError::Collision(source));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "vm/tests.rs"]
mod tests;
