//! Explicitly authorized shell capability. The native executor is opt-in and
//! requires authorization for every command. It is NOT an OS sandbox: callers
//! must provide an isolated workspace/process environment when that is needed.
use crate::host::*;
use serde_json::json;
use std::sync::Arc;
#[derive(Clone, Debug)]
pub struct BashRequest {
    pub context: HostContext,
    pub command: String,
    pub description: String,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
}
pub trait SandboxBashExecutor: Send + Sync + 'static {
    fn execute(&self, request: BashRequest) -> HostFuture<Result<ToolOutput, String>>;
}
pub struct XaiBash<E: SandboxBashExecutor + ?Sized> {
    executor: Arc<E>,
}
impl<E: SandboxBashExecutor + ?Sized> XaiBash<E> {
    pub const fn new(executor: Arc<E>) -> Self {
        Self { executor }
    }
}
impl<E: SandboxBashExecutor + ?Sized> XaiHost for XaiBash<E> {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![definition(
            "run_terminal_cmd",
            "Execute an explicitly authorized foreground shell command. Background sessions are not installed by this adapter.",
            json!({"command":{"type":"string","maxLength":16384},"description":{"type":"string","maxLength":1024},"timeout":{"type":"integer","minimum":1,"maximum":300000,"default":120000},"is_background":{"type":"boolean","const":false}}),
            &["command", "description"],
        )]
    }
    fn call(&self, request: HostRequest) -> HostFuture<Result<ToolOutput, String>> {
        let executor = self.executor.clone();
        Box::pin(async move {
            validate_request(&request)?;
            if request.tool != "run_terminal_cmd" {
                return Err("shell tool not installed".into());
            }
            fields(
                &request.input,
                &["command", "description", "timeout", "is_background"],
            )?;
            let command = string(&request.input, "command")?.to_owned();
            let description = string(&request.input, "description")?.to_owned();
            if command.trim().is_empty() || command.len() > 16384 || description.len() > 1024 {
                return Err("command or description exceeds bounds".into());
            }
            let timeout_ms = number(&request.input, "timeout", 120000, 300000)?;
            if timeout_ms == 0 || boolean(&request.input, "is_background", false)? {
                return Err("background commands require a host task capability".into());
            }
            executor
                .execute(BashRequest {
                    context: request.context,
                    command,
                    description,
                    timeout_ms,
                    max_output_bytes: 64 * 1024,
                })
                .await
        })
    }
}
#[cfg(all(feature = "native", unix))]
type ShellAuthorizer = dyn Fn(&BashRequest) -> Result<(), String> + Send + Sync;

/// Native Unix executor. The host callback is mandatory and sees exact command,
/// deadline, capture limit and identity before a process is created. Environment
/// is cleared; only explicitly supplied variables reach the command. Process
/// groups are terminated after completion, timeout, or cancellation.
#[cfg(all(feature = "native", unix))]
pub struct AuthorizedShell {
    root: std::path::PathBuf,
    shell: std::path::PathBuf,
    env: std::collections::BTreeMap<String, String>,
    authorize: Arc<ShellAuthorizer>,
}
#[cfg(all(feature = "native", unix))]
impl AuthorizedShell {
    pub fn new(
        root: impl AsRef<std::path::Path>,
        shell: impl AsRef<std::path::Path>,
        authorize: impl Fn(&BashRequest) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
        let shell = std::fs::canonicalize(shell).map_err(|e| e.to_string())?;
        if !root.is_dir() || !shell.is_file() {
            return Err("shell requires an existing root and executable".into());
        }
        Ok(Self {
            root,
            shell,
            env: Default::default(),
            authorize: Arc::new(authorize),
        })
    }
    pub fn environment(mut self, env: std::collections::BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }
}
#[cfg(all(feature = "native", unix))]
struct ProcessGroup(nix::unistd::Pid);
#[cfg(all(feature = "native", unix))]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(self.0, nix::sys::signal::Signal::SIGKILL);
    }
}
#[cfg(all(feature = "native", unix))]
impl SandboxBashExecutor for AuthorizedShell {
    fn execute(&self, request: BashRequest) -> HostFuture<Result<ToolOutput, String>> {
        // Authorization happens before even constructing the command.
        if let Err(e) = (self.authorize)(&request) {
            return Box::pin(async move { Err(format!("shell authorization denied: {e}")) });
        }
        let root = self.root.clone();
        let shell = self.shell.clone();
        let env = self.env.clone();
        Box::pin(async move {
            use std::process::Stdio;
            let mut command = tokio::process::Command::new(shell);
            command
                .arg("-c")
                .arg(&request.command)
                .current_dir(root)
                .env_clear()
                .envs(env)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .process_group(0);
            let mut child = command.spawn().map_err(|e| e.to_string())?;
            let group = ProcessGroup(nix::unistd::Pid::from_raw(
                child.id().ok_or("missing process id")? as i32,
            ));
            let stdout = child.stdout.take().ok_or("missing stdout")?;
            let stderr = child.stderr.take().ok_or("missing stderr")?;
            let run = async {
                let (status, out, err) = tokio::join!(
                    child.wait(),
                    capture(stdout, request.max_output_bytes),
                    capture(stderr, request.max_output_bytes)
                );
                Ok::<_, String>((status.map_err(|e| e.to_string())?, out?, err?))
            };
            let result =
                tokio::time::timeout(std::time::Duration::from_millis(request.timeout_ms), run)
                    .await;
            drop(group);
            match result {
                Ok(Ok((status, (stdout, out_cut), (stderr, err_cut)))) => {
                    let value = json!({"stdout":stdout,"stderr":stderr,"exit_code":status.code(),"truncated":out_cut||err_cut});
                    let mut result = ToolOutput::text(value.to_string())
                        .with_structured_result(value)
                        .with_metadata(
                            json!({"call_id":request.context.call_id,"truncated":out_cut||err_cut}),
                        );
                    result.is_error = !status.success();
                    Ok(result)
                }
                Ok(Err(e)) => Err(e),
                Err(_) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    Err("shell command timed out; process group terminated".into())
                }
            }
        })
    }
}
#[cfg(all(feature = "native", unix))]
async fn capture(
    mut stream: impl tokio::io::AsyncRead + Unpin,
    max: usize,
) -> Result<(String, bool), String> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let mut truncated = false;
    loop {
        let n = stream.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let take = n.min(max.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&buffer[..take]);
        truncated |= take < n;
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}
