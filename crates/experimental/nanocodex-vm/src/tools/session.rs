use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    process::{ExitStatus, Stdio},
    sync::{
        Arc, Mutex as StdMutex, MutexGuard as StdMutexGuard, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    command::GuestCommand,
    config::VmConfig,
    egress::EgressLease,
    process::{PrivateVmProcessConfig, VmProcessConfig, VmProcessError},
};
use nanocodex_tools::{ToolContext, ToolInput, ToolOutput, ToolResult, standard::StandardTool};
use thiserror::Error;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
};
use tracing::{Instrument, Span, info, info_span};

use super::{
    VmToolClient,
    protocol::{
        CancelRequest, ControlResponse, CreateDirectoryRequest, ExecuteRequest, ExecuteResponse,
        MemoryRequest, MemoryResponse, ReadFileRequest, ReadFileResponse, ReadyRequest,
        SessionRequest, SessionResponse, ShutdownRequest, TerminateToolProcessesRequest,
        ToolRequest, WireToolContext, WireToolInput, WriteFileRequest,
    },
};

pub(crate) const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_COMMAND_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_GUEST_IN_FLIGHT_REQUESTS: usize = 64;
const MAX_HOST_IN_FLIGHT_REQUESTS: usize = MAX_GUEST_IN_FLIGHT_REQUESTS - 1;
const REQUEST_QUEUE_CAPACITY: usize = MAX_GUEST_IN_FLIGHT_REQUESTS;
const MAX_TERMINAL_STDERR_BYTES: usize = 64 * 1024;
const TERMINAL_STDERR_DRAIN_GRACE: Duration = Duration::from_millis(250);
const MEMORY_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(target_os = "linux")]
const HOST_MEMORY_SAMPLE_INTERVAL: Duration = Duration::from_millis(100);
const KIB_PER_MIB: u64 = 1024;

/// One trusted host-control command executed inside the guest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmCommand {
    program: String,
    arguments: Vec<String>,
    current_directory: String,
    environment: Vec<(String, String)>,
    timeout: Duration,
    max_output_bytes: usize,
    stdout_mirror: Option<String>,
    stderr_mirror: Option<String>,
}

impl VmCommand {
    /// Creates a trusted guest command with a one-minute deadline, `/` as its
    /// working directory, and an 8 MiB combined output limit.
    #[must_use]
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
            current_directory: "/".to_owned(),
            environment: Vec::new(),
            timeout: Duration::from_mins(1),
            max_output_bytes: DEFAULT_COMMAND_OUTPUT_BYTES,
            stdout_mirror: None,
            stderr_mirror: None,
        }
    }

    /// Appends one argument.
    #[must_use]
    pub fn arg(mut self, argument: impl Into<String>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    /// Sets the guest working directory.
    #[must_use]
    pub fn current_directory(mut self, directory: impl Into<String>) -> Self {
        self.current_directory = directory.into();
        self
    }

    /// Extends the guest environment.
    #[must_use]
    pub fn environment(mut self, environment: impl IntoIterator<Item = (String, String)>) -> Self {
        self.environment.extend(environment);
        self
    }

    /// Sets the execution deadline.
    #[must_use]
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Bounds the combined stdout and stderr retained by this command.
    #[must_use]
    pub const fn max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes;
        self
    }

    /// Mirrors output into harness-owned guest files while retaining the same
    /// bounded terminal output.
    ///
    /// The files are truncated before the process starts and updated as bytes
    /// arrive, so another session request can observe long-running command
    /// progress without weakening terminal output or timeout semantics.
    #[must_use]
    pub fn mirror_output(mut self, stdout: impl Into<String>, stderr: impl Into<String>) -> Self {
        self.stdout_mirror = Some(stdout.into());
        self.stderr_mirror = Some(stderr.into());
        self
    }
}

/// Complete output from one trusted host-control command in the guest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmCommandOutput {
    /// Guest process exit code.
    pub exit_code: i32,
    /// Complete bounded standard output.
    pub stdout: Vec<u8>,
    /// Complete bounded standard error.
    pub stderr: Vec<u8>,
}

/// Bounded output captured before a trusted guest command failed to complete.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmCommandPartialOutput {
    /// Standard output captured before the command stopped.
    pub stdout: Vec<u8>,
    /// Standard error captured before the command stopped.
    pub stderr: Vec<u8>,
}

/// Peak host and guest memory observed over one VM session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VmMemoryObservation {
    host_peak_rss_mib: Option<u64>,
    guest_total_mib: Option<u64>,
    guest_peak_used_mib: Option<u64>,
    guest_oom_kills: u64,
    terminal_oom: bool,
}

impl VmMemoryObservation {
    /// Returns the VMM process's peak resident set on supported Linux hosts.
    #[must_use]
    pub const fn host_peak_rss_mib(self) -> Option<u64> {
        self.host_peak_rss_mib
    }

    /// Returns the guest kernel's total usable memory.
    #[must_use]
    pub const fn guest_total_mib(self) -> Option<u64> {
        self.guest_total_mib
    }

    /// Returns peak guest memory use derived from minimum `MemAvailable`.
    #[must_use]
    pub const fn guest_peak_used_mib(self) -> Option<u64> {
        self.guest_peak_used_mib
    }

    /// Returns the guest kernel's observed OOM-kill count for this session.
    #[must_use]
    pub const fn guest_oom_kills(self) -> u64 {
        self.guest_oom_kills
    }

    /// Returns whether a guest counter or terminal kernel diagnostic confirms OOM.
    #[must_use]
    pub const fn oom_detected(self) -> bool {
        self.guest_oom_kills > 0 || self.terminal_oom
    }
}

/// Failure to start, use, provision, or stop one retained VM tool session.
#[derive(Debug, Error)]
pub enum VmToolSessionError {
    /// A session was started without an active Tokio runtime.
    #[error("starting a VM tool session requires an active Tokio runtime")]
    NoRuntime,

    /// The VMM child could not be spawned.
    #[error("failed to spawn the VMM process: {0}")]
    Spawn(#[source] std::io::Error),

    /// The VMM command did not expose a required protocol pipe.
    #[error("the VMM process did not expose piped {0}")]
    MissingPipe(&'static str),

    /// Host-side process or protocol I/O failed.
    #[error("VM tool console I/O failed: {0}")]
    Io(#[from] std::io::Error),

    /// A host or guest protocol frame was not valid JSON.
    #[error("VM tool protocol JSON failed: {0}")]
    Json(#[from] serde_json::Error),

    /// The VMM console closed before a pending response arrived.
    #[error("the VM tool console closed before replying")]
    Closed,

    /// VM startup or egress provisioning did not complete before its deadline.
    #[error("VM readiness and egress provisioning did not complete within {0:?}")]
    StartupTimeout(Duration),

    /// The background response router failed.
    #[error("VM tool response router failed: {0}")]
    Router(String),

    /// The guest returned an application-level tool error.
    #[error("guest tool execution failed: {0}")]
    Guest(String),

    /// A trusted host-control command exceeded its deadline.
    #[error("guest command exceeded {timeout:?}")]
    GuestTimeout {
        /// Configured command deadline.
        timeout: Duration,
        /// Bounded output captured before the process group was terminated.
        output: VmCommandPartialOutput,
    },

    /// A trusted host-control command exceeded its combined output limit.
    #[error("guest command output exceeded the {0}-byte limit")]
    GuestOutputLimit(usize),

    /// The guest returned a response of the wrong typed shape.
    #[error("invalid VM tool response: {0}")]
    Protocol(&'static str),

    /// An inbound or outbound protocol frame exceeded the fixed limit.
    #[error("VM tool protocol frame exceeded the {MAX_FRAME_BYTES}-byte limit")]
    FrameTooLarge,

    /// Graceful guest shutdown did not stop the VMM before the deadline.
    #[error("the VMM did not exit within {0:?} after guest shutdown")]
    ShutdownTimeout(Duration),

    /// Managed guest tool processes did not stop before the deadline.
    #[error("managed guest tool processes did not stop within {0:?}")]
    ToolProcessTerminationTimeout(Duration),

    /// The VMM returned an unsuccessful status after guest shutdown.
    #[error("the VMM exited unsuccessfully after guest shutdown: {0}")]
    VmmExit(ExitStatus),

    /// Public egress assets were provisioned more than once.
    #[error("egress was already provisioned for this VM session")]
    EgressAlreadyProvisioned,

    /// Graceful shutdown was requested while sibling capabilities remained.
    #[error("cannot shut down the VM while {0} sibling capabilities are still alive")]
    ActiveCapabilities(usize),

    /// Graceful shutdown was requested while owner-borrowed operations remained.
    #[error("cannot shut down the VM while {0} requests are still in flight")]
    ActiveRequests(usize),

    /// A public egress destination could not be represented by the guest protocol.
    #[error("egress guest file path is not valid UTF-8: {0}")]
    EgressFilePath(PathBuf),

    /// Private VMM launch-record persistence failed.
    #[error(transparent)]
    VmProcess(#[from] VmProcessError),
}

#[derive(Debug, Error)]
#[error("VM tool session ended before this request completed")]
struct ModelSafeVmToolError {
    #[source]
    diagnostic: VmToolSessionError,
}

/// Owner of one persistent VMM child carrying workspace tool calls.
///
/// Keep this value alive for the complete root-agent tree. Clone
/// [`VmToolSessionHandle`] or [`super::VmTools`] into each driver's tool
/// factory; all of those handles route to this one VM.
pub struct VmToolSession {
    handle: VmToolSessionHandle,
}

/// Clone-cheap capability for sending workspace tool calls to one owned VM.
pub struct VmToolSessionHandle {
    inner: Arc<VmToolSessionInner>,
    counted_capability: bool,
}

struct VmToolSessionInner {
    spawned_at: Instant,
    next_id: AtomicU64,
    host_peak_rss_kib: AtomicU64,
    closing: AtomicBool,
    lifecycle: StdMutex<SessionLifecycle>,
    input: mpsc::Sender<OutboundRequest>,
    cancellations: mpsc::Sender<OutboundCancellation>,
    request_slots: Arc<Semaphore>,
    shutdown_timeout: Duration,
    output: Mutex<Option<ChildStdout>>,
    pending: StdMutex<PendingState>,
    terminal: StdMutex<TerminalDiagnostics>,
    terminal_closed: Notify,
    child: StdMutex<Option<Child>>,
    egress: StdMutex<Option<EgressLease>>,
    process_config: StdMutex<Option<PrivateVmProcessConfig>>,
}

#[derive(Default)]
struct SessionLifecycle {
    capabilities: usize,
    in_flight_requests: usize,
    closing: bool,
}

struct OutboundRequest {
    id: u64,
    frame: Vec<u8>,
    written: Arc<AtomicBool>,
}

struct OutboundCancellation {
    target_id: u64,
    frame: Vec<u8>,
    written: Arc<AtomicBool>,
    _request_slot: OwnedSemaphorePermit,
}

#[derive(Default)]
struct PendingState {
    closed: Option<String>,
    requests: HashMap<u64, PendingResponse>,
}

struct PendingResponse {
    span: Span,
    response: oneshot::Sender<Result<(SessionResponse, usize), String>>,
}

#[derive(Default)]
struct TerminalDiagnostics {
    stderr_tail: Vec<u8>,
    stderr_error: Option<String>,
    closed: bool,
}

struct PendingRequestGuard {
    inner: Weak<VmToolSessionInner>,
    id: u64,
    armed: bool,
    queued: bool,
    written: Arc<AtomicBool>,
    request_slot: Option<OwnedSemaphorePermit>,
}

struct InFlightRequestGuard {
    inner: Weak<VmToolSessionInner>,
}

struct ShutdownGuard {
    inner: Arc<VmToolSessionInner>,
    armed: bool,
}

impl VmToolSession {
    /// Spawns one VM from complete typed inputs without an egress provider.
    ///
    /// `command` must invoke a dedicated VMM process that accepts the private
    /// [`VmProcessConfig`] path as its next argument. The configuration remains
    /// alive until the returned session stops, and keeps guest environment
    /// values out of process arguments.
    ///
    /// Use [`Self::spawn_configured`] when a host-owned egress lease must also
    /// configure and provision the guest.
    ///
    /// # Errors
    ///
    /// Returns an error when the private configuration cannot be written or
    /// the VMM process cannot start.
    pub fn spawn_vm(
        command: Command,
        vm: VmConfig,
        guest: GuestCommand,
    ) -> Result<Self, VmToolSessionError> {
        Self::spawn_vm_with_shutdown_timeout(command, vm, guest, DEFAULT_SHUTDOWN_TIMEOUT)
    }

    fn spawn_vm_with_shutdown_timeout(
        mut command: Command,
        vm: VmConfig,
        guest: GuestCommand,
        shutdown_timeout: Duration,
    ) -> Result<Self, VmToolSessionError> {
        let process_config = VmProcessConfig::new(vm, guest).write_private()?;
        command.arg(process_config.path());
        let session = Self::spawn_with_shutdown_timeout(&mut command, shutdown_timeout)?;
        *lock_unpoisoned(&session.handle.inner.process_config) = Some(process_config);
        Ok(session)
    }

    /// Configures, spawns, and provisions one VM from the same egress lease.
    ///
    /// `command` must invoke a dedicated VMM process that accepts the private
    /// [`VmProcessConfig`] path as its next argument. This method appends that
    /// path, starts the process, waits for the guest tool server, provisions
    /// public egress files, and retains provider guards with every returned
    /// tool capability.
    ///
    /// Prefer this operation to separately calling [`EgressLease::configure`],
    /// [`Self::spawn`], and [`Self::provision_egress`]: consuming the lease here
    /// prevents launch-time environment and retained provider state from
    /// diverging.
    ///
    /// # Errors
    ///
    /// Returns an error when private configuration cannot be written, the VMM
    /// cannot start, the guest is not ready within 30 seconds, or egress
    /// provisioning fails or exceeds that same startup deadline.
    pub async fn spawn_configured(
        command: Command,
        vm: VmConfig,
        guest: GuestCommand,
        egress: EgressLease,
    ) -> Result<Self, VmToolSessionError> {
        Self::spawn_configured_with_timeouts(
            command,
            vm,
            guest,
            egress,
            DEFAULT_STARTUP_TIMEOUT,
            DEFAULT_SHUTDOWN_TIMEOUT,
        )
        .await
    }

    pub(crate) async fn spawn_configured_with_timeouts(
        command: Command,
        vm: VmConfig,
        guest: GuestCommand,
        egress: EgressLease,
        startup_timeout: Duration,
        shutdown_timeout: Duration,
    ) -> Result<Self, VmToolSessionError> {
        let (vm, guest) = egress.configure(vm, &guest);
        let session = Self::spawn_vm_with_shutdown_timeout(command, vm, guest, shutdown_timeout)?;
        let startup = async {
            session.ready().await?;
            session.provision_egress(egress).await
        };
        match tokio::time::timeout(startup_timeout, startup).await {
            Ok(Ok(())) => Ok(session),
            Ok(Err(error)) => {
                session.terminate().await;
                Err(error)
            }
            Err(_) => {
                session.terminate().await;
                Err(VmToolSessionError::StartupTimeout(startup_timeout))
            }
        }
    }

    /// Spawns a VMM command whose guest process runs the companion guest server.
    ///
    /// The command's stdin and stdout are reserved for the typed protocol;
    /// stderr is passed through while a bounded tail is retained for terminal
    /// protocol failures.
    ///
    /// # Errors
    ///
    /// Returns an error when the child or either protocol pipe cannot be
    /// created.
    pub fn spawn(command: &mut Command) -> Result<Self, VmToolSessionError> {
        Self::spawn_with_shutdown_timeout(command, DEFAULT_SHUTDOWN_TIMEOUT)
    }

    fn spawn_with_shutdown_timeout(
        command: &mut Command,
        shutdown_timeout: Duration,
    ) -> Result<Self, VmToolSessionError> {
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| VmToolSessionError::NoRuntime)?;
        let program = command
            .as_std()
            .get_program()
            .to_string_lossy()
            .into_owned();
        let command_content = format!("{:?}", command.as_std());
        let argument_count = command.as_std().get_args().count();
        let span = info_span!(
            target: "nanocodex_vm",
            "vm.session.spawn",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            process.executable.name = program.as_str(),
            process.command_args.count = argument_count,
            process.id = tracing::field::Empty,
            status = tracing::field::Empty,
            error.message = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        record_vm_content(&span, "vm.command", &command_content);
        let started_at = Instant::now();
        let result = span.in_scope(|| {
            let mut child = command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(VmToolSessionError::Spawn)?;
            if let Some(process_id) = child.id() {
                span.record("process.id", process_id);
            }
            let input = child
                .stdin
                .take()
                .ok_or(VmToolSessionError::MissingPipe("stdin"))?;
            let output = child
                .stdout
                .take()
                .ok_or(VmToolSessionError::MissingPipe("stdout"))?;
            let stderr = child
                .stderr
                .take()
                .ok_or(VmToolSessionError::MissingPipe("stderr"))?;
            let (input_sender, input_receiver) = mpsc::channel(REQUEST_QUEUE_CAPACITY);
            let (cancellation_sender, cancellation_receiver) =
                mpsc::channel(MAX_HOST_IN_FLIGHT_REQUESTS);
            let inner = Arc::new(VmToolSessionInner {
                spawned_at: Instant::now(),
                next_id: AtomicU64::new(0),
                host_peak_rss_kib: AtomicU64::new(0),
                closing: AtomicBool::new(false),
                lifecycle: StdMutex::new(SessionLifecycle::default()),
                input: input_sender,
                cancellations: cancellation_sender,
                request_slots: Arc::new(Semaphore::new(MAX_HOST_IN_FLIGHT_REQUESTS)),
                shutdown_timeout,
                output: Mutex::new(Some(output)),
                pending: StdMutex::new(PendingState::default()),
                terminal: StdMutex::new(TerminalDiagnostics::default()),
                terminal_closed: Notify::new(),
                child: StdMutex::new(Some(child)),
                egress: StdMutex::new(None),
                process_config: StdMutex::new(None),
            });
            runtime.spawn(write_requests(
                input,
                input_receiver,
                cancellation_receiver,
                Arc::downgrade(&inner),
            ));
            runtime.spawn(capture_terminal_stderr(stderr, Arc::downgrade(&inner)));
            if let Some(process_id) = lock_unpoisoned(&inner.child)
                .as_ref()
                .and_then(tokio::process::Child::id)
            {
                runtime.spawn(monitor_host_memory(process_id, Arc::downgrade(&inner)));
            }
            Ok(Self {
                handle: VmToolSessionHandle {
                    inner,
                    counted_capability: false,
                },
            })
        });
        record_vm_result(&span, started_at, &result);
        result
    }

    /// Returns a clone-cheap capability for this session.
    #[must_use]
    pub fn handle(&self) -> VmToolSessionHandle {
        self.handle.clone()
    }

    /// Returns the standard VM-backed workspace tool factory for this session.
    #[must_use]
    pub fn tools(&self) -> super::VmTools {
        super::VmTools::new(self.handle())
    }

    /// Waits until the guest tool server has accepted and answered a typed
    /// readiness request.
    ///
    /// Call this before exposing tools to model work when VM startup failure
    /// should abort setup without spending a model request.
    ///
    /// # Errors
    ///
    /// Returns an error when the VMM exits before the guest server is ready or
    /// the readiness response is malformed.
    pub async fn ready(&self) -> Result<(), VmToolSessionError> {
        self.handle.ready().await
    }

    /// Provisions provider-owned public files and retains the complete egress
    /// lease for the lifetime of this VM session.
    ///
    /// Call this exactly once, after spawning the VMM and before exposing tool
    /// handles to an agent. The same lease must already have been applied to
    /// the VM configuration and guest command with [`EgressLease::configure`].
    ///
    /// # Errors
    ///
    /// Returns an error when egress was already provisioned, a guest path is
    /// not UTF-8, or the guest rejects a file write.
    pub async fn provision_egress(&self, egress: EgressLease) -> Result<(), VmToolSessionError> {
        let files = egress.guest_files().cloned().collect::<Vec<_>>();
        {
            let mut provisioned = lock_unpoisoned(&self.handle.inner.egress);
            if provisioned.is_some() {
                return Err(VmToolSessionError::EgressAlreadyProvisioned);
            }
            // Retain revocable provider state even when provisioning fails.
            // Tool handles keep the lease alive with the VMM, so dropping the
            // launch owner cannot silently revoke an active agent tree.
            *provisioned = Some(egress);
        }
        for file in files {
            let path = file
                .guest_path()
                .to_str()
                .ok_or_else(|| VmToolSessionError::EgressFilePath(file.guest_path().to_owned()))?;
            self.write_file(path, file.contents().to_vec(), file.mode())
                .await?;
        }
        Ok(())
    }

    /// Writes one host-owned file into the guest.
    ///
    /// # Errors
    ///
    /// Returns an error when the console closes, file creation fails in the
    /// guest, or the typed response is invalid.
    pub async fn write_file(
        &self,
        path: impl Into<String>,
        contents: Vec<u8>,
        mode: u32,
    ) -> Result<(), VmToolSessionError> {
        self.handle.write_file(path, contents, mode).await
    }

    /// Writes one host-owned file and applies an exact guest modification time.
    ///
    /// # Errors
    ///
    /// Returns an error when the console closes, file creation or metadata
    /// application fails in the guest, or the typed response is invalid.
    pub async fn write_file_with_mtime(
        &self,
        path: impl Into<String>,
        contents: Vec<u8>,
        mode: u32,
        modified_unix_seconds: i64,
    ) -> Result<(), VmToolSessionError> {
        self.handle
            .write_file_with_mtime(path, contents, mode, modified_unix_seconds)
            .await
    }

    /// Creates or updates one host-owned guest directory.
    ///
    /// `modified_unix_seconds` leaves the current modification time unchanged
    /// when absent.
    ///
    /// # Errors
    ///
    /// Returns an error when the console closes, directory creation or
    /// metadata application fails in the guest, or the typed response is
    /// invalid.
    pub async fn create_directory(
        &self,
        path: impl Into<String>,
        mode: u32,
        modified_unix_seconds: Option<i64>,
    ) -> Result<(), VmToolSessionError> {
        self.handle
            .create_directory(path, mode, modified_unix_seconds)
            .await
    }

    /// Reads one result artifact from the guest.
    ///
    /// # Errors
    ///
    /// Returns an error when the console closes, the file cannot be read, or
    /// the typed response is invalid.
    pub async fn read_file(&self, path: impl Into<String>) -> Result<Vec<u8>, VmToolSessionError> {
        self.handle.read_file(path).await
    }

    /// Executes a trusted host-control command in the guest.
    ///
    /// # Errors
    ///
    /// Returns an error when the console closes, the command cannot run or
    /// exceeds its deadline, or the typed response is invalid.
    pub async fn command(&self, command: VmCommand) -> Result<VmCommandOutput, VmToolSessionError> {
        self.handle.command(command).await
    }

    /// Returns the best-effort peak memory observed for this VM session.
    ///
    /// Host RSS remains available when the guest protocol has already failed.
    /// Missing telemetry is represented by absent fields rather than an error
    /// so diagnostics never obscure the attempt's primary result.
    pub async fn memory_observation(&self) -> VmMemoryObservation {
        self.handle.memory_observation().await
    }

    /// Terminates subprocesses retained by the guest workspace-tool runtime.
    ///
    /// This is a non-destructive agent-lifecycle boundary: the VM, filesystem,
    /// and host-control channel remain available. Processes that deliberately
    /// detached from the workspace tool's managed process group are not
    /// terminated.
    ///
    /// # Errors
    ///
    /// Returns an error when the guest cannot acknowledge cleanup before the
    /// session's shutdown deadline or returns an invalid response.
    pub async fn terminate_tool_processes(&self) -> Result<(), VmToolSessionError> {
        let span = info_span!(
            target: "nanocodex_vm",
            "vm.session.terminate_tool_processes",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            vm.session.age_ns = tracing::field::Empty,
            status = tracing::field::Empty,
            error.message = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        let started_at = Instant::now();
        let timeout = self.handle.inner.shutdown_timeout;
        let result = async {
            let response = tokio::time::timeout(
                timeout,
                self.handle.control_request(|id| {
                    SessionRequest::TerminateToolProcesses(TerminateToolProcessesRequest { id })
                }),
            )
            .await
            .map_err(|_| VmToolSessionError::ToolProcessTerminationTimeout(timeout))??;
            let SessionResponse::TerminateToolProcesses(response) = response else {
                return Err(VmToolSessionError::Protocol(
                    "expected a tool-process termination response",
                ));
            };
            control_result(response)
        }
        .instrument(span.clone())
        .await;
        span.record(
            "vm.session.age_ns",
            elapsed_ns(self.handle.inner.spawned_at),
        );
        record_vm_result(&span, started_at, &result);
        result
    }

    async fn terminate(&self) {
        let child = begin_termination(&self.handle.inner);
        if let Some(mut child) = child {
            let _ = tokio::time::timeout(self.handle.inner.shutdown_timeout, child.wait()).await;
        }
    }

    /// Flushes guest filesystems and waits for the VMM process to exit.
    ///
    /// The operation rejects live sibling capabilities and owner-borrowed
    /// requests so it cannot stop the VM while another driver in the same
    /// agent tree is using it. Because the owner is borrowed, callers can drop
    /// those capabilities or await those requests and retry.
    ///
    /// # Errors
    ///
    /// Returns an error when the guest cannot acknowledge the request, the
    /// VMM does not stop promptly, or it exits unsuccessfully.
    pub async fn shutdown(&self) -> Result<(), VmToolSessionError> {
        {
            let mut lifecycle = lock_unpoisoned(&self.handle.inner.lifecycle);
            if lifecycle.closing {
                return Err(VmToolSessionError::Closed);
            }
            if lifecycle.capabilities != 0 {
                return Err(VmToolSessionError::ActiveCapabilities(
                    lifecycle.capabilities,
                ));
            }
            if lifecycle.in_flight_requests != 0 {
                return Err(VmToolSessionError::ActiveRequests(
                    lifecycle.in_flight_requests,
                ));
            }
            lifecycle.closing = true;
        }
        let mut shutdown_guard = ShutdownGuard {
            inner: Arc::clone(&self.handle.inner),
            armed: true,
        };
        self.handle.inner.closing.store(true, Ordering::Release);
        self.handle.inner.request_slots.close();
        let shutdown_timeout = self.handle.inner.shutdown_timeout;
        let started_at = Instant::now();
        let response = match tokio::time::timeout(
            shutdown_timeout,
            self.handle
                .control_request_inner(|id| SessionRequest::Shutdown(ShutdownRequest { id }), true),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                self.terminate().await;
                return Err(error);
            }
            Err(_) => {
                self.terminate().await;
                return Err(VmToolSessionError::ShutdownTimeout(shutdown_timeout));
            }
        };
        let SessionResponse::Shutdown(response) = response else {
            self.terminate().await;
            return Err(VmToolSessionError::Protocol("expected a shutdown response"));
        };
        if let Err(error) = control_result(response) {
            self.terminate().await;
            return Err(error);
        }

        let child = lock_unpoisoned(&self.handle.inner.child).take();
        let Some(mut child) = child else {
            return Err(VmToolSessionError::Closed);
        };
        let remaining = shutdown_timeout.saturating_sub(started_at.elapsed());
        let status = match tokio::time::timeout(remaining, child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                let _ = child.start_kill();
                let _ = tokio::time::timeout(shutdown_timeout, child.wait()).await;
                return Err(VmToolSessionError::ShutdownTimeout(shutdown_timeout));
            }
        };
        close_pending(&self.handle.inner, "VM session shut down");
        shutdown_guard.armed = false;
        if !status.success() {
            return Err(VmToolSessionError::VmmExit(status));
        }
        Ok(())
    }
}

impl Drop for VmToolSessionInner {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Release);
        close_pending(self, "last VM session capability was dropped");
        let child = lock_unpoisoned(&self.child).take();
        if let Some(mut child) = child {
            let _ = child.start_kill();
            let egress = lock_unpoisoned(&self.egress).take();
            let timeout = self.shutdown_timeout;
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _egress = egress;
                    let _ = tokio::time::timeout(timeout, child.wait()).await;
                });
            }
        }
    }
}

impl Clone for VmToolSessionHandle {
    fn clone(&self) -> Self {
        let mut lifecycle = lock_unpoisoned(&self.inner.lifecycle);
        lifecycle.capabilities = lifecycle.capabilities.saturating_add(1);
        drop(lifecycle);
        Self {
            inner: Arc::clone(&self.inner),
            counted_capability: true,
        }
    }
}

impl Drop for VmToolSessionHandle {
    fn drop(&mut self) {
        if !self.counted_capability {
            return;
        }
        let mut lifecycle = lock_unpoisoned(&self.inner.lifecycle);
        lifecycle.capabilities = lifecycle.capabilities.saturating_sub(1);
    }
}

impl VmToolSessionHandle {
    /// Waits until the guest tool server answers a typed readiness request.
    ///
    /// # Errors
    ///
    /// Returns an error when the VMM exits before the guest server is ready or
    /// the readiness response is malformed.
    pub async fn ready(&self) -> Result<(), VmToolSessionError> {
        let span = info_span!(
            target: "nanocodex_vm",
            "vm.session.ready",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            vm.session.age_ns = tracing::field::Empty,
            status = tracing::field::Empty,
            error.message = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        let started_at = Instant::now();
        let result = async {
            let response = self
                .control_request(|id| SessionRequest::Ready(ReadyRequest { id }))
                .await?;
            let SessionResponse::Ready(response) = response else {
                return Err(VmToolSessionError::Protocol(
                    "expected a readiness response",
                ));
            };
            control_result(response)
        }
        .instrument(span.clone())
        .await;
        span.record("vm.session.age_ns", elapsed_ns(self.inner.spawned_at));
        record_vm_result(&span, started_at, &result);
        result
    }

    /// Returns the best-effort peak memory observed for this VM session.
    pub async fn memory_observation(&self) -> VmMemoryObservation {
        let guest = self.memory_response().await.ok();
        let host_peak_rss_kib = self.inner.host_peak_rss_kib.load(Ordering::Relaxed);
        let terminal_oom =
            terminal_oom_detected(&lock_unpoisoned(&self.inner.terminal).stderr_tail);

        let (guest_total_mib, guest_peak_used_mib, guest_oom_kills) = guest.map_or(
            (None, None, 0),
            |MemoryResponse {
                 total_kib,
                 minimum_available_kib,
                 oom_kills,
                 ..
             }| {
                let peak_used_kib = total_kib.zip(minimum_available_kib).map(
                    |(total_kib, minimum_available_kib)| {
                        total_kib.saturating_sub(minimum_available_kib)
                    },
                );
                (
                    total_kib.map(kib_to_mib_ceil),
                    peak_used_kib.map(kib_to_mib_ceil),
                    oom_kills,
                )
            },
        );
        VmMemoryObservation {
            host_peak_rss_mib: (host_peak_rss_kib != 0).then(|| kib_to_mib_ceil(host_peak_rss_kib)),
            guest_total_mib,
            guest_peak_used_mib,
            guest_oom_kills,
            terminal_oom,
        }
    }

    async fn memory_response(&self) -> Result<MemoryResponse, VmToolSessionError> {
        let response = tokio::time::timeout(
            MEMORY_OBSERVATION_TIMEOUT,
            self.control_request(|id| SessionRequest::Memory(MemoryRequest { id })),
        )
        .await
        .map_err(|_| VmToolSessionError::GuestTimeout {
            timeout: MEMORY_OBSERVATION_TIMEOUT,
            output: VmCommandPartialOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        })??;
        let SessionResponse::Memory(response) = response else {
            return Err(VmToolSessionError::Protocol("expected a memory response"));
        };
        Ok(response)
    }

    async fn request(
        &self,
        tool: StandardTool,
        input: ToolInput,
        context: ToolContext<'_>,
    ) -> Result<ToolOutput, VmToolSessionError> {
        let (input_kind, input_bytes) = match &input {
            ToolInput::Function(arguments) => ("function", arguments.get().len()),
            ToolInput::Freeform(input) => ("freeform", input.len()),
        };
        let span = info_span!(
            target: "nanocodex_vm",
            "vm.tool.rpc",
            otel.kind = "client",
            otel.status_code = tracing::field::Empty,
            rpc.system = "libkrun.console",
            rpc.method = tool.name(),
            tool.name = tool.name(),
            session.id = context.session_id(),
            tool.call_id = context.call_id(),
            tool.input.kind = input_kind,
            tool.input.bytes = input_bytes,
            rpc.request.id = tracing::field::Empty,
            rpc.request.bytes = tracing::field::Empty,
            rpc.response.bytes = tracing::field::Empty,
            rpc.admission.duration_ns = tracing::field::Empty,
            rpc.queue.duration_ns = tracing::field::Empty,
            vm.session.first_call = tracing::field::Empty,
            vm.session.age_ns = tracing::field::Empty,
            tool.success = tracing::field::Empty,
            status = tracing::field::Empty,
            error.message = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        let started_at = Instant::now();
        let result = self
            .request_inner(tool, input, context, &span)
            .instrument(span.clone())
            .await;
        if let Ok(execution) = &result {
            span.record("tool.success", execution.success);
        }
        record_vm_result(&span, started_at, &result);
        result
    }

    async fn request_inner(
        &self,
        tool: StandardTool,
        input: ToolInput,
        context: ToolContext<'_>,
        span: &tracing::Span,
    ) -> Result<ToolOutput, VmToolSessionError> {
        let request = SessionRequest::Tool(ToolRequest {
            id: 0,
            tool,
            input: WireToolInput::from(input),
            context: WireToolContext {
                model: context.model().to_owned(),
                session_id: context.session_id().to_owned(),
                call_id: context.call_id().to_owned(),
                output_token_budget: context.output_token_budget(),
            },
        });
        let (response, response_bytes) = self.send_request(request, span, false).await?;
        span.record("rpc.response.bytes", response_bytes);
        span.record("vm.session.age_ns", elapsed_ns(self.inner.spawned_at));
        let SessionResponse::Tool(response) = response else {
            return Err(VmToolSessionError::Protocol("expected a tool response"));
        };
        match (response.execution, response.error) {
            (Some(execution), None) => ToolOutput::from_wire(execution).map_err(Into::into),
            (None, Some(error)) => Err(VmToolSessionError::Guest(error)),
            _ => Err(VmToolSessionError::Protocol(
                "expected exactly one of execution or error",
            )),
        }
    }

    /// Writes one host-owned file into the guest.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is closed, guest file creation fails,
    /// or the response is invalid.
    pub async fn write_file(
        &self,
        path: impl Into<String>,
        contents: Vec<u8>,
        mode: u32,
    ) -> Result<(), VmToolSessionError> {
        self.write_file_inner(path, contents, mode, None).await
    }

    /// Writes one host-owned file and applies an exact guest modification time.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is closed, guest file creation or
    /// metadata application fails, or the response is invalid.
    pub async fn write_file_with_mtime(
        &self,
        path: impl Into<String>,
        contents: Vec<u8>,
        mode: u32,
        modified_unix_seconds: i64,
    ) -> Result<(), VmToolSessionError> {
        self.write_file_inner(path, contents, mode, Some(modified_unix_seconds))
            .await
    }

    async fn write_file_inner(
        &self,
        path: impl Into<String>,
        contents: Vec<u8>,
        mode: u32,
        modified_unix_seconds: Option<i64>,
    ) -> Result<(), VmToolSessionError> {
        let response = self
            .control_request(|id| {
                SessionRequest::WriteFile(WriteFileRequest {
                    id,
                    path: path.into(),
                    contents,
                    mode,
                    modified_unix_seconds,
                })
            })
            .await?;
        let SessionResponse::WriteFile(response) = response else {
            return Err(VmToolSessionError::Protocol(
                "expected a write-file response",
            ));
        };
        control_result(response)
    }

    /// Creates or updates one host-owned guest directory.
    ///
    /// `modified_unix_seconds` leaves the current modification time unchanged
    /// when absent.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is closed, guest directory creation
    /// or metadata application fails, or the response is invalid.
    pub async fn create_directory(
        &self,
        path: impl Into<String>,
        mode: u32,
        modified_unix_seconds: Option<i64>,
    ) -> Result<(), VmToolSessionError> {
        let response = self
            .control_request(|id| {
                SessionRequest::CreateDirectory(CreateDirectoryRequest {
                    id,
                    path: path.into(),
                    mode,
                    modified_unix_seconds,
                })
            })
            .await?;
        let SessionResponse::CreateDirectory(response) = response else {
            return Err(VmToolSessionError::Protocol(
                "expected a create-directory response",
            ));
        };
        control_result(response)
    }

    /// Reads one host-owned artifact from the guest.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is closed, the file cannot be read,
    /// or the response is invalid.
    pub async fn read_file(&self, path: impl Into<String>) -> Result<Vec<u8>, VmToolSessionError> {
        let response = self
            .control_request(|id| {
                SessionRequest::ReadFile(ReadFileRequest {
                    id,
                    path: path.into(),
                })
            })
            .await?;
        let SessionResponse::ReadFile(ReadFileResponse {
            contents, error, ..
        }) = response
        else {
            return Err(VmToolSessionError::Protocol(
                "expected a read-file response",
            ));
        };
        match (contents, error) {
            (Some(contents), None) => Ok(contents),
            (None, Some(error)) => Err(VmToolSessionError::Guest(error)),
            _ => Err(VmToolSessionError::Protocol(
                "expected exactly one of contents or error",
            )),
        }
    }

    /// Executes one trusted host-control command in the guest.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is closed, the command fails to
    /// start, exceeds its deadline, or returns an invalid response.
    pub async fn command(&self, command: VmCommand) -> Result<VmCommandOutput, VmToolSessionError> {
        let command_timeout = command.timeout;
        let max_output_bytes = command.max_output_bytes;
        let timeout_millis = u64::try_from(command_timeout.as_millis()).unwrap_or(u64::MAX);
        let response = self
            .control_request(|id| {
                SessionRequest::Execute(ExecuteRequest {
                    id,
                    program: command.program,
                    arguments: command.arguments,
                    current_directory: command.current_directory,
                    environment: command.environment,
                    timeout_millis,
                    max_output_bytes,
                    stdout_mirror: command.stdout_mirror,
                    stderr_mirror: command.stderr_mirror,
                })
            })
            .await?;
        let SessionResponse::Execute(ExecuteResponse {
            exit_code,
            stdout,
            stderr,
            error,
            timed_out,
            output_limit_exceeded,
            ..
        }) = response
        else {
            return Err(VmToolSessionError::Protocol("expected an execute response"));
        };
        match (
            exit_code,
            stdout,
            stderr,
            error,
            timed_out,
            output_limit_exceeded,
        ) {
            (Some(exit_code), Some(stdout), Some(stderr), None, false, false) => {
                Ok(VmCommandOutput {
                    exit_code,
                    stdout,
                    stderr,
                })
            }
            (None, Some(stdout), Some(stderr), None, true, false) => {
                Err(VmToolSessionError::GuestTimeout {
                    timeout: command_timeout,
                    output: VmCommandPartialOutput { stdout, stderr },
                })
            }
            (None, None, None, None, false, true) => {
                Err(VmToolSessionError::GuestOutputLimit(max_output_bytes))
            }
            (None, None, None, Some(error), false, false) => Err(VmToolSessionError::Guest(error)),
            _ => Err(VmToolSessionError::Protocol(
                "invalid execute response fields",
            )),
        }
    }

    async fn control_request(
        &self,
        make_request: impl FnOnce(u64) -> SessionRequest,
    ) -> Result<SessionResponse, VmToolSessionError> {
        self.control_request_inner(make_request, false).await
    }

    async fn control_request_inner(
        &self,
        make_request: impl FnOnce(u64) -> SessionRequest,
        allow_closing: bool,
    ) -> Result<SessionResponse, VmToolSessionError> {
        let response = self
            .send_request(make_request(0), &Span::current(), allow_closing)
            .await?
            .0;
        Ok(response)
    }

    async fn send_request(
        &self,
        mut request: SessionRequest,
        span: &Span,
        allow_closing: bool,
    ) -> Result<(SessionResponse, usize), VmToolSessionError> {
        if self.inner.closing.load(Ordering::Acquire) && !allow_closing {
            return Err(self.closed_error());
        }
        let request_slot = if allow_closing {
            None
        } else {
            let admission_started_at = Instant::now();
            let permit = Arc::clone(&self.inner.request_slots)
                .acquire_owned()
                .await
                .map_err(|_| self.closed_error())?;
            span.record(
                "rpc.admission.duration_ns",
                elapsed_ns(admission_started_at),
            );
            if self.inner.closing.load(Ordering::Acquire) {
                return Err(self.closed_error());
            }
            Some(permit)
        };
        let _in_flight = if allow_closing {
            None
        } else {
            let mut lifecycle = lock_unpoisoned(&self.inner.lifecycle);
            if lifecycle.closing {
                drop(lifecycle);
                return Err(self.closed_error());
            }
            lifecycle.in_flight_requests = lifecycle.in_flight_requests.saturating_add(1);
            drop(lifecycle);
            Some(InFlightRequestGuard {
                inner: Arc::downgrade(&self.inner),
            })
        };
        self.ensure_reader().await?;
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        set_request_id(&mut request, id);
        span.record("rpc.request.id", id);
        span.record("vm.session.first_call", id == 0);
        let encoded = serde_json::to_string(&request)?;
        if encoded.len() > MAX_FRAME_BYTES {
            return Err(VmToolSessionError::FrameTooLarge);
        }
        span.record("rpc.request.bytes", encoded.len());
        record_vm_content(span, "tool.request", &encoded);

        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = lock_unpoisoned(&self.inner.pending);
            if let Some(error) = &pending.closed {
                return Err(VmToolSessionError::Router(error.clone()));
            }
            pending.requests.insert(
                id,
                PendingResponse {
                    span: span.clone(),
                    response: sender,
                },
            );
        }
        let written = Arc::new(AtomicBool::new(false));
        let mut guard = PendingRequestGuard {
            inner: Arc::downgrade(&self.inner),
            id,
            armed: true,
            queued: false,
            written: Arc::clone(&written),
            request_slot,
        };
        let queued_at = Instant::now();
        let mut frame = encoded.into_bytes();
        frame.push(b'\n');
        self.inner
            .input
            .send(OutboundRequest { id, frame, written })
            .await
            .map_err(|_| self.closed_error())?;
        guard.queued = true;
        span.record("rpc.queue.duration_ns", elapsed_ns(queued_at));
        let response = receiver.await.map_err(|_| self.closed_error())?;
        guard.armed = false;
        response.map_err(VmToolSessionError::Router)
    }

    async fn ensure_reader(&self) -> Result<(), VmToolSessionError> {
        let mut output = self.inner.output.lock().await;
        if let Some(output) = output.take() {
            let inner = Arc::downgrade(&self.inner);
            tokio::spawn(async move {
                route_responses(output, inner).await;
            });
            return Ok(());
        }
        let pending = lock_unpoisoned(&self.inner.pending);
        match &pending.closed {
            Some(error) => Err(VmToolSessionError::Router(error.clone())),
            None => Ok(()),
        }
    }

    fn closed_error(&self) -> VmToolSessionError {
        lock_unpoisoned(&self.inner.pending)
            .closed
            .clone()
            .map_or(VmToolSessionError::Closed, VmToolSessionError::Router)
    }
}

impl Drop for PendingRequestGuard {
    fn drop(&mut self) {
        if self.armed
            && let Some(inner) = self.inner.upgrade()
        {
            lock_unpoisoned(&inner.pending).requests.remove(&self.id);
            if self.queued
                && let Some(request_slot) = self.request_slot.take()
            {
                queue_cancel(&inner, self.id, Arc::clone(&self.written), request_slot);
            }
        }
    }
}

impl Drop for InFlightRequestGuard {
    fn drop(&mut self) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let mut lifecycle = lock_unpoisoned(&inner.lifecycle);
        lifecycle.in_flight_requests = lifecycle.in_flight_requests.saturating_sub(1);
    }
}

impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let child = begin_termination(&self.inner);
        let Some(mut child) = child else {
            return;
        };
        let timeout = self.inner.shutdown_timeout;
        let egress = lock_unpoisoned(&self.inner.egress).clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _egress = egress;
                let _ = tokio::time::timeout(timeout, child.wait()).await;
            });
        }
    }
}

fn begin_termination(inner: &Arc<VmToolSessionInner>) -> Option<Child> {
    {
        let mut lifecycle = lock_unpoisoned(&inner.lifecycle);
        lifecycle.closing = true;
    }
    inner.closing.store(true, Ordering::Release);
    inner.request_slots.close();
    close_pending(inner, "VM session terminated");
    let mut child = lock_unpoisoned(&inner.child).take()?;
    let _ = child.start_kill();
    Some(child)
}

fn queue_cancel(
    inner: &Arc<VmToolSessionInner>,
    target_id: u64,
    written: Arc<AtomicBool>,
    request_slot: OwnedSemaphorePermit,
) {
    if inner.closing.load(Ordering::Acquire) {
        return;
    }
    let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
    let request = SessionRequest::Cancel(CancelRequest { id, target_id });
    let Ok(mut frame) = serde_json::to_vec(&request) else {
        return;
    };
    frame.push(b'\n');
    let cancellation = OutboundCancellation {
        target_id,
        frame,
        written,
        _request_slot: request_slot,
    };
    match inner.cancellations.try_send(cancellation) {
        Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            inner.closing.store(true, Ordering::Release);
            inner.request_slots.close();
            close_pending(
                inner,
                "bounded VM cancellation queue exhausted unexpectedly",
            );
        }
    }
}

async fn write_requests(
    mut input: ChildStdin,
    mut requests: mpsc::Receiver<OutboundRequest>,
    mut cancellations: mpsc::Receiver<OutboundCancellation>,
    inner: Weak<VmToolSessionInner>,
) {
    let mut deferred_cancellations = HashMap::<u64, OutboundCancellation>::new();
    loop {
        enum Outbound {
            Request(OutboundRequest),
            Cancellation(OutboundCancellation),
        }
        let outbound = tokio::select! {
            biased;
            Some(cancellation) = cancellations.recv() => {
                Outbound::Cancellation(cancellation)
            }
            Some(request) = requests.recv() => Outbound::Request(request),
            else => break,
        };
        let result = match outbound {
            Outbound::Cancellation(cancellation)
                if !cancellation.written.load(Ordering::Acquire) =>
            {
                deferred_cancellations.insert(cancellation.target_id, cancellation);
                continue;
            }
            Outbound::Cancellation(cancellation) => {
                write_request_frame(&mut input, &cancellation.frame).await
            }
            Outbound::Request(request) => {
                let result = write_request_frame(&mut input, &request.frame).await;
                if result.is_ok() {
                    request.written.store(true, Ordering::Release);
                    if let Some(cancellation) = deferred_cancellations.remove(&request.id)
                        && let Err(error) =
                            write_request_frame(&mut input, &cancellation.frame).await
                    {
                        break close_writer_after_error(&inner, error).await;
                    }
                }
                result
            }
        };
        if let Err(error) = result {
            close_writer_after_error(&inner, error).await;
            break;
        }
    }
}

async fn write_request_frame(input: &mut ChildStdin, frame: &[u8]) -> io::Result<()> {
    input.write_all(frame).await?;
    input.flush().await
}

async fn close_writer_after_error(inner: &Weak<VmToolSessionInner>, error: io::Error) {
    if let Some(inner) = inner.upgrade() {
        let message =
            terminal_router_message(&inner, &format!("VM tool console write failed: {error}"))
                .await;
        close_pending(&inner, &message);
    }
}

async fn capture_terminal_stderr(mut stderr: ChildStderr, inner: Weak<VmToolSessionInner>) {
    let mut passthrough = tokio::io::stderr();
    let mut buffer = [0_u8; 8 * 1024];
    let stderr_error = loop {
        match stderr.read(&mut buffer).await {
            Ok(0) => break None,
            Ok(read) => {
                let _ = passthrough.write_all(&buffer[..read]).await;
                if let Some(inner) = inner.upgrade() {
                    append_terminal_stderr(&mut lock_unpoisoned(&inner.terminal), &buffer[..read]);
                }
            }
            Err(error) => break Some(error.to_string()),
        }
    };
    let _ = passthrough.flush().await;
    if let Some(inner) = inner.upgrade() {
        let mut terminal = lock_unpoisoned(&inner.terminal);
        terminal.stderr_error = stderr_error;
        terminal.closed = true;
        drop(terminal);
        inner.terminal_closed.notify_waiters();
    }
}

#[cfg(target_os = "linux")]
async fn monitor_host_memory(process_id: u32, inner: Weak<VmToolSessionInner>) {
    loop {
        let Some(inner) = inner.upgrade() else {
            return;
        };
        match tokio::fs::read_to_string(format!("/proc/{process_id}/status")).await {
            Ok(status) => {
                if let Some(rss_kib) = parse_linux_process_rss_kib(&status) {
                    inner
                        .host_peak_rss_kib
                        .fetch_max(rss_kib, Ordering::Relaxed);
                }
            }
            Err(_) if inner.closing.load(Ordering::Acquire) => return,
            Err(_) => {}
        }
        tokio::time::sleep(HOST_MEMORY_SAMPLE_INTERVAL).await;
    }
}

#[cfg(not(target_os = "linux"))]
async fn monitor_host_memory(_process_id: u32, _inner: Weak<VmToolSessionInner>) {}

#[cfg(any(target_os = "linux", test))]
fn parse_linux_process_rss_kib(status: &str) -> Option<u64> {
    status
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            matches!(key, "VmRSS" | "VmHWM")
                .then(|| value.split_whitespace().next()?.parse::<u64>().ok())
                .flatten()
        })
        .max()
}

fn terminal_oom_detected(stderr: &[u8]) -> bool {
    let stderr = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    [
        "out of memory: killed process",
        "memory cgroup out of memory",
        "oom-kill:",
    ]
    .iter()
    .any(|diagnostic| stderr.contains(diagnostic))
}

const fn kib_to_mib_ceil(kib: u64) -> u64 {
    kib.saturating_add(KIB_PER_MIB - 1) / KIB_PER_MIB
}

fn append_terminal_stderr(terminal: &mut TerminalDiagnostics, bytes: &[u8]) {
    if bytes.len() >= MAX_TERMINAL_STDERR_BYTES {
        terminal.stderr_tail.clear();
        terminal
            .stderr_tail
            .extend_from_slice(&bytes[bytes.len() - MAX_TERMINAL_STDERR_BYTES..]);
        return;
    }
    let overflow = terminal
        .stderr_tail
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(MAX_TERMINAL_STDERR_BYTES);
    if overflow != 0 {
        terminal.stderr_tail.drain(..overflow);
    }
    terminal.stderr_tail.extend_from_slice(bytes);
}

async fn route_responses(output: ChildStdout, inner: Weak<VmToolSessionInner>) {
    let mut output = BufReader::new(output);
    loop {
        let line = match read_frame(&mut output).await {
            Ok(Some(line)) => line,
            Ok(None) => {
                if let Some(inner) = inner.upgrade() {
                    let message = terminal_router_message(&inner, "VM tool console closed").await;
                    close_pending(&inner, &message);
                }
                return;
            }
            Err(error) => {
                if let Some(inner) = inner.upgrade() {
                    let message = terminal_router_message(
                        &inner,
                        &format!("VM tool console read failed: {error}"),
                    )
                    .await;
                    close_pending(&inner, &message);
                }
                return;
            }
        };
        let response = match serde_json::from_slice::<SessionResponse>(&line) {
            Ok(response) => response,
            Err(error) => {
                if let Some(inner) = inner.upgrade() {
                    close_pending(&inner, &format!("invalid VM tool response: {error}"));
                }
                return;
            }
        };
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let id = response.id();
        let pending = lock_unpoisoned(&inner.pending).requests.remove(&id);
        if let Some(pending) = pending {
            record_vm_content(
                &pending.span,
                "tool.response",
                &String::from_utf8_lossy(&line),
            );
            let _ = pending.response.send(Ok((response, line.len())));
        } else {
            info!(
                target: "nanocodex_vm",
                rpc_response_id = id,
                "discarded response for a cancelled VM request"
            );
        }
    }
}

async fn terminal_router_message(inner: &Arc<VmToolSessionInner>, base: &str) -> String {
    let wait_for_stderr = async {
        loop {
            let closed = inner.terminal_closed.notified();
            if lock_unpoisoned(&inner.terminal).closed {
                return;
            }
            closed.await;
        }
    };
    let _ = tokio::time::timeout(TERMINAL_STDERR_DRAIN_GRACE, wait_for_stderr).await;

    let status = lock_unpoisoned(&inner.child)
        .as_mut()
        .and_then(|child| child.try_wait().ok().flatten());
    let terminal = lock_unpoisoned(&inner.terminal);
    let stderr = String::from_utf8_lossy(&terminal.stderr_tail);
    let stderr = stderr.trim();
    let mut details = Vec::new();
    if let Some(status) = status {
        details.push(format!("VMM exited with {status}"));
    }
    if !stderr.is_empty() {
        details.push(format!("VMM stderr: {stderr}"));
    }
    if let Some(error) = &terminal.stderr_error {
        details.push(format!("reading VMM stderr failed: {error}"));
    }
    if details.is_empty() {
        base.to_owned()
    } else {
        format!("{base}; {}", details.join("; "))
    }
}

async fn read_frame(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> Result<Option<Vec<u8>>, VmToolSessionError> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(VmToolSessionError::Closed)
            };
        }
        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if frame.len().saturating_add(newline) > MAX_FRAME_BYTES {
                return Err(VmToolSessionError::FrameTooLarge);
            }
            frame.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            return Ok(Some(frame));
        }
        if frame.len().saturating_add(available.len()) > MAX_FRAME_BYTES {
            return Err(VmToolSessionError::FrameTooLarge);
        }
        let consumed = available.len();
        frame.extend_from_slice(available);
        reader.consume(consumed);
    }
}

const fn set_request_id(request: &mut SessionRequest, id: u64) {
    match request {
        SessionRequest::Ready(request) => request.id = id,
        SessionRequest::Tool(request) => request.id = id,
        SessionRequest::WriteFile(request) => request.id = id,
        SessionRequest::CreateDirectory(request) => request.id = id,
        SessionRequest::ReadFile(request) => request.id = id,
        SessionRequest::Memory(request) => request.id = id,
        SessionRequest::Execute(request) => request.id = id,
        SessionRequest::Cancel(request) => request.id = id,
        SessionRequest::TerminateToolProcesses(request) => request.id = id,
        SessionRequest::Shutdown(request) => request.id = id,
    }
}

fn close_pending(inner: &VmToolSessionInner, message: &str) {
    inner.request_slots.close();
    let requests = {
        let mut pending = lock_unpoisoned(&inner.pending);
        if pending.closed.is_none() {
            pending.closed = Some(message.to_owned());
        }
        std::mem::take(&mut pending.requests)
    };
    for (_, request) in requests {
        let _ = request.response.send(Err(message.to_owned()));
    }
}

fn lock_unpoisoned<T>(mutex: &StdMutex<T>) -> StdMutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn control_result(response: ControlResponse) -> Result<(), VmToolSessionError> {
    match response.error {
        None => Ok(()),
        Some(error) => Err(VmToolSessionError::Guest(error)),
    }
}

fn record_vm_result<T, E>(span: &tracing::Span, started_at: Instant, result: &Result<T, E>)
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
                    target: "nanocodex_vm",
                    duration_ns,
                    status = "completed",
                    "VM operation completed"
                );
            });
        }
        Err(error) => {
            span.record("status", "failed");
            span.record("otel.status_code", "ERROR");
            span.record("error.message", tracing::field::display(error));
            span.in_scope(|| {
                info!(
                    target: "nanocodex_vm",
                    duration_ns,
                    status = "failed",
                    error = %error,
                    "VM operation failed"
                );
            });
        }
    }
}

fn record_vm_content(span: &tracing::Span, kind: &'static str, content: &str) {
    span.in_scope(|| {
        info!(
            target: "nanocodex_vm",
            content_kind = kind,
            content,
            "VM tool content"
        );
    });
}

fn elapsed_ns(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[async_trait::async_trait]
impl VmToolClient for VmToolSessionHandle {
    async fn execute(
        &self,
        tool: StandardTool,
        input: ToolInput,
        context: ToolContext<'_>,
    ) -> ToolResult {
        match self.request(tool, input, context).await {
            Ok(execution) => Ok(execution),
            Err(diagnostic @ VmToolSessionError::Router(_)) => {
                Err(Box::new(ModelSafeVmToolError { diagnostic }))
            }
            Err(error) => Err(Box::new(error)),
        }
    }
}

#[cfg(test)]
mod tracing_tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex, atomic::Ordering},
        time::{Duration, Instant},
    };

    use nanocodex_tools::{
        ToolContext, ToolInput, contract::ToolOutputBody, runtime::ToolRuntime,
        standard::StandardTool,
    };
    use serde_json::{json, value::to_raw_value};
    use tracing::{Id, Instrument, Subscriber, field::Visit, span::Attributes};
    use tracing_subscriber::{
        Layer, layer::Context as LayerContext, prelude::*, registry::LookupSpan,
    };

    use super::{
        VmCommand, VmCommandPartialOutput, VmToolSession, VmToolSessionError, kib_to_mib_ceil,
        lock_unpoisoned, parse_linux_process_rss_kib, terminal_oom_detected,
    };

    static TRACE_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn parses_peak_linux_vmm_rss_and_confirmed_oom_diagnostics() {
        let status = "Name:\tvm-run\nVmHWM:\t77824 kB\nVmRSS:\t65536 kB\n";
        assert_eq!(parse_linux_process_rss_kib(status), Some(77_824));
        assert_eq!(kib_to_mib_ceil(77_824), 76);
        assert!(terminal_oom_detected(
            b"kernel: Out of memory: Killed process 42 (agent)"
        ));
        assert!(!terminal_oom_detected(b"guest command exited 137"));
    }

    #[derive(Clone, Default)]
    struct TraceCapture {
        spans: Arc<Mutex<HashMap<u64, CapturedSpan>>>,
        names: Arc<Mutex<Vec<&'static str>>>,
    }

    struct CapturedSpan {
        name: &'static str,
        parent: Option<u64>,
        fields: HashMap<String, String>,
    }

    struct FieldCapture<'a>(&'a mut HashMap<String, String>);

    impl Visit for FieldCapture<'_> {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.insert(field.name().to_owned(), value.to_owned());
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.insert(field.name().to_owned(), format!("{value:?}"));
        }
    }

    impl<S> Layer<S> for TraceCapture
    where
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    {
        fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: LayerContext<'_, S>) {
            self.names
                .lock()
                .unwrap()
                .push(attributes.metadata().name());
            let parent = attributes
                .parent()
                .map(|parent| parent.clone().into_u64())
                .or_else(|| {
                    attributes
                        .is_contextual()
                        .then(|| context.current_span().id().map(Id::into_u64))
                        .flatten()
                });
            let mut fields = HashMap::new();
            attributes.record(&mut FieldCapture(&mut fields));
            self.spans.lock().unwrap().insert(
                id.clone().into_u64(),
                CapturedSpan {
                    name: attributes.metadata().name(),
                    parent,
                    fields,
                },
            );
        }

        fn on_record(
            &self,
            id: &Id,
            values: &tracing::span::Record<'_>,
            _context: LayerContext<'_, S>,
        ) {
            if let Some(span) = self.spans.lock().unwrap().get_mut(&id.clone().into_u64()) {
                values.record(&mut FieldCapture(&mut span.fields));
            }
        }
    }

    #[test]
    fn spawning_without_a_tokio_runtime_returns_a_typed_error() {
        let mut command = tokio::process::Command::new("/bin/true");

        assert!(matches!(
            VmToolSession::spawn(&mut command),
            Err(VmToolSessionError::NoRuntime)
        ));
    }

    #[test]
    fn vm_rpc_is_timed_parented_and_records_admission_wait() {
        let _test_guard = TRACE_TEST_LOCK.lock().unwrap();
        let response = r#"{"kind":"tool","payload":{"id":0,"execution":{"output":"ok","success":true,"structured_result":null,"metadata":null,"process_trace":null},"error":null}}"#;
        let script = format!("IFS= read -r request\nprintf '%s\\n' '{response}'");
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg(script);
        let capture = TraceCapture::default();
        let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(capture.clone()));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        tracing::dispatcher::with_default(&dispatch, || {
            // Earlier tests may have registered these static callsites while
            // no subscriber was active. Rebuild only after installing this
            // dispatch so cached `never` interest cannot make the assertion
            // order-dependent.
            tracing::callsite::rebuild_interest_cache();
            runtime.block_on(async {
                let session = VmToolSession::spawn(&mut command).unwrap();
                let handle = session.handle();
                let held_slots = handle
                    .inner
                    .request_slots
                    .acquire_many(super::MAX_HOST_IN_FLIGHT_REQUESTS as u32)
                    .await
                    .unwrap();
                let context =
                    ToolContext::new("test-model", "test-session", "test-call", &[], 1_000);
                let request = handle
                    .request(
                        StandardTool::ExecCommand,
                        ToolInput::Function(to_raw_value(&json!({"cmd": "true"})).unwrap()),
                        context,
                    )
                    .instrument(tracing::info_span!("test.tool.execute"));
                let release_slots = async move {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    drop(held_slots);
                };
                let (execution, ()) = tokio::join!(request, release_slots);
                let execution = execution.unwrap();
                assert!(execution.success);
            });
        });

        let spans = capture.spans.lock().unwrap();
        let (tool_id, _) = spans
            .iter()
            .find(|(_, span)| span.name == "test.tool.execute")
            .unwrap();
        let rpc = spans
            .values()
            .find(|span| span.name == "vm.tool.rpc")
            .unwrap();
        assert_eq!(rpc.parent, Some(*tool_id));
        assert_eq!(
            rpc.fields.get("status").map(String::as_str),
            Some("completed")
        );
        assert_eq!(
            rpc.fields.get("vm.session.first_call").map(String::as_str),
            Some("true")
        );
        let admission_duration_ns = rpc
            .fields
            .get("rpc.admission.duration_ns")
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert!(admission_duration_ns >= 15_000_000);
        assert!(rpc.fields.contains_key("rpc.queue.duration_ns"));
        assert!(rpc.fields.contains_key("duration_ns"));
        assert!(capture.names.lock().unwrap().contains(&"vm.session.spawn"));
    }

    #[test]
    fn readiness_waits_for_a_typed_guest_response() {
        let _test_guard = TRACE_TEST_LOCK.lock().unwrap();
        let response = r#"{"kind":"ready","payload":{"id":0,"error":null}}"#;
        let script = format!("IFS= read -r request\nprintf '%s\\n' '{response}'");
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg(script);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let session = VmToolSession::spawn(&mut command).unwrap();
            session.ready().await.unwrap();
        });
    }

    #[test]
    fn managed_tool_process_termination_keeps_the_vm_session_open() {
        let _test_guard = TRACE_TEST_LOCK.lock().unwrap();
        let terminated = r#"{"kind":"terminate_tool_processes","payload":{"id":0,"error":null}}"#;
        let ready = r#"{"kind":"ready","payload":{"id":1,"error":null}}"#;
        let shutdown = r#"{"kind":"shutdown","payload":{"id":2,"error":null}}"#;
        let script = format!(
            "IFS= read -r terminate\n\
             case \"$terminate\" in *'\"kind\":\"terminate_tool_processes\"'*) ;; *) exit 91 ;; esac\n\
             printf '%s\\n' '{terminated}'\n\
             IFS= read -r ready\n\
             printf '%s\\n' '{ready}'\n\
             IFS= read -r shutdown\n\
             printf '%s\\n' '{shutdown}'"
        );
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg(script);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let session = VmToolSession::spawn(&mut command).unwrap();
            session.terminate_tool_processes().await.unwrap();
            session.ready().await.unwrap();
            session.shutdown().await.unwrap();
        });
    }

    #[test]
    fn model_tool_error_hides_terminal_stderr_while_operator_error_retains_it() {
        const SENTINEL: &str = "SENTINEL_VM_SECRET_7f35ad";

        let _test_guard = TRACE_TEST_LOCK.lock().unwrap();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg(format!(
            "IFS= read -r request\nprintf '%s\\n' 'guest runtime failed: {SENTINEL}' >&2\nexit 23"
        ));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let session = VmToolSession::spawn(&mut command).unwrap();
            let tools = session
                .tools()
                .tools_builder()
                .web_search(false)
                .image_generation(false)
                .build()
                .unwrap();
            let tool_runtime = ToolRuntime::new_with_tools("/", None, None, &tools);
            let output = tool_runtime
                .execute_tool(
                    StandardTool::ExecCommand.name(),
                    ToolInput::Function(to_raw_value(&json!({"cmd": "true"})).unwrap()),
                    ToolContext::new("model", "session", "call", &[], 1_000),
                )
                .await;
            assert!(!output.success);
            let ToolOutputBody::Text(model_error) = output.output else {
                panic!("tool registry should produce a model-visible text error");
            };
            assert_eq!(
                model_error,
                "VM tool session ended before this request completed"
            );
            assert!(!model_error.contains(SENTINEL));

            let error = session.ready().await.unwrap_err();
            let VmToolSessionError::Router(message) = error else {
                panic!("expected a terminal router failure");
            };
            assert!(message.contains("VM tool console closed"));
            assert!(message.contains(SENTINEL));
        });
    }

    #[test]
    fn cancelled_request_sends_targeted_guest_cancellation() {
        let _test_guard = TRACE_TEST_LOCK.lock().unwrap();
        let cancel = r#"{"kind":"cancel","payload":{"id":1,"error":null}}"#;
        let second = r#"{"kind":"write_file","payload":{"id":2,"error":null}}"#;
        let script = format!(
            "IFS= read -r first\nIFS= read -r cancel\nprintf '%s\\n' '{cancel}'\nIFS= read -r second\nprintf '%s\\n' '{second}'"
        );
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg(script);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let session = VmToolSession::spawn(&mut command).unwrap();
            let cancelled = tokio::time::timeout(
                Duration::from_millis(10),
                session.write_file("/first", Vec::new(), 0o600),
            )
            .await;
            assert!(cancelled.is_err());

            session
                .write_file("/second", Vec::new(), 0o600)
                .await
                .unwrap();
        });
    }

    #[test]
    fn command_timeout_preserves_bounded_guest_output() {
        let _test_guard = TRACE_TEST_LOCK.lock().unwrap();
        let response = r#"{"kind":"execute","payload":{"id":0,"exit_code":null,"stdout":"cGFydGlhbCBzdGRvdXQ=","stderr":"cGFydGlhbCBzdGRlcnI=","error":null,"timed_out":true,"output_limit_exceeded":false}}"#;
        let script = format!("IFS= read -r request\nprintf '%s\\n' '{response}'");
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg(script);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let session = VmToolSession::spawn(&mut command).unwrap();
            let error = session
                .command(VmCommand::new("/bin/true").timeout(Duration::from_secs(17)))
                .await
                .unwrap_err();
            let VmToolSessionError::GuestTimeout { timeout, output } = error else {
                panic!("expected a typed guest timeout");
            };
            assert_eq!(timeout, Duration::from_secs(17));
            assert_eq!(
                output,
                VmCommandPartialOutput {
                    stdout: b"partial stdout".to_vec(),
                    stderr: b"partial stderr".to_vec(),
                }
            );
        });
    }

    #[test]
    fn concurrent_handles_multiplex_out_of_order_responses() {
        let _test_guard = TRACE_TEST_LOCK.lock().unwrap();
        let first = r#"{"kind":"write_file","payload":{"id":1,"error":null}}"#;
        let second = r#"{"kind":"write_file","payload":{"id":0,"error":null}}"#;
        let script =
            format!("IFS= read -r first\nIFS= read -r second\nprintf '%s\\n' '{first}' '{second}'");
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg(script);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let session = VmToolSession::spawn(&mut command).unwrap();
            let completed = tokio::time::timeout(Duration::from_secs(1), async {
                tokio::join!(
                    session.write_file("/first", Vec::new(), 0o600),
                    session.write_file("/second", Vec::new(), 0o600),
                )
            })
            .await
            .expect("both requests should be written before either response");
            completed.0.unwrap();
            completed.1.unwrap();
        });
    }

    #[test]
    fn cancelled_partial_write_cannot_corrupt_the_next_request() {
        let _test_guard = TRACE_TEST_LOCK.lock().unwrap();
        let cancel = r#"{"kind":"cancel","payload":{"id":1,"error":null}}"#;
        let second = r#"{"kind":"write_file","payload":{"id":2,"error":null}}"#;
        let script = format!(
            "sleep 0.05\nIFS= read -r first\nIFS= read -r cancel\nprintf '%s\\n' '{cancel}'\nIFS= read -r second\nprintf '%s\\n' '{second}'"
        );
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg(script);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let session = VmToolSession::spawn(&mut command).unwrap();
            let cancelled = tokio::time::timeout(
                Duration::from_millis(10),
                session.write_file("/large", vec![b'x'; 256 * 1024], 0o600),
            )
            .await;
            assert!(cancelled.is_err());

            session
                .write_file("/second", Vec::new(), 0o600)
                .await
                .unwrap();
        });
    }

    #[test]
    fn tool_capabilities_own_the_vmm_and_egress_lifetime() {
        let _test_guard = TRACE_TEST_LOCK.lock().unwrap();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg("sleep 30");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let session = VmToolSession::spawn(&mut command).unwrap();
            let guard = Arc::new(());
            let weak_guard = Arc::downgrade(&guard);
            let mut egress = crate::egress::EgressLease::disabled();
            egress.retain(guard);
            session.provision_egress(egress).await.unwrap();
            let tools = session.tools();

            drop(session);
            assert!(
                weak_guard.upgrade().is_some(),
                "dropping the launch owner must not revoke an active tool tree"
            );

            drop(tools);
            tokio::time::timeout(Duration::from_secs(1), async {
                while weak_guard.upgrade().is_some() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("egress must be released after the killed VMM is reaped");
        });
    }

    #[test]
    fn graceful_shutdown_rejects_live_sibling_capabilities() {
        let _test_guard = TRACE_TEST_LOCK.lock().unwrap();
        let response = r#"{"kind":"shutdown","payload":{"id":0,"error":null}}"#;
        let script = format!("IFS= read -r request\nprintf '%s\\n' '{response}'");
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg(script);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let session = VmToolSession::spawn(&mut command).unwrap();
            let handle = session.handle();
            assert!(matches!(
                session.shutdown().await,
                Err(VmToolSessionError::ActiveCapabilities(1))
            ));
            drop(handle);
            session.shutdown().await.unwrap();
        });
    }

    #[test]
    fn graceful_shutdown_rejects_owner_requests_already_in_flight() {
        let _test_guard = TRACE_TEST_LOCK.lock().unwrap();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg("IFS= read -r request\nsleep 30");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let session = VmToolSession::spawn(&mut command).unwrap();
            {
                let request = session.write_file("/pending", Vec::new(), 0o600);
                tokio::pin!(request);
                tokio::select! {
                    result = &mut request => panic!("request unexpectedly completed: {result:?}"),
                    () = tokio::time::sleep(Duration::from_millis(10)) => {}
                }
                assert!(matches!(
                    session.shutdown().await,
                    Err(VmToolSessionError::ActiveRequests(1))
                ));
            }
            session.terminate().await;
        });
    }

    #[test]
    fn cancelling_graceful_shutdown_forcibly_terminates_the_vmm() {
        let _test_guard = TRACE_TEST_LOCK.lock().unwrap();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg("IFS= read -r request\nsleep 30");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let session =
                VmToolSession::spawn_with_shutdown_timeout(&mut command, Duration::from_millis(50))
                    .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(10), session.shutdown())
                    .await
                    .is_err()
            );
            assert!(session.handle.inner.closing.load(Ordering::Acquire));
            assert!(lock_unpoisoned(&session.handle.inner.child).is_none());
        });
    }

    #[test]
    fn startup_and_shutdown_deadlines_are_enforced() {
        let _test_guard = TRACE_TEST_LOCK.lock().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let mut startup_command = tokio::process::Command::new("/bin/sh");
            startup_command.arg("-c").arg("sleep 30");
            let started_at = Instant::now();
            let startup = VmToolSession::spawn_configured_with_timeouts(
                startup_command,
                crate::config::VmConfig::ext4("/unused/root.ext4"),
                crate::command::GuestCommand::new("/bin/true"),
                crate::egress::EgressLease::disabled(),
                Duration::from_millis(10),
                Duration::from_millis(50),
            )
            .await;
            assert!(matches!(
                startup,
                Err(VmToolSessionError::StartupTimeout(timeout))
                    if timeout == Duration::from_millis(10)
            ));
            assert!(started_at.elapsed() < Duration::from_secs(1));

            let mut shutdown_command = tokio::process::Command::new("/bin/sh");
            shutdown_command
                .arg("-c")
                .arg("IFS= read -r request\nsleep 30");
            let session = VmToolSession::spawn_with_shutdown_timeout(
                &mut shutdown_command,
                Duration::from_millis(10),
            )
            .unwrap();
            let started_at = Instant::now();
            assert!(matches!(
                session.shutdown().await,
                Err(VmToolSessionError::ShutdownTimeout(timeout))
                    if timeout == Duration::from_millis(10)
            ));
            assert!(started_at.elapsed() < Duration::from_secs(1));
        });
    }

    #[test]
    fn cancellation_storm_releases_bounded_admission_slots() {
        let _test_guard = TRACE_TEST_LOCK.lock().unwrap();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg("sleep 30");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let session = VmToolSession::spawn(&mut command).unwrap();
            for index in 0..(super::MAX_HOST_IN_FLIGHT_REQUESTS * 2) {
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(1),
                        session.write_file(format!("/cancelled-{index}"), Vec::new(), 0o600),
                    )
                    .await
                    .is_err()
                );
            }
            let permits = tokio::time::timeout(
                Duration::from_secs(1),
                session
                    .handle
                    .inner
                    .request_slots
                    .acquire_many(super::MAX_HOST_IN_FLIGHT_REQUESTS as u32),
            )
            .await
            .expect("cancelled requests must release all admission slots")
            .unwrap();
            drop(permits);
            session.terminate().await;
        });
    }

    #[test]
    fn configured_spawn_retains_private_input_until_the_vmm_has_loaded_it() {
        let _test_guard = TRACE_TEST_LOCK.lock().unwrap();
        let ready = r#"{"kind":"ready","payload":{"id":0,"error":null}}"#;
        let shutdown = r#"{"kind":"shutdown","payload":{"id":1,"error":null}}"#;
        let script = format!(
            "config=$1\nsleep 0.05\ntest -f \"$config\" || exit 9\n\
             IFS= read -r request\nprintf '%s\\n' '{ready}'\n\
             IFS= read -r request\nprintf '%s\\n' '{shutdown}'"
        );
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg(script).arg("vm-test");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let session = VmToolSession::spawn_configured(
                command,
                crate::config::VmConfig::ext4("/unused/root.ext4"),
                crate::command::GuestCommand::new("/bin/true"),
                crate::egress::EgressLease::disabled(),
            )
            .await
            .unwrap();
            session.shutdown().await.unwrap();
        });
    }
}
