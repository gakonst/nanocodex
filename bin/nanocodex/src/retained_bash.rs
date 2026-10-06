//! Retained Bash jobs. Each job owns a workspace runtime so stopping one cannot
//! cancel another. The existing foreground executor supplies capture/deadlines.
use nanocodex_claude_tools::{
    BashRequest, BashResult, ClaudeBash, SandboxBashExecutor, ToolOutput,
};
use nanocodex_oai_tools::{ToolContext, ToolInput, workspace_runtime::WorkspaceToolRuntime};
use serde_json::{Value, json, value::to_raw_value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    sync::{Mutex, watch},
    time::{Duration, Instant},
};

// The host retains the lease for the entire foreground/background execution.
pub(super) type PinWorkspace = Arc<dyn Fn() -> (PathBuf, Box<dyn Send + Sync>) + Send + Sync>;
pub(super) type Completion = Arc<dyn Fn(String, String, &Result<String, String>) + Send + Sync>;
const fn text_reply(text: String) -> ToolOutput {
    // Preserve the CLI's existing text-only receipt (including absent metadata)
    // while letting either host adapt the provider-neutral result.
    ToolOutput {
        content: nanocodex_claude_tools::ToolContent::Text(text),
        is_error: false,
        structured_result: None,
        metadata: None,
    }
}

struct Job {
    runtime: Arc<WorkspaceToolRuntime>,
    worker: Option<tokio::task::JoinHandle<()>>,
    result: watch::Receiver<Option<std::result::Result<String, String>>>,
    stopped: bool,
}
impl Drop for Job {
    fn drop(&mut self) {
        if let Some(worker) = &self.worker {
            worker.abort();
        }
        let runtime = self.runtime.clone();
        tokio::spawn(async move { runtime.control().cancel().await });
    }
}

const FOREGROUND_DEFAULT_MS: u64 = 120000;
const FOREGROUND_MAX_MS: u64 = 600000;
const BACKGROUND_DEFAULT_MS: u64 = 1800000;
const BACKGROUND_MAX_MS: u64 = 7200000;

struct BackgroundLimits {
    default_ms: u64,
    maximum_ms: u64,
}
impl BackgroundLimits {
    fn read() -> std::result::Result<Self, String> {
        fn value(name: &str, floor: u64) -> std::result::Result<u64, String> {
            match std::env::var(name) {
                Err(std::env::VarError::NotPresent) => Ok(floor),
                Ok(raw) if !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()) => {
                    let parsed = raw
                        .parse::<u64>()
                        .map_err(|_| format!("{name} exceeds supported integer milliseconds"))?;
                    Ok(parsed.max(floor))
                }
                _ => Err(format!("{name} must be nonnegative integer milliseconds")),
            }
        }
        let default_ms = value("BASH_DEFAULT_TIMEOUT_MS", BACKGROUND_DEFAULT_MS)?;
        let maximum_ms = value("BASH_MAX_TIMEOUT_MS", BACKGROUND_MAX_MS)?.max(default_ms);
        // Promotion must safely add the preceding foreground window. Validate
        // both arithmetic and the actual monotonic clock range before spawning.
        let combined = maximum_ms
            .checked_add(FOREGROUND_MAX_MS)
            .ok_or("Bash background timeout exceeds supported clock range")?;
        Instant::now()
            .checked_add(Duration::from_millis(combined))
            .ok_or("Bash background timeout exceeds supported clock range")?;
        Ok(Self {
            default_ms,
            maximum_ms,
        })
    }
}

pub(super) struct Shell {
    workspace: PinWorkspace,
    cwd: Mutex<(PathBuf, PathBuf)>,
    jobs: Mutex<BTreeMap<String, Job>>,
    completion: Option<Completion>,
}
impl Shell {
    pub(super) fn new(workspace: PinWorkspace, completion: Option<Completion>) -> Self {
        let (current, _lease) = workspace();
        Self {
            workspace,
            completion,
            cwd: Mutex::new((current.clone(), current)),
            jobs: Mutex::new(BTreeMap::new()),
        }
    }
    // The native CLI publishes this definition; the private Hand publishes its
    // dispatch envelope and leaves model schemas to the managed harness.
    #[allow(dead_code)]
    pub(super) fn definition() -> Value {
        ClaudeBash::<RetainedBash>::definitions().remove(0)
    }
    pub(super) async fn execute(
        &self,
        mut input: Value,
        session: String,
    ) -> std::result::Result<ToolOutput, String> {
        let disabled = std::env::var("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS").as_deref() == Ok("1");
        let background = input
            .get("run_in_background")
            .map_or(Some(false), Value::as_bool)
            .ok_or("invalid run_in_background")?;
        if background && disabled {
            return Err("background Bash tasks are disabled by host configuration".into());
        }
        let starts_sleep = input["command"]
            .as_str()
            .unwrap_or("")
            .trim_start()
            .split(|c: char| c.is_whitespace() || c == ';')
            .next()
            == Some("sleep");
        let promote = !background && !disabled && !starts_sleep;
        let limits = if background || promote {
            Some(BackgroundLimits::read()?)
        } else {
            None
        };
        let background_ms = if background {
            let limits = limits.as_ref().expect("background limits");
            let timeout = input
                .get("timeout")
                .map_or(Some(limits.default_ms), Value::as_u64)
                .ok_or("invalid background timeout")?;
            if !(1..=limits.maximum_ms).contains(&timeout) {
                return Err(format!(
                    "background timeout must be 1..{} milliseconds",
                    limits.maximum_ms
                ));
            }
            timeout
        } else {
            limits
                .as_ref()
                .map_or(BACKGROUND_DEFAULT_MS, |limits| limits.default_ms)
        };
        // Validate synchronously before admitting a background task. This uses
        // the same adapter parser with an executor that performs no effects.
        struct Validate;
        impl SandboxBashExecutor for Validate {
            async fn execute(&self, _: BashRequest) -> std::result::Result<BashResult, String> {
                Ok(BashResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 0,
                    truncated: false,
                })
            }
        }
        if let Some(object) = input.as_object_mut() {
            object.insert("run_in_background".into(), json!(false));
            if background {
                // Native host owns the background deadline; the portable adapter
                // still validates every other field with its foreground contract.
                object.insert(
                    "timeout".into(),
                    json!(background_ms.min(FOREGROUND_MAX_MS)),
                );
            }
        }
        ClaudeBash::new(Validate)
            .execute("Bash", input.clone())
            .await?;
        // Serialize foreground turns so their observed cwd is applied in order.
        // A background job snapshots cwd but never changes the next command's cwd.
        let (workspace, workspace_lease) = (self.workspace)();
        let mut cwd = self.cwd.lock().await;
        if cwd.0 != workspace {
            *cwd = (workspace.clone(), workspace.clone());
        }
        let start = cwd
            .1
            .canonicalize()
            .ok()
            .filter(|p| p.starts_with(&workspace) && p.is_dir())
            .unwrap_or_else(|| workspace.clone());
        let runtime = Arc::new(WorkspaceToolRuntime::new(start.clone()));
        let retained = RetainedBash {
            runtime: runtime.clone(),
            gate: Arc::new(Mutex::new(())),
        };
        if !background {
            let receipt = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
            let foreground_ms = input
                .get("timeout")
                .and_then(Value::as_u64)
                .unwrap_or(FOREGROUND_DEFAULT_MS);
            let shell = ClaudeBash::new(CwdBash {
                retained,
                receipt: receipt.path().into(),
                execution_timeout: promote.then_some(foreground_ms + background_ms),
                background_timeout: promote.then_some(background_ms),
            });
            let mut execution = Box::pin(async move { shell.execute("Bash", input).await });
            let output = if promote {
                tokio::select! {
                    output = &mut execution => Some(output),
                    _ = tokio::time::sleep(Duration::from_millis(foreground_ms)) => None,
                }
            } else {
                Some((&mut execution).await)
            };
            if output.is_none() {
                let mut jobs = self.jobs.lock().await;
                if jobs.len() >= 256 {
                    return Err("Bash task limit reached; foreground command cancelled".into());
                }
                let id = format!("bash-{}", uuid::Uuid::new_v4());
                let (sender, result) = watch::channel(None);
                let notify = self.completion.clone();
                let task = id.clone();
                let worker = tokio::spawn(async move {
                    let _workspace_lease = workspace_lease;
                    let _receipt = receipt;
                    let output = execution.await;
                    sender.send_replace(Some(output.clone()));
                    notify_completed(notify, session, task, &output);
                });
                jobs.insert(
                    id.clone(),
                    Job {
                        runtime,
                        worker: Some(worker),
                        result,
                        stopped: false,
                    },
                );
                return Ok(text_reply(json!({"task_id":id,"status":"running","auto_backgrounded":true,
                    "foreground_timeout_ms":foreground_ms,"background_timeout_ms":background_ms,
                    "session_cwd":start,"message":"Foreground timeout reached; command moved to the background. Directory changes made by this command do not apply to subsequent commands."}).to_string()));
            }
            let output = output.expect("foreground completion");
            // EXIT traps observe the shell's real final directory, including
            // compound commands and early `exit`. Missing/invalid receipts reset.
            let observed = receipt
                .as_file()
                .metadata()
                .ok()
                .filter(|m| m.len() <= 4096)
                .and_then(|_| std::fs::read_to_string(receipt.path()).ok())
                .and_then(|p| {
                    Path::new(p.strip_suffix('\n').unwrap_or(&p))
                        .canonicalize()
                        .ok()
                })
                .filter(|p| p.starts_with(&workspace) && p.is_dir());
            cwd.1 = observed.unwrap_or_else(|| workspace.clone());
            return output.map(text_reply);
        }
        drop(cwd);
        let shell = ClaudeBash::new(BackgroundBash {
            retained,
            timeout_ms: background_ms,
        });
        let mut jobs = self.jobs.lock().await;
        if jobs.len() >= 256 {
            return Err("Bash task limit reached (256 per session)".into());
        }
        let id = format!("bash-{}", uuid::Uuid::new_v4());
        let (sender, result) = watch::channel(None);
        let notify = self.completion.clone();
        let task = id.clone();
        let worker = tokio::spawn(async move {
            let _workspace_lease = workspace_lease;
            let output = shell.execute("Bash", input).await;
            sender.send_replace(Some(output.clone()));
            notify_completed(notify, session, task, &output);
        });
        jobs.insert(
            id.clone(),
            Job {
                runtime,
                worker: Some(worker),
                result,
                stopped: false,
            },
        );
        Ok(text_reply(
            json!({"task_id":id,"status":"running","background_timeout_ms":background_ms})
                .to_string(),
        ))
    }
    pub(super) async fn output(
        &self,
        id: &str,
        block: bool,
        timeout: u64,
    ) -> std::result::Result<ToolOutput, String> {
        let (mut result, stopped) = {
            let jobs = self.jobs.lock().await;
            let job = jobs.get(id).ok_or("unknown Bash task_id in this session")?;
            (job.result.clone(), job.stopped)
        };
        if block && !stopped && result.borrow().is_none() && timeout > 0 {
            let _ = tokio::time::timeout(Duration::from_millis(timeout), result.changed()).await;
        }
        let outcome = result.borrow().clone();
        // A stop may have arrived while this poll was waiting.
        let stopped = self
            .jobs
            .lock()
            .await
            .get(id)
            .is_some_and(|job| job.stopped);
        let reply = match outcome {
            Some(Ok(output)) => {
                json!({"task_id":id,"status":"completed","output":serde_json::from_str::<Value>(&output).unwrap_or(json!(output))})
            }
            Some(Err(error)) => json!({"task_id":id,"status":"failed","error":error}),
            None => json!({"task_id":id,"status":if stopped {"stopped"} else {"running"}}),
        };
        Ok(text_reply(reply.to_string()))
    }
    pub(super) async fn stop(&self, id: &str) -> std::result::Result<ToolOutput, String> {
        let mut jobs = self.jobs.lock().await;
        let job = jobs
            .get_mut(id)
            .ok_or("unknown Bash task_id in this session")?;
        if job.result.borrow().is_some() {
            return Ok(text_reply(
                json!({"task_id":id,"status":"already_finished"}).to_string(),
            ));
        }
        if let Some(worker) = job.worker.take() {
            worker.abort();
            let _ = worker.await;
        }
        job.runtime.control().cancel().await;
        job.stopped = true;
        Ok(text_reply(
            json!({"task_id":id,"status":"stopped"}).to_string(),
        ))
    }
}

// The receipt is private host bookkeeping, kept separate from bounded stdout.
struct CwdBash {
    retained: RetainedBash,
    receipt: PathBuf,
    execution_timeout: Option<u64>,
    background_timeout: Option<u64>,
}
impl SandboxBashExecutor for CwdBash {
    async fn execute(&self, mut request: BashRequest) -> std::result::Result<BashResult, String> {
        if let Some(timeout) = self.execution_timeout {
            request.timeout_ms = timeout;
        }
        fn quote(value: &str) -> String {
            format!("'{}'", value.replace('\'', "'\"'\"'"))
        }
        let trap = format!(
            "command pwd -P > {}",
            quote(&self.receipt.to_string_lossy())
        );
        request.command = format!("trap {} EXIT\n{}", quote(&trap), request.command);
        self.retained
            .execute(request)
            .await
            .map_err(|error| match self.background_timeout {
                Some(timeout) => background_error(error, timeout),
                None => error,
            })
    }
}

struct BackgroundBash {
    retained: RetainedBash,
    timeout_ms: u64,
}
impl SandboxBashExecutor for BackgroundBash {
    async fn execute(&self, mut request: BashRequest) -> std::result::Result<BashResult, String> {
        request.timeout_ms = self.timeout_ms;
        self.retained
            .execute(request)
            .await
            .map_err(|error| background_error(error, self.timeout_ms))
    }
}
fn background_error(error: String, timeout_ms: u64) -> String {
    if error.starts_with("Bash timed out;") {
        format!(
            "Background command was stopped after reaching its background time limit ({timeout_ms} milliseconds); retained process terminated"
        )
    } else {
        error
    }
}

fn notify_completed(
    callback: Option<Completion>,
    session: String,
    task: String,
    output: &Result<String, String>,
) {
    if let Some(callback) = callback {
        callback(session, task, output);
    }
}

struct RetainedBash {
    runtime: Arc<WorkspaceToolRuntime>,
    gate: Arc<tokio::sync::Mutex<()>>,
}

struct CancelShell {
    runtime: Option<Arc<WorkspaceToolRuntime>>,
    gate: Option<tokio::sync::OwnedMutexGuard<()>>,
}
impl Drop for CancelShell {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            let gate = self.gate.take();
            tokio::spawn(async move {
                runtime.control().cancel().await;
                drop(gate);
            });
        }
    }
}

impl SandboxBashExecutor for RetainedBash {
    async fn execute(&self, request: BashRequest) -> std::result::Result<BashResult, String> {
        let gate = Arc::clone(&self.gate).lock_owned().await;
        let mut cleanup = CancelShell {
            runtime: Some(Arc::clone(&self.runtime)),
            gate: Some(gate),
        };
        let deadline = Instant::now() + Duration::from_millis(request.timeout_ms);
        let context = ToolContext::new("claude", "native-bash", "bash", &[], 1024);
        let input = json!({"cmd":request.command,"yield_time_ms":250,"max_output_tokens":1024});
        let mut output = match tokio::time::timeout_at(
            deadline,
            self.runtime.execute_tool(
                "exec_command",
                ToolInput::Function(to_raw_value(&input).map_err(|e| e.to_string())?),
                context,
            ),
        )
        .await
        {
            Ok(output) => output,
            Err(_) => {
                self.runtime.control().cancel().await;
                cleanup.runtime = None;
                return Err("Bash timed out; retained process terminated".to_owned());
            }
        };
        let mut stdout = String::new();
        let mut truncated = false;
        loop {
            if !output.success {
                return Err(output.structured_result().to_string());
            }
            let result = output.structured_result();
            let chunk = result
                .get("output")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let remaining = request.max_stdout_bytes.saturating_sub(stdout.len());
            let mut end = chunk.len().min(remaining);
            while !chunk.is_char_boundary(end) {
                end -= 1;
            }
            stdout.push_str(&chunk[..end]);
            truncated |= end < chunk.len();
            truncated |= result
                .get("original_token_count")
                .and_then(Value::as_u64)
                .is_some_and(|tokens| tokens.saturating_mul(4) > chunk.len() as u64 + 3);
            if let Some(code) = result.get("exit_code").and_then(Value::as_i64) {
                cleanup.runtime = None;
                return Ok(BashResult {
                    stdout,
                    stderr: String::new(),
                    exit_code: i32::try_from(code).map_err(|e| e.to_string())?,
                    truncated,
                });
            }
            let session = result
                .get("session_id")
                .and_then(Value::as_i64)
                .ok_or("Bash host returned neither exit status nor retained process")?;
            let input = json!({"session_id":session,"yield_time_ms":250,"max_output_tokens":1024});
            output = match tokio::time::timeout_at(
                deadline,
                self.runtime.execute_tool(
                    "write_stdin",
                    ToolInput::Function(to_raw_value(&input).map_err(|e| e.to_string())?),
                    context,
                ),
            )
            .await
            {
                Ok(output) => output,
                Err(_) => {
                    self.runtime.control().cancel().await;
                    cleanup.runtime = None;
                    return Err("Bash timed out; retained process terminated".to_owned());
                }
            };
        }
    }
}
