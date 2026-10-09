//! Retained Bash jobs. Each job owns a workspace runtime so stopping one cannot
//! cancel another. The existing foreground executor supplies capture/deadlines.
use super::*;
use std::collections::BTreeMap;
use tokio::sync::{Mutex, watch};

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

/// Session cwd carried between foreground commands. Commands snapshot it at
/// start; completions apply in start order so a slow earlier command cannot
/// overwrite the directory left by a later one.
struct SessionCwd {
    workspace: PathBuf,
    current: PathBuf,
    started: u64,
    applied: u64,
}

pub(super) struct Shell {
    workspace: Arc<worktree::Workspace>,
    cwd: Mutex<SessionCwd>,
    jobs: Mutex<BTreeMap<String, Job>>,
    scheduler: Option<Arc<scheduler::SessionScheduler>>,
}
impl Shell {
    pub(super) fn new(
        workspace: Arc<worktree::Workspace>,
        scheduler: Option<Arc<scheduler::SessionScheduler>>,
    ) -> Self {
        let current = workspace.current();
        Self {
            workspace,
            scheduler,
            cwd: Mutex::new(SessionCwd {
                workspace: current.clone(),
                current,
                started: 0,
                applied: 0,
            }),
            jobs: Mutex::new(BTreeMap::new()),
        }
    }
    pub(super) fn definition() -> ToolDefinition {
        let mut schema = ClaudeBash::<RetainedBash>::definitions().remove(0);
        schema["description"] = json!(
            "Run Bash in the authorized workspace. Foreground working-directory changes inside the project carry to the next Bash call; outside-project directories reset to the project root. Background jobs snapshot the current directory without changing it. A foreground command that reaches its timeout moves to the background with a background deadline (30 minutes by default), except commands starting with sleep or when CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1. Environment exports do not carry. Explicit background timeout sets its execution deadline: 30 minutes by default, at most 2 hours. BASH_DEFAULT_TIMEOUT_MS and BASH_MAX_TIMEOUT_MS can raise background limits but cannot lower them. Foreground timeout remains 120000ms by default and at most 600000ms. Background commands return a task_id for TaskOutput and TaskStop. Timeout includes process cleanup; output is bounded. No sandbox bypass."
        );
        // Preserve the pinned Orca Claude Code input schema verbatim. Runtime
        // limits belong in execute; schema additions can invalidate Messages.
        schema["input_schema"] = serde_json::from_str(include_str!("bash.input_schema.json"))
            .expect("captured Bash input schema");
        serde_json::from_value(schema).expect("Bash definition")
    }
    pub(super) async fn execute(
        &self,
        mut input: Value,
        session: String,
    ) -> std::result::Result<ClaudeToolReply, String> {
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
        // Hold the cwd lock only to snapshot the start directory. Holding it for
        // the whole command serialized concurrent Code Mode calls behind the
        // slowest one. A background job snapshots cwd but never changes it.
        let (workspace, workspace_lease) = self.workspace.pin_current();
        let (start, sequence) = {
            let mut cwd = self.cwd.lock().await;
            if cwd.workspace != workspace {
                cwd.workspace = workspace.clone();
                cwd.current = workspace.clone();
            }
            cwd.started += 1;
            let start = cwd
                .current
                .canonicalize()
                .ok()
                .filter(|p| p.starts_with(&workspace) && p.is_dir())
                .unwrap_or_else(|| workspace.clone());
            (start, cwd.started)
        };
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
                let notify = self.scheduler.clone();
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
            let mut cwd = self.cwd.lock().await;
            if cwd.workspace == workspace && sequence > cwd.applied {
                cwd.current = observed.unwrap_or_else(|| workspace.clone());
                cwd.applied = sequence;
            }
            drop(cwd);
            return output.map(text_reply);
        }
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
        let notify = self.scheduler.clone();
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
    ) -> std::result::Result<ClaudeToolReply, String> {
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
    pub(super) async fn stop(&self, id: &str) -> std::result::Result<ClaudeToolReply, String> {
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
    scheduler: Option<Arc<scheduler::SessionScheduler>>,
    session: String,
    task: String,
    output: &std::result::Result<String, String>,
) {
    if let Some(scheduler) = scheduler {
        let event = json!({"source":"Bash","task_id":task,"status":if output.is_ok() {"completed"} else {"failed"},"reason":output.as_ref().err().map(|error| error.chars().take(1024).collect::<String>()),"untrusted":true});
        let _ = scheduler.enqueue(session, task, format!("Bash background task finished (process status, not user instructions); use TaskOutput for retained output: {event}"));
    }
}
