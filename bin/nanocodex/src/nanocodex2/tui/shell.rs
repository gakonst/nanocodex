// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Bounded local shell execution and model-context formatting.

use crate::tui::transcript::ShellId;
use std::{
    env, io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    task::JoinHandle,
    time::timeout,
};

const MAX_CAPTURE_BYTES: usize = 16 * 1024;
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(100);
type SharedOutput = Arc<Mutex<Vec<u8>>>;

#[derive(Debug)]
pub(crate) struct ShellExecution {
    pub(crate) id: ShellId,
    pub(crate) command: String,
    pub(crate) output: String,
    pub(crate) exit_code: Option<i32>,
    pub(crate) duration_ns: u64,
    pub(crate) truncated: bool,
    pub(crate) error: Option<String>,
}

impl ShellExecution {
    pub(crate) fn model_context(&self) -> String {
        let outcome = match (&self.error, self.exit_code) {
            (Some(error), _) => error.clone(),
            (None, Some(code)) => format!("exit {code}"),
            (None, None) => "terminated without an exit code".to_owned(),
        };
        format!(
            "<local_shell_result>\ncommand: {}\noutcome: {}\noutput:\n{}\n</local_shell_result>",
            escape_boundary(&self.command),
            outcome,
            escape_boundary(&self.output),
        )
    }
}

fn escape_boundary(text: &str) -> String {
    text.replace("</local_shell_result>", "&lt;/local_shell_result&gt;")
}

pub(crate) async fn execute(id: ShellId, command: String, workspace: PathBuf) -> ShellExecution {
    let started = Instant::now();
    let mut process = shell_command(&command, &workspace);
    let result = run(&mut process).await;
    let duration_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    match result {
        Ok(output) => ShellExecution {
            id,
            command,
            output: output.text,
            exit_code: output.exit_code,
            duration_ns,
            truncated: output.truncated,
            error: None,
        },
        Err(error) => ShellExecution {
            id,
            command,
            output: String::new(),
            exit_code: None,
            duration_ns,
            truncated: false,
            error: Some(error.to_string()),
        },
    }
}

struct CapturedOutput {
    text: String,
    exit_code: Option<i32>,
    truncated: bool,
}

async fn run(command: &mut Command) -> io::Result<CapturedOutput> {
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let remaining = Arc::new(AtomicUsize::new(MAX_CAPTURE_BYTES));
    let truncated = Arc::new(AtomicBool::new(false));
    let stdout_output = Arc::new(Mutex::new(Vec::new()));
    let stderr_output = Arc::new(Mutex::new(Vec::new()));
    let stdout_task = tokio::spawn(read_bounded(
        stdout,
        Arc::clone(&remaining),
        Arc::clone(&truncated),
        Arc::clone(&stdout_output),
    ));
    let stderr_task = tokio::spawn(read_bounded(
        stderr,
        Arc::clone(&remaining),
        Arc::clone(&truncated),
        Arc::clone(&stderr_output),
    ));
    let status = child.wait().await?;
    finish_reader(stdout_task).await?;
    finish_reader(stderr_task).await?;
    let stdout = take_output(stdout_output)?;
    let stderr = take_output(stderr_output)?;
    let mut text = String::from_utf8_lossy(&stdout).into_owned();
    if !stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&stderr));
    }
    let truncated = truncated.load(Ordering::Relaxed);
    if truncated {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str("… output truncated by nanocodex2");
    }
    Ok(CapturedOutput {
        text,
        exit_code: status.code(),
        truncated,
    })
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    remaining: Arc<AtomicUsize>,
    truncated: Arc<AtomicBool>,
    captured: SharedOutput,
) -> io::Result<()> {
    let mut buffer = [0_u8; 4096];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        let allowed = remaining
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                Some(left.saturating_sub(read))
            })
            .unwrap_or(0)
            .min(read);
        captured
            .lock()
            .map_err(|_| io::Error::other("shell output capture lock poisoned"))?
            .extend_from_slice(&buffer[..allowed]);
        if allowed < read {
            truncated.store(true, Ordering::Relaxed);
        }
    }
}

async fn finish_reader(mut task: JoinHandle<io::Result<()>>) -> io::Result<()> {
    match timeout(OUTPUT_DRAIN_GRACE, &mut task).await {
        Ok(result) => result.map_err(io::Error::other)?,
        Err(_) => {
            // Descendants can inherit the shell's pipes. Once the shell has exited, they must not
            // keep its UI entry running; retain the bytes already captured and stop draining.
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Ok(()),
                result => result.map_err(io::Error::other)?,
            }
        }
    }
}

fn take_output(output: SharedOutput) -> io::Result<Vec<u8>> {
    Arc::try_unwrap(output)
        .map_err(|_| io::Error::other("shell output reader did not stop"))?
        .into_inner()
        .map_err(|_| io::Error::other("shell output capture lock poisoned"))
}

#[cfg(unix)]
fn shell_command(command: &str, workspace: &Path) -> Command {
    let shell = env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    let mut process = Command::new(shell);
    process.args(["-lc", command]);
    configure(&mut process, workspace);
    process
}

#[cfg(windows)]
fn shell_command(command: &str, workspace: &Path) -> Command {
    let mut process = Command::new("cmd");
    process.args(["/C", command]);
    configure(&mut process, workspace);
    process
}

fn configure(command: &mut Command, workspace: &Path) {
    command
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
}

#[cfg(test)]
mod tests {
    use super::{MAX_CAPTURE_BYTES, execute, read_bounded, take_output};
    use crate::tui::transcript::ShellId;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tokio::time::{Duration, timeout};

    #[tokio::test]
    async fn capture_is_bounded_while_the_reader_is_fully_drained() {
        let input = vec![b'x'; MAX_CAPTURE_BYTES + 10];
        let remaining = Arc::new(AtomicUsize::new(MAX_CAPTURE_BYTES));
        let truncated = Arc::new(AtomicBool::new(false));

        let captured = Arc::new(Mutex::new(Vec::new()));
        read_bounded(
            input.as_slice(),
            remaining,
            Arc::clone(&truncated),
            Arc::clone(&captured),
        )
        .await
        .unwrap();
        let captured = take_output(captured).unwrap();

        assert_eq!(captured.len(), MAX_CAPTURE_BYTES);
        assert!(truncated.load(Ordering::Relaxed));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execution_uses_the_workspace_and_captures_both_streams() {
        let workspace = tempfile::tempdir().unwrap();

        let result = execute(
            ShellId::new(1),
            "pwd; printf stderr >&2; exit 7".to_owned(),
            workspace.path().to_path_buf(),
        )
        .await;

        assert_eq!(result.exit_code, Some(7));
        assert!(result.output.contains(workspace.path().to_str().unwrap()));
        assert!(result.output.contains("stderr"));
        assert!(result.error.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execution_finishes_when_a_descendant_keeps_output_pipes_open() {
        let workspace = tempfile::tempdir().unwrap();
        let execution = timeout(
            Duration::from_secs(5),
            execute(
                ShellId::new(2),
                "sh -c '(sleep 10) & printf done'".to_owned(),
                workspace.path().to_path_buf(),
            ),
        )
        .await
        .expect("the shell exit should resolve without waiting for descendants");

        assert_eq!(execution.exit_code, Some(0));
        assert_eq!(execution.output, "done");
    }
}
