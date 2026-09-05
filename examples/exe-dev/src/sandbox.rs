//! Caller-owned exe.dev sandbox tools for an agent running outside the guest.

use std::{
    path::PathBuf,
    process::{ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use nanocodex::{
    Tool, Tools,
    tools::{
        ToolContext, ToolDefinition, ToolExposure, ToolInput, ToolOutput, ToolResult,
        ToolsBuildError, contract::async_trait,
    },
};
#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{Mutex, mpsc},
};

const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_OUTPUT_LIMIT: usize = 128 * 1024;
const MAX_SCRIPT_BYTES: usize = 32 * 1024;
const REMOTE_COMMAND_TIMEOUT_SECONDS: u64 = 120;

/// Caller-selected cleanup policy for one owned remote sandbox.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxCleanup {
    /// Leave the VM running for follow-up work.
    Retain,
    /// Delete the exact VM created by this sandbox handle.
    Delete,
}

/// Configuration for one application-owned exe.dev VM.
#[derive(Clone, Debug)]
pub struct SandboxConfig {
    /// Exact VM name unavailable for model selection.
    pub name: String,
    /// SSH executable used for both lobby and guest calls.
    pub ssh_binary: PathBuf,
    /// Deadline for each local SSH process.
    pub command_timeout: Duration,
    /// Combined retained stdout and stderr limit per call.
    pub output_limit: usize,
}

impl SandboxConfig {
    /// Creates conservative defaults around one exact VM name.
    pub fn new(name: impl Into<String>) -> Result<Self, SandboxError> {
        let name = name.into();
        validate_vm_name(&name)?;
        Ok(Self {
            name,
            ssh_binary: PathBuf::from("ssh"),
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
            output_limit: DEFAULT_OUTPUT_LIMIT,
        })
    }
}

/// Invalid policy or failed exe.dev control operation.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// VM names are restricted before they enter an SSH argument.
    #[error("invalid exe.dev VM name `{0}`")]
    InvalidName(String),
    /// A process-backed operation could not be started or observed.
    #[error("exe.dev SSH operation failed: {0}")]
    Io(#[from] std::io::Error),
}

/// One retained, application-owned exe.dev sandbox exposed through narrow tools.
#[derive(Clone)]
pub struct ExeDevSandbox {
    inner: Arc<SandboxInner>,
}

struct SandboxInner {
    name: String,
    transport: Arc<dyn SandboxTransport>,
    state: Mutex<SandboxState>,
}

#[derive(Default)]
struct SandboxState {
    created: bool,
}

impl ExeDevSandbox {
    /// Builds a sandbox handle without creating its VM.
    pub fn new(config: SandboxConfig) -> Result<Self, SandboxError> {
        validate_vm_name(&config.name)?;
        let transport = SshTransport {
            binary: config.ssh_binary,
            timeout: config.command_timeout,
            output_limit: config.output_limit,
            known_hosts: tempfile::NamedTempFile::new()?.into_temp_path(),
            #[cfg(unix)]
            control_directory: tempfile::Builder::new()
                .prefix("ncx-ssh-")
                .tempdir_in("/tmp")?,
        };
        Ok(Self::with_transport(config.name, Arc::new(transport)))
    }

    fn with_transport(name: String, transport: Arc<dyn SandboxTransport>) -> Self {
        Self {
            inner: Arc::new(SandboxInner {
                name,
                transport,
                state: Mutex::new(SandboxState::default()),
            }),
        }
    }

    /// Returns the exact caller-owned VM name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Builds the three model-callable tools without host workspace access.
    pub fn tools(&self) -> Result<Tools, ToolsBuildError> {
        Tools::builder()
            .without_defaults()
            .exposure(ToolExposure::DirectAndCodeMode)
            .tool(CreateSandbox(self.clone()))
            .tool(ExecSandbox(self.clone()))
            .tool(SandboxInfo(self.clone()))
            .build()
    }

    /// Applies caller-owned cleanup after the agent turn has finished.
    pub async fn cleanup(
        &self,
        policy: SandboxCleanup,
    ) -> Result<Option<SandboxOperation>, SandboxError> {
        let mut operation = if policy == SandboxCleanup::Delete {
            let mut state = self.inner.state.lock().await;
            if state.created {
                let output = self
                    .inner
                    .transport
                    .lobby(vec![
                        "rm".to_owned(),
                        self.inner.name.clone(),
                        "--json".to_owned(),
                    ])
                    .await?;
                let success = output.success();
                if success {
                    state.created = false;
                }
                Some(SandboxOperation::from_process(
                    self.info(state.created),
                    output,
                ))
            } else {
                None
            }
        } else {
            None
        };
        let closed = self.close_connections().await?;
        if let Some(operation) = &mut operation {
            operation.control_masters_closed = closed;
        }
        Ok(operation)
    }

    async fn close_connections(&self) -> Result<usize, SandboxError> {
        let guest = self
            .inner
            .transport
            .close(&format!("{}.exe.xyz", self.inner.name))
            .await;
        let lobby = self.inner.transport.close("exe.dev").await;
        Ok(usize::from(guest?) + usize::from(lobby?))
    }

    fn info(&self, created: bool) -> SandboxIdentity {
        SandboxIdentity {
            name: self.inner.name.clone(),
            ssh_destination: format!("{}.exe.xyz", self.inner.name),
            preview_url: format!("https://{}.exe.xyz", self.inner.name),
            created,
        }
    }

    async fn create(&self) -> Result<SandboxOperation, SandboxError> {
        let mut state = self.inner.state.lock().await;
        if state.created {
            return Ok(SandboxOperation {
                sandbox: self.info(true),
                exit_code: Some(0),
                stdout: "sandbox already created by this agent session".to_owned(),
                stderr: String::new(),
                timed_out: false,
                output_limit_exceeded: false,
                connection_reused: false,
                control_master_pid: None,
                control_masters_closed: 0,
                wall_time_seconds: 0.0,
            });
        }
        let output = self
            .inner
            .transport
            .lobby(vec![
                "new".to_owned(),
                format!("--name={}", self.inner.name),
                "--tag=nanocodex-tool".to_owned(),
                "--no-email".to_owned(),
                "--json".to_owned(),
            ])
            .await?;
        state.created = output.success();
        Ok(SandboxOperation::from_process(
            self.info(state.created),
            output,
        ))
    }

    async fn execute(&self, script: String) -> Result<SandboxOperation, SandboxError> {
        if script.trim().is_empty() || script.len() > MAX_SCRIPT_BYTES {
            return Err(SandboxError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("command must contain 1..={MAX_SCRIPT_BYTES} bytes"),
            )));
        }
        let created = self.inner.state.lock().await.created;
        if !created {
            return Err(SandboxError::Io(std::io::Error::other(
                "sandbox has not been created; call sandbox_create first",
            )));
        }
        let output = self
            .inner
            .transport
            .guest(&format!("{}.exe.xyz", self.inner.name), script)
            .await?;
        Ok(SandboxOperation::from_process(self.info(true), output))
    }
}

/// Stable identity and private preview for the caller-owned VM.
#[derive(Clone, Debug, Serialize)]
pub struct SandboxIdentity {
    /// Exact application-selected name.
    pub name: String,
    /// SSH destination derived from the validated name.
    pub ssh_destination: String,
    /// Private exe.dev HTTPS endpoint on port 8000.
    pub preview_url: String,
    /// Whether this handle successfully created the VM.
    pub created: bool,
}

/// Structured result returned by create, execute, and cleanup operations.
#[derive(Debug, Serialize)]
pub struct SandboxOperation {
    /// Stable sandbox identity.
    pub sandbox: SandboxIdentity,
    /// SSH process exit code, absent after timeout or signal termination.
    pub exit_code: Option<i32>,
    /// Bounded standard output.
    pub stdout: String,
    /// Bounded standard error.
    pub stderr: String,
    /// Whether the local operation reached its deadline.
    pub timed_out: bool,
    /// Whether combined output crossed the configured bound.
    pub output_limit_exceeded: bool,
    /// Whether this operation reused an already-active SSH control master.
    pub connection_reused: bool,
    /// Active OpenSSH control-master PID observed after the operation.
    pub control_master_pid: Option<u32>,
    /// Number of control masters explicitly closed after caller cleanup.
    pub control_masters_closed: usize,
    /// Local wall-clock duration.
    pub wall_time_seconds: f64,
}

impl SandboxOperation {
    fn from_process(sandbox: SandboxIdentity, output: ProcessOutput) -> Self {
        Self {
            sandbox,
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            timed_out: output.timed_out,
            output_limit_exceeded: output.output_limit_exceeded,
            connection_reused: output.connection_reused,
            control_master_pid: output.control_master_pid,
            control_masters_closed: 0,
            wall_time_seconds: output.wall_time_seconds,
        }
    }

    fn success(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out && !self.output_limit_exceeded
    }

    fn tool_output(&self) -> ToolOutput {
        ToolOutput::from_json(
            serde_json::to_value(self).unwrap_or_else(
                |error| json!({"error": format!("failed to encode sandbox result: {error}")}),
            ),
            self.success(),
        )
        .with_process_trace(
            self.exit_code,
            None,
            None,
            self.stdout.len().saturating_add(self.stderr.len()),
            self.wall_time_seconds,
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecInput {
    command: String,
}

struct CreateSandbox(ExeDevSandbox);

#[async_trait]
impl Tool for CreateSandbox {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "sandbox_create",
            "Create the one caller-named private exe.dev VM owned by this session. This is idempotent within the session and must run before sandbox_exec.",
            empty_schema(),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let EmptyInput {} = input.decode_json()?;
        Ok(self.0.create().await?.tool_output())
    }
}

struct ExecSandbox(ExeDevSandbox);

#[async_trait]
impl Tool for ExecSandbox {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "sandbox_exec",
            "Run a non-interactive bash script inside the caller-owned exe.dev VM. The command is sent over SSH stdin, never run on the Nanocodex host, and has bounded time and output.",
            json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_SCRIPT_BYTES,
                        "description": "Complete bash script to execute in the remote VM."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let ExecInput { command } = input.decode_json()?;
        Ok(self.0.execute(command).await?.tool_output())
    }
}

struct SandboxInfo(ExeDevSandbox);

#[async_trait]
impl Tool for SandboxInfo {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "sandbox_info",
            "Return the fixed identity, private preview URL, and creation state of the caller-owned exe.dev VM without changing it.",
            empty_schema(),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let EmptyInput {} = input.decode_json()?;
        let created = self.0.inner.state.lock().await.created;
        Ok(ToolOutput::json(&self.0.info(created)))
    }
}

fn empty_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
}

fn validate_vm_name(name: &str) -> Result<(), SandboxError> {
    let valid = (3..=63).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric);
    if valid {
        Ok(())
    } else {
        Err(SandboxError::InvalidName(name.to_owned()))
    }
}

#[async_trait]
trait SandboxTransport: Send + Sync {
    async fn lobby(&self, arguments: Vec<String>) -> std::io::Result<ProcessOutput>;
    async fn guest(&self, destination: &str, script: String) -> std::io::Result<ProcessOutput>;
    async fn close(&self, destination: &str) -> std::io::Result<bool>;
}

struct SshTransport {
    binary: PathBuf,
    timeout: Duration,
    output_limit: usize,
    known_hosts: tempfile::TempPath,
    #[cfg(unix)]
    control_directory: tempfile::TempDir,
}

#[async_trait]
impl SandboxTransport for SshTransport {
    async fn lobby(&self, arguments: Vec<String>) -> std::io::Result<ProcessOutput> {
        self.run("exe.dev", arguments, None, false).await
    }

    async fn guest(&self, destination: &str, script: String) -> std::io::Result<ProcessOutput> {
        self.run(
            destination,
            vec![
                "timeout".to_owned(),
                "--signal=TERM".to_owned(),
                "--kill-after=5s".to_owned(),
                format!("{REMOTE_COMMAND_TIMEOUT_SECONDS}s"),
                "bash".to_owned(),
                "-se".to_owned(),
            ],
            Some(script.into_bytes()),
            true,
        )
        .await
    }

    async fn close(&self, destination: &str) -> std::io::Result<bool> {
        self.close_master(destination).await
    }
}

impl SshTransport {
    async fn run(
        &self,
        destination: &str,
        arguments: Vec<String>,
        input: Option<Vec<u8>>,
        accept_new_host: bool,
    ) -> std::io::Result<ProcessOutput> {
        let existing_master = self.control_master(destination).await?;
        let mut command = Command::new(&self.binary);
        command
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-o")
            .arg("ConnectionAttempts=6");
        self.configure_multiplexing(&mut command);
        if accept_new_host {
            command
                .arg("-o")
                .arg("StrictHostKeyChecking=accept-new")
                .arg("-o")
                .arg(format!("UserKnownHostsFile={}", self.known_hosts.display()));
        }
        command
            .arg(destination)
            .args(arguments)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut output = bounded_output(command, input, self.timeout, self.output_limit).await?;
        let active_master = self.control_master(destination).await?;
        output.connection_reused = existing_master.is_some();
        output.control_master_pid = active_master.and_then(|master| master.pid);
        Ok(output)
    }

    #[cfg(unix)]
    fn configure_multiplexing(&self, command: &mut Command) {
        command
            .arg("-o")
            .arg("ControlMaster=auto")
            .arg("-o")
            .arg("ControlPersist=60")
            .arg("-o")
            .arg(format!("ControlPath={}", self.control_path().display()));
    }

    #[cfg(not(unix))]
    fn configure_multiplexing(&self, _command: &mut Command) {}

    #[cfg(unix)]
    fn control_path(&self) -> PathBuf {
        self.control_directory.path().join("%C")
    }

    #[cfg(unix)]
    async fn control_master(&self, destination: &str) -> std::io::Result<Option<ControlMaster>> {
        let mut command = Command::new(&self.binary);
        command
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg(format!("ControlPath={}", self.control_path().display()))
            .arg("-O")
            .arg("check")
            .arg(destination)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(3), command.output())
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "SSH control check timed out")
            })??;
        if !output.status.success() {
            return Ok(None);
        }
        let message = String::from_utf8_lossy(&output.stderr);
        Ok(Some(ControlMaster {
            pid: parse_control_master_pid(&message),
        }))
    }

    #[cfg(not(unix))]
    async fn control_master(&self, _destination: &str) -> std::io::Result<Option<ControlMaster>> {
        Ok(None)
    }

    #[cfg(unix)]
    async fn close_master(&self, destination: &str) -> std::io::Result<bool> {
        if self.control_master(destination).await?.is_none() {
            return Ok(false);
        }
        let mut command = Command::new(&self.binary);
        command
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg(format!("ControlPath={}", self.control_path().display()))
            .arg("-O")
            .arg("exit")
            .arg(destination)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(3), command.output())
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "SSH control shutdown timed out",
                )
            })??;
        if output.status.success() {
            Ok(true)
        } else {
            Err(std::io::Error::other(format!(
                "failed to close SSH control master for {destination}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }

    #[cfg(not(unix))]
    async fn close_master(&self, _destination: &str) -> std::io::Result<bool> {
        Ok(false)
    }
}

struct ControlMaster {
    pid: Option<u32>,
}

fn parse_control_master_pid(message: &str) -> Option<u32> {
    let suffix = message.split_once("pid=")?.1;
    let digits = suffix
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits.parse().ok()
}

struct ProcessOutput {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
    output_limit_exceeded: bool,
    connection_reused: bool,
    control_master_pid: Option<u32>,
    wall_time_seconds: f64,
}

impl ProcessOutput {
    fn success(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out && !self.output_limit_exceeded
    }
}

async fn bounded_output(
    mut command: Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    output_limit: usize,
) -> std::io::Result<ProcessOutput> {
    let started = Instant::now();
    let mut child = command.spawn()?;
    let process_group = process_group_value(&child);
    #[cfg(unix)]
    let mut process_group_guard = ProcessGroupGuard(process_group);

    if let Some(input) = input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("SSH stdin was not piped"))?;
        stdin.write_all(&input).await?;
        stdin.shutdown().await?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("SSH stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("SSH stderr was not piped"))?;
    let retained = Arc::new(AtomicUsize::new(0));
    let (limit_sender, mut limit_receiver) = mpsc::channel(1);
    let mut stdout_task = tokio::spawn(read_bounded(
        stdout,
        Arc::clone(&retained),
        output_limit,
        limit_sender.clone(),
    ));
    let mut stderr_task = tokio::spawn(read_bounded(
        stderr,
        Arc::clone(&retained),
        output_limit,
        limit_sender,
    ));
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    let outcome = tokio::select! {
        status = child.wait() => WaitOutcome::Exited(status?),
        () = &mut deadline => WaitOutcome::TimedOut,
        Some(()) = limit_receiver.recv() => WaitOutcome::OutputLimitExceeded,
    };
    if !matches!(outcome, WaitOutcome::Exited(_)) {
        kill_child(&mut child, process_group)?;
        child.wait().await?;
    }
    let (stdout, stderr) = tokio::time::timeout(Duration::from_secs(1), async {
        let stdout = (&mut stdout_task).await.map_err(std::io::Error::other)??;
        let stderr = (&mut stderr_task).await.map_err(std::io::Error::other)??;
        Ok::<_, std::io::Error>((stdout, stderr))
    })
    .await
    .map_err(|_| std::io::Error::other("SSH descendants kept output pipes open"))??;
    #[cfg(unix)]
    process_group_guard.disarm();

    let output_limit_exceeded = matches!(outcome, WaitOutcome::OutputLimitExceeded)
        || retained.load(Ordering::Relaxed) > output_limit;
    Ok(ProcessOutput {
        exit_code: match outcome {
            WaitOutcome::Exited(status) => status.code(),
            WaitOutcome::TimedOut | WaitOutcome::OutputLimitExceeded => None,
        },
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        timed_out: matches!(outcome, WaitOutcome::TimedOut),
        output_limit_exceeded,
        connection_reused: false,
        control_master_pid: None,
        wall_time_seconds: started.elapsed().as_secs_f64(),
    })
}

enum WaitOutcome {
    Exited(ExitStatus),
    TimedOut,
    OutputLimitExceeded,
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    retained: Arc<AtomicUsize>,
    limit: usize,
    limit_sender: mpsc::Sender<()>,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    let mut reported = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(output);
        }
        let previous = retained.fetch_add(read, Ordering::Relaxed);
        let available = limit.saturating_sub(previous);
        output.extend_from_slice(&buffer[..read.min(available)]);
        if previous.saturating_add(read) > limit && !reported {
            reported = true;
            let _ = limit_sender.try_send(());
        }
    }
}

#[cfg(unix)]
fn process_group_value(child: &Child) -> Option<Pid> {
    child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .map(Pid::from_raw)
}

#[cfg(not(unix))]
fn process_group_value(_child: &Child) -> Option<()> {
    None
}

#[cfg(unix)]
fn kill_child(child: &mut Child, process_group: Option<Pid>) -> std::io::Result<()> {
    let child_kill = child.start_kill();
    if let Some(process_group) = process_group {
        match killpg(process_group, Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(Errno::EPERM) => child_kill.or(Ok(())),
            Err(error) => Err(std::io::Error::other(error)),
        }
    } else {
        child_kill
    }
}

#[cfg(not(unix))]
fn kill_child(child: &mut Child, _process_group: Option<()>) -> std::io::Result<()> {
    child.start_kill()
}

#[cfg(unix)]
struct ProcessGroupGuard(Option<Pid>);

#[cfg(unix)]
impl ProcessGroupGuard {
    const fn disarm(&mut self) {
        self.0 = None;
    }
}

#[cfg(unix)]
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if let Some(process_group) = self.0 {
            let _ = killpg(process_group, Signal::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex as StdMutex};

    use super::*;

    #[derive(Default)]
    struct FakeTransport {
        calls: StdMutex<Vec<String>>,
        outputs: StdMutex<VecDeque<ProcessOutput>>,
    }

    impl FakeTransport {
        fn successful() -> ProcessOutput {
            ProcessOutput {
                exit_code: Some(0),
                stdout: "ok".to_owned(),
                stderr: String::new(),
                timed_out: false,
                output_limit_exceeded: false,
                connection_reused: false,
                control_master_pid: None,
                wall_time_seconds: 0.01,
            }
        }

        fn push(&self, output: ProcessOutput) {
            self.outputs.lock().unwrap().push_back(output);
        }
    }

    #[async_trait]
    impl SandboxTransport for FakeTransport {
        async fn lobby(&self, arguments: Vec<String>) -> std::io::Result<ProcessOutput> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("lobby {}", arguments.join(" ")));
            self.outputs
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| std::io::Error::other("missing fake output"))
        }

        async fn guest(&self, destination: &str, script: String) -> std::io::Result<ProcessOutput> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("guest {destination} {script}"));
            self.outputs
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| std::io::Error::other("missing fake output"))
        }

        async fn close(&self, destination: &str) -> std::io::Result<bool> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("close {destination}"));
            Ok(true)
        }
    }

    fn sandbox(transport: Arc<FakeTransport>) -> ExeDevSandbox {
        ExeDevSandbox::with_transport("nanocodex-test".to_owned(), transport)
    }

    #[test]
    fn names_are_validated_before_entering_ssh_arguments() {
        assert!(SandboxConfig::new("nanocodex-test").is_ok());
        assert!(SandboxConfig::new("UPPER").is_err());
        assert!(SandboxConfig::new("-leading").is_err());
        assert!(SandboxConfig::new("two words").is_err());
    }

    #[tokio::test]
    async fn create_is_idempotent_and_targets_one_owned_name() {
        let transport = Arc::new(FakeTransport::default());
        transport.push(FakeTransport::successful());
        let sandbox = sandbox(transport.clone());

        let first = sandbox.create().await.unwrap();
        let second = sandbox.create().await.unwrap();

        assert!(first.success());
        assert!(second.success());
        assert_eq!(
            transport.calls.lock().unwrap().as_slice(),
            ["lobby new --name=nanocodex-test --tag=nanocodex-tool --no-email --json"]
        );
    }

    #[tokio::test]
    async fn command_never_runs_before_successful_creation() {
        let transport = Arc::new(FakeTransport::default());
        let sandbox = sandbox(transport.clone());

        let error = sandbox.execute("uname -a".to_owned()).await.unwrap_err();

        assert!(error.to_string().contains("has not been created"));
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn command_is_sent_only_to_the_owned_guest() {
        let transport = Arc::new(FakeTransport::default());
        transport.push(FakeTransport::successful());
        transport.push(FakeTransport::successful());
        let sandbox = sandbox(transport.clone());
        sandbox.create().await.unwrap();

        let output = sandbox.execute("printf proof".to_owned()).await.unwrap();

        assert!(output.success());
        assert_eq!(
            transport.calls.lock().unwrap().last().unwrap(),
            "guest nanocodex-test.exe.xyz printf proof"
        );
    }

    #[tokio::test]
    async fn deletion_is_caller_owned_and_exact() {
        let transport = Arc::new(FakeTransport::default());
        transport.push(FakeTransport::successful());
        transport.push(FakeTransport::successful());
        let sandbox = sandbox(transport.clone());
        sandbox.create().await.unwrap();

        assert!(
            sandbox
                .cleanup(SandboxCleanup::Retain)
                .await
                .unwrap()
                .is_none()
        );
        let deleted = sandbox
            .cleanup(SandboxCleanup::Delete)
            .await
            .unwrap()
            .unwrap();

        assert!(deleted.success());
        let calls = transport.calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .any(|call| call == "lobby rm nanocodex-test --json")
        );
        assert!(
            calls
                .iter()
                .any(|call| call == "close nanocodex-test.exe.xyz")
        );
        assert!(calls.iter().any(|call| call == "close exe.dev"));
        assert_eq!(deleted.control_masters_closed, 2);
    }

    #[test]
    fn parses_openssh_control_master_pid() {
        assert_eq!(
            parse_control_master_pid("Master running (pid=4217)\r\n"),
            Some(4217)
        );
        assert_eq!(parse_control_master_pid("Control socket absent"), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_output_is_bounded_while_produced() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("while :; do printf 1234567890; done")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command.process_group(0);

        let output = bounded_output(command, None, Duration::from_secs(5), 32)
            .await
            .unwrap();

        assert!(output.output_limit_exceeded);
        assert!(!output.timed_out);
        assert!(output.stdout.len().saturating_add(output.stderr.len()) <= 32);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_deadline_terminates_the_local_ssh_group() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command.process_group(0);

        let output = bounded_output(command, None, Duration::from_millis(50), 1024)
            .await
            .unwrap();

        assert!(output.timed_out);
        assert_eq!(output.exit_code, None);
        assert!(output.wall_time_seconds < 2.0);
    }
}
