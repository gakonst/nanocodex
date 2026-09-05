use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    future::Future,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use nanocodex_tools::{ToolContext, workspace_runtime::WorkspaceToolRuntime};
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use thiserror::Error;
use tokio::{
    fs::{File, OpenOptions},
    io::{
        AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
        BufReader,
    },
    process::{Child, Command},
    sync::mpsc,
    task::JoinSet,
};

use super::protocol::{
    CancelRequest, ControlResponse, CreateDirectoryRequest, ExecuteRequest, ExecuteResponse,
    MemoryResponse, ReadFileRequest, ReadFileResponse, SessionRequest, SessionResponse,
    ShutdownRequest, ToolResponse, WriteFileRequest,
};
#[cfg(all(feature = "guest-runtime", target_os = "linux"))]
use crate::overlay::{GuestOverlayError, enter_guest_overlay_root};

const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_CONTROL_FILE_BYTES: usize = 32 * 1024 * 1024;
#[cfg(feature = "guest-runtime")]
const FILESYSTEM_SYNC_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_IN_FLIGHT_REQUESTS: usize = 64;
const MEMORY_SAMPLE_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Default)]
struct GuestMemoryMonitor {
    total_kib: AtomicU64,
    minimum_available_kib: AtomicU64,
    initial_oom_kills: AtomicU64,
    current_oom_kills: AtomicU64,
    oom_baseline_ready: AtomicBool,
}

impl GuestMemoryMonitor {
    async fn sample(&self) {
        let (meminfo, vmstat) = tokio::join!(
            tokio::fs::read_to_string("/proc/meminfo"),
            tokio::fs::read_to_string("/proc/vmstat"),
        );
        if let Ok(meminfo) = meminfo
            && let Some((total_kib, available_kib)) = parse_meminfo(&meminfo)
        {
            self.total_kib.store(total_kib, Ordering::Relaxed);
            let _ = self
                .minimum_available_kib
                .fetch_min(available_kib, Ordering::Relaxed);
        }
        if let Ok(vmstat) = vmstat
            && let Some(oom_kills) = parse_vmstat_oom_kills(&vmstat)
        {
            if self
                .oom_baseline_ready
                .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                self.initial_oom_kills.store(oom_kills, Ordering::Relaxed);
            }
            self.current_oom_kills.store(oom_kills, Ordering::Relaxed);
        }
    }

    async fn run(self: Arc<Self>) {
        loop {
            self.sample().await;
            tokio::time::sleep(MEMORY_SAMPLE_INTERVAL).await;
        }
    }

    fn response(&self, id: u64) -> MemoryResponse {
        let total_kib = self.total_kib.load(Ordering::Relaxed);
        let minimum_available_kib = self.minimum_available_kib.load(Ordering::Relaxed);
        let available = total_kib > 0 && minimum_available_kib != u64::MAX;
        MemoryResponse {
            id,
            total_kib: available.then_some(total_kib),
            minimum_available_kib: available.then_some(minimum_available_kib),
            oom_kills: self
                .current_oom_kills
                .load(Ordering::Relaxed)
                .saturating_sub(self.initial_oom_kills.load(Ordering::Relaxed)),
            error: (!available).then(|| "guest /proc memory telemetry is unavailable".to_owned()),
        }
    }
}

fn parse_meminfo(contents: &str) -> Option<(u64, u64)> {
    let mut total = None;
    let mut available = None;
    for line in contents.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let Some(value) = value
            .split_whitespace()
            .next()
            .and_then(|value| value.parse::<u64>().ok())
        else {
            continue;
        };
        match key {
            "MemTotal" => total = Some(value),
            "MemAvailable" => available = Some(value),
            _ => {}
        }
        if total.is_some() && available.is_some() {
            break;
        }
    }
    Some((total?, available?))
}

fn parse_vmstat_oom_kills(contents: &str) -> Option<u64> {
    contents.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let key = fields.next()?;
        let value = fields.next()?;
        (key == "oom_kill").then(|| value.parse().ok()).flatten()
    })
}

/// Failure while serving VM tool requests inside the guest.
#[derive(Debug, Error)]
pub enum VmGuestError {
    /// The immutable lower and writable upper disks could not become the
    /// guest's effective OverlayFS root.
    #[cfg(all(feature = "guest-runtime", target_os = "linux"))]
    #[error(transparent)]
    Overlay(#[from] GuestOverlayError),

    /// Guest console I/O failed.
    #[error("VM tool console I/O failed: {0}")]
    Io(#[from] std::io::Error),

    /// A protocol frame was not valid JSON.
    #[error("VM tool protocol JSON failed: {0}")]
    Json(#[from] serde_json::Error),

    /// The host closed the console in the middle of a frame.
    #[error("the VM tool console closed before a complete frame")]
    Closed,

    /// A concurrently executed guest request task failed.
    #[error("guest tool execution task failed: {0}")]
    Task(String),

    /// An inbound or outbound protocol frame exceeded the fixed limit.
    #[error("VM tool protocol frame exceeded the {MAX_FRAME_BYTES}-byte limit")]
    FrameTooLarge,

    /// The host reused an identifier while its earlier request was active.
    #[error("VM tool protocol reused active request ID {0}")]
    DuplicateRequestId(u64),
}

#[cfg(feature = "guest-runtime")]
pub(crate) async fn serve(workspace: &Path) -> Result<(), VmGuestError> {
    serve_io(workspace, tokio::io::stdin(), tokio::io::stdout()).await
}

#[cfg(all(feature = "guest-runtime", target_os = "linux"))]
pub(crate) async fn serve_overlay(
    workspace: &Path,
    resolver_configuration: Option<&str>,
) -> Result<(), VmGuestError> {
    enter_guest_overlay_root(resolver_configuration)?;
    tokio::fs::create_dir_all(workspace).await?;
    serve(workspace).await
}

#[cfg(feature = "guest-runtime")]
async fn serve_io(
    workspace: &Path,
    input: impl AsyncRead + Unpin,
    output: impl AsyncWrite + Unpin,
) -> Result<(), VmGuestError> {
    serve_io_with_frame_limit(workspace, input, output, MAX_FRAME_BYTES).await
}

#[cfg(feature = "guest-runtime")]
async fn serve_io_with_frame_limit(
    workspace: &Path,
    input: impl AsyncRead + Unpin,
    output: impl AsyncWrite + Unpin,
    max_frame_bytes: usize,
) -> Result<(), VmGuestError> {
    serve_io_with_frame_limit_and_sync(workspace, input, output, max_frame_bytes, sync_filesystems)
        .await
}

async fn serve_io_with_frame_limit_and_sync<Sync, SyncFuture>(
    workspace: &Path,
    input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
    max_frame_bytes: usize,
    sync: Sync,
) -> Result<(), VmGuestError>
where
    Sync: FnOnce(ShutdownRequest) -> SyncFuture,
    SyncFuture: Future<Output = ControlResponse>,
{
    let runtime = Arc::new(
        WorkspaceToolRuntime::with_environment_and_view_image_wire_limit(
            workspace.to_path_buf(),
            u64::try_from(max_frame_bytes).unwrap_or(u64::MAX),
            std::env::vars_os().collect(),
        ),
    );
    let memory = Arc::new(GuestMemoryMonitor {
        minimum_available_kib: AtomicU64::new(u64::MAX),
        ..GuestMemoryMonitor::default()
    });
    memory.sample().await;
    let memory_task = tokio::spawn(Arc::clone(&memory).run());
    let mut input = BufReader::new(input);
    let mut requests = JoinSet::<SessionResponse>::new();
    let mut active = HashMap::<u64, tokio::task::AbortHandle>::new();
    let mut accepting = true;
    let mut shutdown = None;

    let result = async {
        while accepting || !requests.is_empty() {
            tokio::select! {
                joined = requests.join_next(), if !requests.is_empty() => {
                    match joined.ok_or(VmGuestError::Closed)? {
                        Ok(response) => {
                            active.remove(&response.id());
                            write_response(&mut output, &response, max_frame_bytes).await?;
                        }
                        Err(error) if error.is_cancelled() => {}
                        Err(error) => return Err(VmGuestError::Task(error.to_string())),
                    }
                }
                frame = read_frame(&mut input),
                    if accepting && requests.len() < MAX_IN_FLIGHT_REQUESTS =>
                {
                    let Some(frame) = frame? else {
                        accepting = false;
                        requests.abort_all();
                        continue;
                    };
                    match serde_json::from_slice::<SessionRequest>(&frame)? {
                        SessionRequest::Shutdown(request) => {
                            shutdown = Some(request);
                            accepting = false;
                            runtime.control().cancel().await;
                            active.clear();
                            requests.abort_all();
                        }
                        SessionRequest::Cancel(request) => {
                            if let Some(task) = active.remove(&request.target_id) {
                                task.abort();
                            }
                            let response = SessionResponse::Cancel(ControlResponse {
                                id: request.id,
                                error: None,
                            });
                            write_response(&mut output, &response, max_frame_bytes).await?;
                        }
                        SessionRequest::TerminateToolProcesses(request) => {
                            runtime.control().cancel().await;
                            let response =
                                SessionResponse::TerminateToolProcesses(ControlResponse {
                                    id: request.id,
                                    error: None,
                                });
                            write_response(&mut output, &response, max_frame_bytes).await?;
                        }
                        request => {
                            let id = request.id();
                            if active.contains_key(&id) {
                                return Err(VmGuestError::DuplicateRequestId(id));
                            }
                            let runtime = Arc::clone(&runtime);
                            let memory = Arc::clone(&memory);
                            let task =
                                requests.spawn(async move {
                                    execute_request(runtime, memory, request).await
                                });
                            active.insert(id, task);
                        }
                    }
                }
            }
        }
        Ok::<_, VmGuestError>(shutdown)
    }
    .await;

    runtime.control().cancel().await;
    memory_task.abort();
    if let Some(request) = result? {
        let response = SessionResponse::Shutdown(sync(request).await);
        write_response(&mut output, &response, max_frame_bytes).await?;
    }
    Ok(())
}

#[cfg(test)]
async fn serve_test_io(
    workspace: &Path,
    input: impl AsyncRead + Unpin,
    output: impl AsyncWrite + Unpin,
) -> Result<(), VmGuestError> {
    serve_test_io_with_frame_limit(workspace, input, output, MAX_FRAME_BYTES).await
}

#[cfg(test)]
async fn serve_test_io_with_frame_limit(
    workspace: &Path,
    input: impl AsyncRead + Unpin,
    output: impl AsyncWrite + Unpin,
    max_frame_bytes: usize,
) -> Result<(), VmGuestError> {
    serve_io_with_frame_limit_and_sync(
        workspace,
        input,
        output,
        max_frame_bytes,
        |request| async move {
            ControlResponse {
                id: request.id,
                error: None,
            }
        },
    )
    .await
}

async fn execute_request(
    runtime: Arc<WorkspaceToolRuntime>,
    memory: Arc<GuestMemoryMonitor>,
    request: SessionRequest,
) -> SessionResponse {
    match request {
        SessionRequest::Ready(request) => SessionResponse::Ready(ControlResponse {
            id: request.id,
            error: None,
        }),
        SessionRequest::Tool(request) => {
            let context = ToolContext::new(
                &request.context.model,
                &request.context.session_id,
                &request.context.call_id,
                &[],
                request.context.output_token_budget,
            );
            let execution = runtime
                .execute_tool(request.tool.name(), request.input.into(), context)
                .await;
            SessionResponse::Tool(match execution.into_wire() {
                Ok(execution) => ToolResponse::completed(request.id, execution),
                Err(error) => ToolResponse::failed(request.id, error.to_string()),
            })
        }
        SessionRequest::WriteFile(request) => SessionResponse::WriteFile(write_file(request).await),
        SessionRequest::CreateDirectory(request) => {
            SessionResponse::CreateDirectory(create_directory(request).await)
        }
        SessionRequest::ReadFile(request) => SessionResponse::ReadFile(read_file(request).await),
        SessionRequest::Memory(request) => SessionResponse::Memory(memory.response(request.id)),
        SessionRequest::Execute(request) => {
            SessionResponse::Execute(execute_command(request).await)
        }
        SessionRequest::Cancel(CancelRequest { id, .. }) => {
            SessionResponse::Cancel(ControlResponse {
                id,
                error: Some("cancel cannot be dispatched as a concurrent request".to_owned()),
            })
        }
        SessionRequest::TerminateToolProcesses(request) => {
            SessionResponse::TerminateToolProcesses(ControlResponse {
                id: request.id,
                error: Some(
                    "tool-process termination cannot be dispatched as a concurrent request"
                        .to_owned(),
                ),
            })
        }
        SessionRequest::Shutdown(request) => SessionResponse::Shutdown(ControlResponse {
            id: request.id,
            error: Some("shutdown cannot be dispatched as a concurrent request".to_owned()),
        }),
    }
}

async fn write_response(
    output: &mut (impl AsyncWrite + Unpin),
    response: &SessionResponse,
    max_frame_bytes: usize,
) -> Result<(), VmGuestError> {
    let mut encoded = match encode_frame(response, max_frame_bytes)? {
        EncodedFrame::Complete(encoded) => encoded,
        EncodedFrame::TooLarge => {
            let SessionResponse::Tool(response) = response else {
                return Err(VmGuestError::FrameTooLarge);
            };
            let fallback = SessionResponse::Tool(ToolResponse::failed(
                response.id,
                format!(
                    "VM tool response exceeded the {max_frame_bytes}-byte protocol frame limit"
                ),
            ));
            match encode_frame(&fallback, max_frame_bytes)? {
                EncodedFrame::Complete(encoded) => encoded,
                EncodedFrame::TooLarge => return Err(VmGuestError::FrameTooLarge),
            }
        }
    };
    encoded.push(b'\n');
    output.write_all(&encoded).await?;
    output.flush().await?;
    Ok(())
}

enum EncodedFrame {
    Complete(Vec<u8>),
    TooLarge,
}

fn encode_frame(
    response: &SessionResponse,
    max_frame_bytes: usize,
) -> Result<EncodedFrame, serde_json::Error> {
    let mut output = BoundedFrameWriter::new(max_frame_bytes);
    match serde_json::to_writer(&mut output, response) {
        Ok(()) => Ok(EncodedFrame::Complete(output.into_inner())),
        Err(_) if output.limit_exceeded => Ok(EncodedFrame::TooLarge),
        Err(error) => Err(error),
    }
}

struct BoundedFrameWriter {
    bytes: Vec<u8>,
    max_bytes: usize,
    limit_exceeded: bool,
}

impl BoundedFrameWriter {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(max_bytes.min(8 * 1024)),
            max_bytes,
            limit_exceeded: false,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl std::io::Write for BoundedFrameWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if buffer.len() > self.max_bytes.saturating_sub(self.bytes.len()) {
            self.limit_exceeded = true;
            return Err(std::io::Error::other(
                "VM tool protocol frame limit exceeded",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn read_frame(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> Result<Option<Vec<u8>>, VmGuestError> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(VmGuestError::Closed)
            };
        }
        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if frame.len().saturating_add(newline) > MAX_FRAME_BYTES {
                return Err(VmGuestError::FrameTooLarge);
            }
            frame.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            return Ok(Some(frame));
        }
        if frame.len().saturating_add(available.len()) > MAX_FRAME_BYTES {
            return Err(VmGuestError::FrameTooLarge);
        }
        let consumed = available.len();
        frame.extend_from_slice(available);
        reader.consume(consumed);
    }
}

#[cfg(feature = "guest-runtime")]
async fn sync_filesystems(request: ShutdownRequest) -> ControlResponse {
    let mut command = Command::new("/bin/sync");
    command.kill_on_drop(true);
    let error = match tokio::time::timeout(FILESYSTEM_SYNC_TIMEOUT, command.status()).await {
        Ok(Ok(status)) if status.success() => None,
        Ok(Ok(status)) => Some(format!("sync exited with {status}")),
        Ok(Err(error)) => Some(error.to_string()),
        Err(_) => Some(format!(
            "sync exceeded the {FILESYSTEM_SYNC_TIMEOUT:?} shutdown deadline"
        )),
    };
    ControlResponse {
        id: request.id,
        error,
    }
}

async fn write_file(request: WriteFileRequest) -> ControlResponse {
    let result = atomic_write_file(
        &request.path,
        &request.contents,
        request.mode,
        request.modified_unix_seconds,
        request.id,
    )
    .await;
    ControlResponse {
        id: request.id,
        error: result.err().map(|error| error.to_string()),
    }
}

async fn atomic_write_file(
    path: &str,
    contents: &[u8],
    mode: u32,
    modified_unix_seconds: Option<i64>,
    request_id: u64,
) -> std::io::Result<()> {
    let path = PathBuf::from(path);
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("file path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("file path has no name"))?
        .to_string_lossy();
    tokio::fs::create_dir_all(parent).await?;
    let temporary = parent.join(format!(".{name}.nanocodex-{request_id}.tmp"));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .await?;
        file.write_all(contents).await?;
        file.flush().await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(mode))
                .await?;
        }
        drop(file);
        tokio::fs::rename(&temporary, &path).await?;
        set_modified_time(&path, modified_unix_seconds)
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

async fn create_directory(request: CreateDirectoryRequest) -> ControlResponse {
    let result =
        create_directory_path(&request.path, request.mode, request.modified_unix_seconds).await;
    ControlResponse {
        id: request.id,
        error: result.err().map(|error| error.to_string()),
    }
}

async fn create_directory_path(
    path: &str,
    mode: u32,
    modified_unix_seconds: Option<i64>,
) -> std::io::Result<()> {
    let path = PathBuf::from(path);
    tokio::fs::create_dir_all(&path).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).await?;
    }
    set_modified_time(&path, modified_unix_seconds)
}

fn set_modified_time(path: &Path, modified_unix_seconds: Option<i64>) -> std::io::Result<()> {
    let Some(modified_unix_seconds) = modified_unix_seconds else {
        return Ok(());
    };
    filetime::set_file_mtime(
        path,
        filetime::FileTime::from_unix_time(modified_unix_seconds, 0),
    )
}

async fn read_file(request: ReadFileRequest) -> ReadFileResponse {
    let contents = async {
        let file = tokio::fs::File::open(&request.path).await?;
        let metadata = file.metadata().await?;
        if !metadata.is_file() {
            return Err(std::io::Error::other(
                "control reads require a regular file",
            ));
        }
        let maximum = u64::try_from(MAX_CONTROL_FILE_BYTES).unwrap_or(u64::MAX);
        if metadata.len() > maximum {
            return Err(std::io::Error::other(format!(
                "file is {} bytes, exceeding the {MAX_CONTROL_FILE_BYTES}-byte control limit",
                metadata.len()
            )));
        }
        let mut contents = Vec::with_capacity(
            usize::try_from(metadata.len())
                .unwrap_or(usize::MAX)
                .min(MAX_CONTROL_FILE_BYTES),
        );
        file.take(maximum.saturating_add(1))
            .read_to_end(&mut contents)
            .await?;
        if contents.len() > MAX_CONTROL_FILE_BYTES {
            return Err(std::io::Error::other(format!(
                "file grew beyond the {MAX_CONTROL_FILE_BYTES}-byte control limit while reading"
            )));
        }
        Ok(contents)
    }
    .await;
    match contents {
        Ok(contents) => ReadFileResponse {
            id: request.id,
            contents: Some(contents),
            error: None,
        },
        Err(error) => ReadFileResponse {
            id: request.id,
            contents: None,
            error: Some(error.to_string()),
        },
    }
}

async fn execute_command(request: ExecuteRequest) -> ExecuteResponse {
    let stdout_mirror = match open_output_mirror(request.stdout_mirror.as_deref()).await {
        Ok(mirror) => mirror,
        Err(error) => return failed_execute_response(request.id, error),
    };
    let stderr_mirror = match open_output_mirror(request.stderr_mirror.as_deref()).await {
        Ok(mirror) => mirror,
        Err(error) => return failed_execute_response(request.id, error),
    };
    let environment = command_environment(std::env::vars_os(), &request.environment);
    let mut command = Command::new(&request.program);
    command
        .args(&request.arguments)
        .current_dir(&request.current_directory)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.process_group(0);

    let timeout = Duration::from_millis(request.timeout_millis);
    match command_output(
        &mut command,
        timeout,
        request.max_output_bytes,
        stdout_mirror,
        stderr_mirror,
    )
    .await
    {
        Ok(CommandOutcome::Completed(output)) => ExecuteResponse {
            id: request.id,
            exit_code: Some(output.status.code().unwrap_or(1)),
            stdout: Some(output.stdout),
            stderr: Some(output.stderr),
            error: None,
            timed_out: false,
            output_limit_exceeded: false,
        },
        Ok(CommandOutcome::TimedOut { stdout, stderr }) => ExecuteResponse {
            id: request.id,
            exit_code: None,
            stdout: Some(stdout),
            stderr: Some(stderr),
            error: None,
            timed_out: true,
            output_limit_exceeded: false,
        },
        Ok(CommandOutcome::OutputLimitExceeded) => ExecuteResponse {
            id: request.id,
            exit_code: None,
            stdout: None,
            stderr: None,
            error: None,
            timed_out: false,
            output_limit_exceeded: true,
        },
        Err(error) => failed_execute_response(request.id, error),
    }
}

async fn open_output_mirror(path: Option<&str>) -> std::io::Result<Option<File>> {
    match path {
        Some(path) => OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .await
            .map(Some),
        None => Ok(None),
    }
}

fn failed_execute_response(id: u64, error: std::io::Error) -> ExecuteResponse {
    ExecuteResponse {
        id,
        exit_code: None,
        stdout: None,
        stderr: None,
        error: Some(error.to_string()),
        timed_out: false,
        output_limit_exceeded: false,
    }
}

fn command_environment(
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
    overrides: &[(String, String)],
) -> BTreeMap<OsString, OsString> {
    let mut environment = inherited.into_iter().collect::<BTreeMap<_, _>>();
    environment.extend(overrides.iter().map(|(name, value)| {
        (
            OsString::from(name.as_str()),
            OsString::from(value.as_str()),
        )
    }));
    environment
}

enum CommandOutcome {
    Completed(std::process::Output),
    TimedOut { stdout: Vec<u8>, stderr: Vec<u8> },
    OutputLimitExceeded,
}

enum WaitOutcome {
    Exited(ExitStatus),
    TimedOut,
    OutputLimitExceeded,
}

async fn command_output(
    command: &mut Command,
    timeout: Duration,
    max_output_bytes: usize,
    stdout_mirror: Option<File>,
    stderr_mirror: Option<File>,
) -> std::io::Result<CommandOutcome> {
    let mut child = command.spawn()?;
    let process_group = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .map(Pid::from_raw);
    let mut process_group_guard = ProcessGroupGuard(process_group);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("guest command stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("guest command stderr was not piped"))?;
    let retained = Arc::new(AtomicUsize::new(0));
    let (limit_sender, mut limit_receiver) = mpsc::channel(1);
    let mut stdout = tokio::spawn(read_bounded(
        stdout,
        Arc::clone(&retained),
        max_output_bytes,
        limit_sender.clone(),
        stdout_mirror,
    ));
    let mut stderr = tokio::spawn(read_bounded(
        stderr,
        Arc::clone(&retained),
        max_output_bytes,
        limit_sender,
        stderr_mirror,
    ));
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    let mut outcome = tokio::select! {
        status = child.wait() => WaitOutcome::Exited(status?),
        () = &mut deadline => WaitOutcome::TimedOut,
        Some(()) = limit_receiver.recv() => WaitOutcome::OutputLimitExceeded,
    };
    if !matches!(outcome, WaitOutcome::Exited(_)) {
        if let Some(status) = child.try_wait()? {
            outcome = WaitOutcome::Exited(status);
        } else {
            kill_process_group(&mut child, process_group)?;
            child.wait().await?;
        }
    }
    let (stdout, stderr) = tokio::time::timeout(Duration::from_secs(1), async {
        let stdout = (&mut stdout).await.map_err(std::io::Error::other)??;
        let stderr = (&mut stderr).await.map_err(std::io::Error::other)??;
        Ok::<_, std::io::Error>((stdout, stderr))
    })
    .await
    .unwrap_or_else(|_| {
        let _ = kill_process_group(&mut child, process_group);
        Err(std::io::Error::other(
            "guest command descendants kept output pipes open",
        ))
    })?;
    process_group_guard.disarm();
    let output_limit_exceeded = retained.load(Ordering::Relaxed) > max_output_bytes;
    match outcome {
        WaitOutcome::Exited(_) if output_limit_exceeded => Ok(CommandOutcome::OutputLimitExceeded),
        WaitOutcome::Exited(status) => Ok(CommandOutcome::Completed(std::process::Output {
            status,
            stdout,
            stderr,
        })),
        WaitOutcome::TimedOut => Ok(CommandOutcome::TimedOut { stdout, stderr }),
        WaitOutcome::OutputLimitExceeded => Ok(CommandOutcome::OutputLimitExceeded),
    }
}

fn kill_process_group(child: &mut Child, process_group: Option<Pid>) -> std::io::Result<()> {
    if let Some(process_group) = process_group {
        let child_kill = child.start_kill();
        match killpg(process_group, Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(Errno::EPERM) => child_kill.or(Ok(())),
            Err(error) => Err(std::io::Error::other(error)),
        }
    } else {
        child.start_kill()
    }
}

struct ProcessGroupGuard(Option<Pid>);

impl ProcessGroupGuard {
    const fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if let Some(process_group) = self.0 {
            let _ = killpg(process_group, Signal::SIGKILL);
        }
    }
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    retained: Arc<AtomicUsize>,
    limit: usize,
    limit_sender: mpsc::Sender<()>,
    mut mirror: Option<File>,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    let mut reported = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            if let Some(mirror) = &mut mirror {
                mirror.flush().await?;
            }
            break;
        }
        if let Some(mirror) = &mut mirror {
            mirror.write_all(&buffer[..read]).await?;
            mirror.flush().await?;
        }
        let offset = retained.fetch_add(read, Ordering::Relaxed);
        let allowed = limit.saturating_sub(offset).min(read);
        output.extend_from_slice(&buffer[..allowed]);
        if allowed < read && !reported {
            reported = true;
            let _ = limit_sender.try_send(());
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        ffi::OsString,
        fs::{self, File},
        process, thread,
        time::{Duration, Instant, UNIX_EPOCH},
    };

    use nanocodex_tools::{ToolInput, contract::ToolOutputBody, standard::StandardTool};
    use nix::sys::signal::Signal;
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    use serde_json::{json, value::to_raw_value};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::super::protocol::{
        CancelRequest, ExecuteRequest, ReadFileRequest, ReadyRequest, SessionRequest,
        SessionResponse, ShutdownRequest, TerminateToolProcessesRequest, ToolRequest,
        WireToolContext, WireToolInput,
    };
    use super::{
        atomic_write_file, command_environment, create_directory_path, execute_command,
        parse_meminfo, parse_vmstat_oom_kills, read_file, serve_test_io,
        serve_test_io_with_frame_limit,
    };

    const DEFAULT_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
    const DETACHED_PROCESS_PID_FILE_ENV: &str = "NANOCODEX_VM_TEST_DETACHED_PID_FILE";
    const PATH_TRACING_IMAGE_BYTES: u64 = 48_262_737;

    #[test]
    fn parses_guest_peak_memory_inputs() {
        let meminfo = "MemTotal:       524288 kB\nmalformed\nMemAvailable:   131071 kB\n";
        assert_eq!(parse_meminfo(meminfo), Some((524_288, 131_071)));
        assert_eq!(
            parse_vmstat_oom_kills("pgfault 12\noom_kill 3\npgmajfault 1\n"),
            Some(3)
        );
    }

    #[test]
    fn trusted_commands_inherit_guest_environment_and_apply_explicit_overrides() {
        let environment = command_environment(
            [
                (OsString::from("HTTPS_PROXY"), OsString::from("from-guest")),
                (OsString::from("PATH"), OsString::from("/guest/bin")),
            ],
            &[
                ("PATH".to_owned(), "/image/bin".to_owned()),
                ("IMAGE_MODE".to_owned(), "release".to_owned()),
            ],
        );

        assert_eq!(
            environment.get(&OsString::from("HTTPS_PROXY")),
            Some(&OsString::from("from-guest"))
        );
        assert_eq!(
            environment.get(&OsString::from("PATH")),
            Some(&OsString::from("/image/bin"))
        );
        assert_eq!(
            environment.get(&OsString::from("IMAGE_MODE")),
            Some(&OsString::from("release"))
        );
    }

    #[tokio::test]
    async fn filesystem_controls_apply_exact_modes_and_epoch_mtimes() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("tests");
        let file = directory.join("test.sh");
        let directory = directory.to_string_lossy();
        let file = file.to_string_lossy();

        create_directory_path(&directory, 0o700, None)
            .await
            .unwrap();
        atomic_write_file(&file, b"#!/bin/sh\n", 0o640, Some(0), 7)
            .await
            .unwrap();
        create_directory_path(&directory, 0o750, Some(0))
            .await
            .unwrap();

        let file_metadata = fs::metadata(file.as_ref()).unwrap();
        assert_eq!(file_metadata.permissions().mode() & 0o7777, 0o640);
        assert_eq!(file_metadata.modified().unwrap(), UNIX_EPOCH);
        let directory_metadata = fs::metadata(directory.as_ref()).unwrap();
        assert_eq!(directory_metadata.permissions().mode() & 0o7777, 0o750);
        assert_eq!(directory_metadata.modified().unwrap(), UNIX_EPOCH);
    }

    #[tokio::test]
    async fn timeout_kills_descendants_holding_output_pipes() {
        let started_at = Instant::now();
        let response = execute_command(ExecuteRequest {
            id: 1,
            program: "/bin/sh".to_owned(),
            arguments: vec![
                "-c".to_owned(),
                "printf 'partial stdout'; printf 'partial stderr' >&2; sleep 30 & wait".to_owned(),
            ],
            current_directory: "/".to_owned(),
            environment: Vec::new(),
            // Leave enough time for a process scheduled on a loaded CI host to
            // publish its partial output before exercising the timeout path.
            timeout_millis: 1_000,
            max_output_bytes: DEFAULT_OUTPUT_BYTES,
            stdout_mirror: None,
            stderr_mirror: None,
        })
        .await;

        assert!(response.timed_out);
        assert!(response.error.is_none());
        assert_eq!(
            response.stdout.as_deref(),
            Some(b"partial stdout".as_slice())
        );
        assert_eq!(
            response.stderr.as_deref(),
            Some(b"partial stderr".as_slice())
        );
        assert!(started_at.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn command_output_is_bounded_while_it_is_produced() {
        let response = execute_command(ExecuteRequest {
            id: 1,
            program: "/usr/bin/yes".to_owned(),
            arguments: vec!["bounded".to_owned()],
            current_directory: "/".to_owned(),
            environment: Vec::new(),
            timeout_millis: 5_000,
            max_output_bytes: 8,
            stdout_mirror: None,
            stderr_mirror: None,
        })
        .await;

        assert!(response.output_limit_exceeded);
        assert!(!response.timed_out);
        assert!(response.error.is_none());
        assert!(response.stdout.is_none());
        assert!(response.stderr.is_none());
    }

    #[tokio::test]
    async fn command_output_is_mirrored_before_the_process_exits() {
        let directory = tempfile::tempdir().unwrap();
        let stdout = directory.path().join("stdout");
        let stderr = directory.path().join("stderr");
        let execution = tokio::spawn(execute_command(ExecuteRequest {
            id: 1,
            program: "/bin/sh".to_owned(),
            arguments: vec![
                "-c".to_owned(),
                "printf first; printf diagnostic >&2; sleep 0.5; printf second".to_owned(),
            ],
            current_directory: "/".to_owned(),
            environment: Vec::new(),
            timeout_millis: 5_000,
            max_output_bytes: DEFAULT_OUTPUT_BYTES,
            stdout_mirror: Some(stdout.to_string_lossy().into_owned()),
            stderr_mirror: Some(stderr.to_string_lossy().into_owned()),
        }));

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if fs::read(&stdout).is_ok_and(|contents| contents == b"first")
                    && fs::read(&stderr).is_ok_and(|contents| contents == b"diagnostic")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("mirrors should expose initial output while the process runs");
        assert!(!execution.is_finished());

        let response = execution.await.unwrap();
        assert_eq!(response.stdout.as_deref(), Some(b"firstsecond".as_slice()));
        assert_eq!(response.stderr.as_deref(), Some(b"diagnostic".as_slice()));
        assert_eq!(fs::read(stdout).unwrap(), b"firstsecond");
        assert_eq!(fs::read(stderr).unwrap(), b"diagnostic");
    }

    #[tokio::test]
    async fn control_read_rejects_non_regular_files_without_blocking() {
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            read_file(ReadFileRequest {
                id: 1,
                path: "/dev/zero".to_owned(),
            }),
        )
        .await
        .expect("special-file rejection must not wait for EOF");

        assert!(response.contents.is_none());
        assert!(
            response
                .error
                .is_some_and(|error| error.contains("regular file"))
        );
    }

    #[tokio::test]
    async fn independent_requests_execute_concurrently() {
        let workspace = tempfile::tempdir().unwrap();
        let marker = workspace.path().join("second-started");
        let (host, guest) = tokio::io::duplex(64 * 1024);
        let (host_read, mut host_write) = tokio::io::split(host);
        let (guest_read, guest_write) = tokio::io::split(guest);
        let guest_task = tokio::spawn({
            let workspace = workspace.path().to_owned();
            async move { serve_test_io(&workspace, guest_read, guest_write).await }
        });

        for request in [
            SessionRequest::Execute(ExecuteRequest {
                id: 0,
                program: "/bin/sh".to_owned(),
                arguments: vec![
                    "-c".to_owned(),
                    format!("while [ ! -f '{}' ]; do sleep 0.01; done", marker.display()),
                ],
                current_directory: workspace.path().to_string_lossy().into_owned(),
                environment: Vec::new(),
                timeout_millis: 5_000,
                max_output_bytes: DEFAULT_OUTPUT_BYTES,
                stdout_mirror: None,
                stderr_mirror: None,
            }),
            SessionRequest::Execute(ExecuteRequest {
                id: 1,
                program: "/usr/bin/touch".to_owned(),
                arguments: vec![marker.to_string_lossy().into_owned()],
                current_directory: workspace.path().to_string_lossy().into_owned(),
                environment: Vec::new(),
                timeout_millis: 5_000,
                max_output_bytes: DEFAULT_OUTPUT_BYTES,
                stdout_mirror: None,
                stderr_mirror: None,
            }),
        ] {
            host_write
                .write_all(&serde_json::to_vec(&request).unwrap())
                .await
                .unwrap();
            host_write.write_all(b"\n").await.unwrap();
        }

        let mut responses = BufReader::new(host_read).lines();
        for _ in 0..2 {
            let line = responses.next_line().await.unwrap().unwrap();
            assert!(matches!(
                serde_json::from_str::<SessionResponse>(&line).unwrap(),
                SessionResponse::Execute(response)
                    if response.error.is_none() && !response.timed_out
            ));
        }
        host_write
            .write_all(
                &serde_json::to_vec(&SessionRequest::Shutdown(ShutdownRequest { id: 2 })).unwrap(),
            )
            .await
            .unwrap();
        host_write.write_all(b"\n").await.unwrap();
        drop(host_write);
        let shutdown = responses.next_line().await.unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str::<SessionResponse>(&shutdown).unwrap(),
            SessionResponse::Shutdown(response) if response.id == 2 && response.error.is_none()
        ));
        guest_task.await.unwrap().unwrap();
        assert!(marker.is_file());
    }

    #[tokio::test]
    async fn shutdown_aborts_in_flight_work_before_syncing() {
        let workspace = tempfile::tempdir().unwrap();
        let (host, guest) = tokio::io::duplex(64 * 1024);
        let (host_read, mut host_write) = tokio::io::split(host);
        let (guest_read, guest_write) = tokio::io::split(guest);
        let guest_task = tokio::spawn({
            let workspace = workspace.path().to_owned();
            async move { serve_test_io(&workspace, guest_read, guest_write).await }
        });
        for request in [
            SessionRequest::Execute(ExecuteRequest {
                id: 0,
                program: "/bin/sh".to_owned(),
                arguments: vec!["-c".to_owned(), "sleep 30 & wait".to_owned()],
                current_directory: workspace.path().to_string_lossy().into_owned(),
                environment: Vec::new(),
                timeout_millis: 60_000,
                max_output_bytes: DEFAULT_OUTPUT_BYTES,
                stdout_mirror: None,
                stderr_mirror: None,
            }),
            SessionRequest::Shutdown(ShutdownRequest { id: 1 }),
        ] {
            host_write
                .write_all(&serde_json::to_vec(&request).unwrap())
                .await
                .unwrap();
            host_write.write_all(b"\n").await.unwrap();
        }

        let mut responses = BufReader::new(host_read).lines();
        let line = tokio::time::timeout(Duration::from_secs(2), responses.next_line())
            .await
            .expect("shutdown must not wait for the in-flight command")
            .unwrap()
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<SessionResponse>(&line).unwrap(),
            SessionResponse::Shutdown(response) if response.id == 1 && response.error.is_none()
        ));
        drop(host_write);
        guest_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn targeted_cancel_aborts_only_the_requested_guest_task() {
        let workspace = tempfile::tempdir().unwrap();
        let (host, guest) = tokio::io::duplex(64 * 1024);
        let (host_read, mut host_write) = tokio::io::split(host);
        let (guest_read, guest_write) = tokio::io::split(guest);
        let guest_task = tokio::spawn({
            let workspace = workspace.path().to_owned();
            async move { serve_test_io(&workspace, guest_read, guest_write).await }
        });
        for request in [
            SessionRequest::Execute(ExecuteRequest {
                id: 0,
                program: "/bin/sh".to_owned(),
                arguments: vec!["-c".to_owned(), "sleep 30 & wait".to_owned()],
                current_directory: workspace.path().to_string_lossy().into_owned(),
                environment: Vec::new(),
                timeout_millis: 60_000,
                max_output_bytes: DEFAULT_OUTPUT_BYTES,
                stdout_mirror: None,
                stderr_mirror: None,
            }),
            SessionRequest::Cancel(CancelRequest {
                id: 1,
                target_id: 0,
            }),
            SessionRequest::Execute(ExecuteRequest {
                id: 2,
                program: "/usr/bin/true".to_owned(),
                arguments: Vec::new(),
                current_directory: workspace.path().to_string_lossy().into_owned(),
                environment: Vec::new(),
                timeout_millis: 5_000,
                max_output_bytes: DEFAULT_OUTPUT_BYTES,
                stdout_mirror: None,
                stderr_mirror: None,
            }),
        ] {
            host_write
                .write_all(&serde_json::to_vec(&request).unwrap())
                .await
                .unwrap();
            host_write.write_all(b"\n").await.unwrap();
        }

        let mut responses = BufReader::new(host_read).lines();
        let mut cancelled = false;
        let mut follow_up = false;
        while !cancelled || !follow_up {
            let line = tokio::time::timeout(Duration::from_secs(2), responses.next_line())
                .await
                .expect("cancellation and the independent follow-up must complete")
                .unwrap()
                .unwrap();
            match serde_json::from_str::<SessionResponse>(&line).unwrap() {
                SessionResponse::Cancel(response) if response.id == 1 => cancelled = true,
                SessionResponse::Execute(response) if response.id == 2 => {
                    assert!(response.error.is_none());
                    assert!(!response.timed_out);
                    follow_up = true;
                }
                response => panic!("unexpected response ID {}", response.id()),
            }
        }

        host_write
            .write_all(
                &serde_json::to_vec(&SessionRequest::Shutdown(ShutdownRequest { id: 3 })).unwrap(),
            )
            .await
            .unwrap();
        host_write.write_all(b"\n").await.unwrap();
        drop(host_write);
        let shutdown = responses.next_line().await.unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str::<SessionResponse>(&shutdown).unwrap(),
            SessionResponse::Shutdown(response) if response.id == 3 && response.error.is_none()
        ));
        guest_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn tool_process_termination_kills_foreground_session_and_keeps_guest_ready() {
        let workspace = tempfile::tempdir().unwrap();
        let pid_file = workspace.path().join("foreground.pid");
        let (host, guest) = tokio::io::duplex(64 * 1024);
        let (host_read, mut host_write) = tokio::io::split(host);
        let (guest_read, guest_write) = tokio::io::split(guest);
        let guest_task = tokio::spawn({
            let workspace = workspace.path().to_owned();
            async move { serve_test_io(&workspace, guest_read, guest_write).await }
        });
        let command = format!("printf %s $$ > '{}'; exec sleep 30", pid_file.display());
        let start = SessionRequest::Tool(ToolRequest {
            id: 0,
            tool: StandardTool::ExecCommand,
            input: WireToolInput::from(ToolInput::Function(
                to_raw_value(&json!({
                    "cmd": command,
                    "login": false,
                    "yield_time_ms": 250,
                }))
                .unwrap(),
            )),
            context: WireToolContext {
                model: "model".to_owned(),
                session_id: "session".to_owned(),
                call_id: "foreground".to_owned(),
                output_token_budget: 10_000,
            },
        });
        host_write
            .write_all(&serde_json::to_vec(&start).unwrap())
            .await
            .unwrap();
        host_write.write_all(b"\n").await.unwrap();

        let mut responses = BufReader::new(host_read).lines();
        let started = tokio::time::timeout(Duration::from_secs(2), responses.next_line())
            .await
            .expect("the foreground command must yield")
            .unwrap()
            .unwrap();
        let SessionResponse::Tool(response) =
            serde_json::from_str::<SessionResponse>(&started).unwrap()
        else {
            panic!("expected the foreground command response");
        };
        assert_eq!(response.id, 0);
        assert!(response.error.is_none());
        let execution = response.execution.expect("foreground command must execute");
        assert!(
            execution.success,
            "foreground command failed: {:?}",
            execution.output
        );
        let pid = fs::read_to_string(&pid_file)
            .unwrap()
            .parse::<i32>()
            .unwrap();
        assert_eq!(kill(Pid::from_raw(pid), None), Ok(()));

        let terminate =
            SessionRequest::TerminateToolProcesses(TerminateToolProcessesRequest { id: 1 });
        host_write
            .write_all(&serde_json::to_vec(&terminate).unwrap())
            .await
            .unwrap();
        host_write.write_all(b"\n").await.unwrap();
        let terminated = tokio::time::timeout(Duration::from_secs(2), responses.next_line())
            .await
            .expect("managed process termination must complete")
            .unwrap()
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<SessionResponse>(&terminated).unwrap(),
            SessionResponse::TerminateToolProcesses(response)
                if response.id == 1 && response.error.is_none()
        ));
        assert_eq!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH));

        let ready = SessionRequest::Ready(ReadyRequest { id: 2 });
        host_write
            .write_all(&serde_json::to_vec(&ready).unwrap())
            .await
            .unwrap();
        host_write.write_all(b"\n").await.unwrap();
        let ready = responses.next_line().await.unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str::<SessionResponse>(&ready).unwrap(),
            SessionResponse::Ready(response) if response.id == 2 && response.error.is_none()
        ));

        host_write
            .write_all(
                &serde_json::to_vec(&SessionRequest::Shutdown(ShutdownRequest { id: 3 })).unwrap(),
            )
            .await
            .unwrap();
        host_write.write_all(b"\n").await.unwrap();
        drop(host_write);
        let shutdown = responses.next_line().await.unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str::<SessionResponse>(&shutdown).unwrap(),
            SessionResponse::Shutdown(response) if response.id == 3 && response.error.is_none()
        ));
        guest_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn tool_process_termination_preserves_deliberately_detached_process() {
        let workspace = tempfile::tempdir().unwrap();
        let pid_file = workspace.path().join("detached.pid");
        let (host, guest) = tokio::io::duplex(64 * 1024);
        let (host_read, mut host_write) = tokio::io::split(host);
        let (guest_read, guest_write) = tokio::io::split(guest);
        let guest_task = tokio::spawn({
            let workspace = workspace.path().to_owned();
            async move { serve_test_io(&workspace, guest_read, guest_write).await }
        });
        let test_executable = env::current_exe().unwrap();
        let test_executable = shlex::try_quote(test_executable.to_str().unwrap()).unwrap();
        let pid_file_argument = shlex::try_quote(pid_file.to_str().unwrap()).unwrap();
        let command = format!(
            "{DETACHED_PROCESS_PID_FILE_ENV}={pid_file_argument} {test_executable} \
             --exact tools::guest::tests::deliberately_detached_process_child --nocapture \
             >/dev/null 2>&1 </dev/null & child=$!; \
             while [ ! -s {pid_file_argument} ]; do kill -0 \"$child\" || exit 1; sleep 0.01; done"
        );
        let start = SessionRequest::Tool(ToolRequest {
            id: 0,
            tool: StandardTool::ExecCommand,
            input: WireToolInput::from(ToolInput::Function(
                to_raw_value(&json!({
                    "cmd": command,
                    "login": false,
                }))
                .unwrap(),
            )),
            context: WireToolContext {
                model: "model".to_owned(),
                session_id: "session".to_owned(),
                call_id: "detached".to_owned(),
                output_token_budget: 10_000,
            },
        });
        host_write
            .write_all(&serde_json::to_vec(&start).unwrap())
            .await
            .unwrap();
        host_write.write_all(b"\n").await.unwrap();

        let mut responses = BufReader::new(host_read).lines();
        let started = tokio::time::timeout(Duration::from_secs(2), responses.next_line())
            .await
            .expect("the detaching command must complete")
            .unwrap()
            .unwrap();
        let SessionResponse::Tool(response) =
            serde_json::from_str::<SessionResponse>(&started).unwrap()
        else {
            panic!("expected the detaching command response");
        };
        assert_eq!(response.id, 0);
        assert!(response.error.is_none());
        let execution = response.execution.expect("detaching command must execute");
        assert!(
            execution.success,
            "detaching command failed: {:?}",
            execution.output
        );
        let pid = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(pid) = fs::read_to_string(&pid_file) {
                    break pid.parse::<i32>().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the detached process must publish its PID");
        assert_eq!(kill(Pid::from_raw(pid), None), Ok(()));

        let terminate =
            SessionRequest::TerminateToolProcesses(TerminateToolProcessesRequest { id: 1 });
        host_write
            .write_all(&serde_json::to_vec(&terminate).unwrap())
            .await
            .unwrap();
        host_write.write_all(b"\n").await.unwrap();
        let terminated = responses.next_line().await.unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str::<SessionResponse>(&terminated).unwrap(),
            SessionResponse::TerminateToolProcesses(response)
                if response.id == 1 && response.error.is_none()
        ));
        assert_eq!(kill(Pid::from_raw(pid), None), Ok(()));
        kill(Pid::from_raw(pid), Signal::SIGKILL).unwrap();

        host_write
            .write_all(
                &serde_json::to_vec(&SessionRequest::Shutdown(ShutdownRequest { id: 2 })).unwrap(),
            )
            .await
            .unwrap();
        host_write.write_all(b"\n").await.unwrap();
        drop(host_write);
        let shutdown = responses.next_line().await.unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str::<SessionResponse>(&shutdown).unwrap(),
            SessionResponse::Shutdown(response) if response.id == 2 && response.error.is_none()
        ));
        guest_task.await.unwrap().unwrap();
    }

    #[test]
    fn deliberately_detached_process_child() {
        let Some(pid_file) = env::var_os(DETACHED_PROCESS_PID_FILE_ENV) else {
            return;
        };
        nix::unistd::setsid().unwrap();
        fs::write(pid_file, process::id().to_string()).unwrap();
        thread::sleep(Duration::from_secs(30));
    }

    #[tokio::test]
    async fn oversized_tool_response_becomes_a_scoped_failure_and_guest_stays_ready() {
        const TEST_FRAME_BYTES: usize = 1_024;

        let workspace = tempfile::tempdir().unwrap();
        let (host, guest) = tokio::io::duplex(64 * 1024);
        let (host_read, mut host_write) = tokio::io::split(host);
        let (guest_read, guest_write) = tokio::io::split(guest);
        let guest_task = tokio::spawn({
            let workspace = workspace.path().to_owned();
            async move {
                serve_test_io_with_frame_limit(
                    &workspace,
                    guest_read,
                    guest_write,
                    TEST_FRAME_BYTES,
                )
                .await
            }
        });
        let oversized = SessionRequest::Tool(ToolRequest {
            id: 0,
            tool: StandardTool::ExecCommand,
            input: WireToolInput::from(ToolInput::Function(
                to_raw_value(&json!({
                    "cmd": "/usr/bin/yes x | /usr/bin/head -c 4096",
                    "max_output_tokens": 10_000,
                }))
                .unwrap(),
            )),
            context: WireToolContext {
                model: "model".to_owned(),
                session_id: "session".to_owned(),
                call_id: "oversized".to_owned(),
                output_token_budget: 10_000,
            },
        });
        host_write
            .write_all(&serde_json::to_vec(&oversized).unwrap())
            .await
            .unwrap();
        host_write.write_all(b"\n").await.unwrap();

        let mut responses = BufReader::new(host_read).lines();
        let line = responses.next_line().await.unwrap().unwrap();
        let SessionResponse::Tool(response) =
            serde_json::from_str::<SessionResponse>(&line).unwrap()
        else {
            panic!("expected a tool response");
        };
        assert_eq!(response.id, 0);
        assert!(response.execution.is_none());
        assert!(
            response
                .error
                .is_some_and(|error| error.contains("1024-byte protocol frame limit"))
        );

        host_write
            .write_all(&serde_json::to_vec(&SessionRequest::Ready(ReadyRequest { id: 1 })).unwrap())
            .await
            .unwrap();
        host_write.write_all(b"\n").await.unwrap();
        let line = responses.next_line().await.unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str::<SessionResponse>(&line).unwrap(),
            SessionResponse::Ready(response) if response.id == 1 && response.error.is_none()
        ));

        host_write
            .write_all(
                &serde_json::to_vec(&SessionRequest::Shutdown(ShutdownRequest { id: 2 })).unwrap(),
            )
            .await
            .unwrap();
        host_write.write_all(b"\n").await.unwrap();
        drop(host_write);
        let shutdown = responses.next_line().await.unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str::<SessionResponse>(&shutdown).unwrap(),
            SessionResponse::Shutdown(response) if response.id == 2 && response.error.is_none()
        ));
        guest_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn exact_path_tracing_image_is_rejected_before_encoding_and_session_remains_usable() {
        let workspace = tempfile::tempdir().unwrap();
        let image = workspace.path().join("image.ppm");
        File::create(&image)
            .unwrap()
            .set_len(PATH_TRACING_IMAGE_BYTES)
            .unwrap();
        let (host, guest) = tokio::io::duplex(64 * 1024);
        let (host_read, mut host_write) = tokio::io::split(host);
        let (guest_read, guest_write) = tokio::io::split(guest);
        let guest_task = tokio::spawn({
            let workspace = workspace.path().to_owned();
            async move { serve_test_io(&workspace, guest_read, guest_write).await }
        });
        let view_image = SessionRequest::Tool(ToolRequest {
            id: 0,
            tool: StandardTool::ViewImage,
            input: WireToolInput::from(ToolInput::Function(
                to_raw_value(&json!({
                    "path": image,
                    "detail": "original",
                }))
                .unwrap(),
            )),
            context: WireToolContext {
                model: "model".to_owned(),
                session_id: "session".to_owned(),
                call_id: "view-image".to_owned(),
                output_token_budget: 10_000,
            },
        });
        host_write
            .write_all(&serde_json::to_vec(&view_image).unwrap())
            .await
            .unwrap();
        host_write.write_all(b"\n").await.unwrap();

        let mut responses = BufReader::new(host_read).lines();
        let line = responses.next_line().await.unwrap().unwrap();
        let SessionResponse::Tool(response) =
            serde_json::from_str::<SessionResponse>(&line).unwrap()
        else {
            panic!("expected a tool response");
        };
        assert_eq!(response.id, 0);
        assert!(response.error.is_none());
        let execution = response.execution.unwrap();
        assert!(!execution.success);
        let ToolOutputBody::Text(error) = execution.output else {
            panic!("oversized image should return a bounded text error");
        };
        assert!(error.contains("48262737 bytes"));
        assert!(error.contains("resize or convert"));

        for request in [
            SessionRequest::Cancel(CancelRequest {
                id: 1,
                target_id: 0,
            }),
            SessionRequest::Execute(ExecuteRequest {
                id: 2,
                program: "/usr/bin/true".to_owned(),
                arguments: Vec::new(),
                current_directory: workspace.path().to_string_lossy().into_owned(),
                environment: Vec::new(),
                timeout_millis: 5_000,
                max_output_bytes: DEFAULT_OUTPUT_BYTES,
                stdout_mirror: None,
                stderr_mirror: None,
            }),
        ] {
            host_write
                .write_all(&serde_json::to_vec(&request).unwrap())
                .await
                .unwrap();
            host_write.write_all(b"\n").await.unwrap();
        }

        let mut cancel_completed = false;
        let mut command_completed = false;
        while !cancel_completed || !command_completed {
            let line = tokio::time::timeout(Duration::from_secs(2), responses.next_line())
                .await
                .expect("late cancellation and follow-up command must complete")
                .unwrap()
                .unwrap();
            match serde_json::from_str::<SessionResponse>(&line).unwrap() {
                SessionResponse::Cancel(response) if response.id == 1 => {
                    assert!(response.error.is_none());
                    cancel_completed = true;
                }
                SessionResponse::Execute(response) if response.id == 2 => {
                    assert!(response.error.is_none());
                    assert!(!response.timed_out);
                    command_completed = true;
                }
                response => panic!("unexpected response ID {}", response.id()),
            }
        }

        host_write
            .write_all(
                &serde_json::to_vec(&SessionRequest::Shutdown(ShutdownRequest { id: 3 })).unwrap(),
            )
            .await
            .unwrap();
        host_write.write_all(b"\n").await.unwrap();
        drop(host_write);
        let shutdown = responses.next_line().await.unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str::<SessionResponse>(&shutdown).unwrap(),
            SessionResponse::Shutdown(response) if response.id == 3 && response.error.is_none()
        ));
        guest_task.await.unwrap().unwrap();
    }
}
