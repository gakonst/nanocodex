mod output;
mod process;
mod selection;

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::{sync::Mutex, task::JoinHandle, time::timeout};

const DEFAULT_EXEC_YIELD_MS: u64 = 10_000;
const DEFAULT_WRITE_YIELD_MS: u64 = 250;
const DEFAULT_POLL_YIELD_MS: u64 = 5_000;
const DRAIN_GRACE: Duration = Duration::from_secs(2);
const MAX_CAPTURE_BYTES: usize = 1024 * 1024;
const MAX_LIVE_SESSIONS: usize = 64;

pub(crate) struct ExecCommand {
    script: String,
    workdir: Option<String>,
    shell: Option<String>,
    login: Option<bool>,
    tty: bool,
    yield_time_ms: Option<i64>,
    max_output_tokens: Option<i64>,
}

impl ExecCommand {
    pub(crate) const fn new(
        script: String,
        workdir: Option<String>,
        shell: Option<String>,
        login: Option<bool>,
        tty: bool,
        yield_time_ms: Option<i64>,
        max_output_tokens: Option<i64>,
    ) -> Self {
        Self {
            script,
            workdir,
            shell,
            login,
            tty,
            yield_time_ms,
            max_output_tokens,
        }
    }
}

pub(crate) struct WriteStdin {
    session_id: i64,
    chars: String,
    yield_time_ms: Option<i64>,
    max_output_tokens: Option<i64>,
}

impl WriteStdin {
    pub(crate) const fn new(
        session_id: i64,
        chars: String,
        yield_time_ms: Option<i64>,
        max_output_tokens: Option<i64>,
    ) -> Self {
        Self {
            session_id,
            chars,
            yield_time_ms,
            max_output_tokens,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct ExecCommandResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    chunk_id: Option<String>,
    wall_time_seconds: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_token_count: Option<usize>,
    output: String,
}

pub(crate) struct ShellSessions {
    sessions: Mutex<SessionStore>,
    next_session_id: AtomicI64,
    default_shell: selection::Shell,
}

impl ShellSessions {
    pub(crate) fn new() -> Self {
        Self {
            sessions: Mutex::new(SessionStore::default()),
            next_session_id: AtomicI64::new(1),
            default_shell: selection::default_user_shell(),
        }
    }

    pub(crate) const fn default_shell_name(&self) -> &'static str {
        self.default_shell.name()
    }

    pub(crate) async fn execute(
        &self,
        command: ExecCommand,
        workspace: &Path,
    ) -> ExecCommandResult {
        let started_at = Instant::now();
        let session_id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let workdir = resolve_workdir(workspace, command.workdir.as_deref());
        let shell = command.shell.as_deref().map_or_else(
            || self.default_shell.clone(),
            selection::get_shell_by_model_provided_path,
        );
        let (environment, secrets) = process::sanitized_environment();
        let spawned = match process::spawn(
            &command.script,
            &workdir,
            &shell,
            command.login.unwrap_or(true),
            command.tty,
            &environment,
        ) {
            Ok(spawned) => spawned,
            Err(error) => {
                return ExecCommandResult::failed(
                    started_at.elapsed(),
                    format!("failed to spawn {}: {error}", shell.path().display()),
                );
            }
        };
        let session = Session::new(session_id, spawned, secrets);
        let pruned = self.sessions.lock().await.insert(Arc::clone(&session));
        if let Some(pruned) = pruned {
            pruned.terminate().await;
        }

        let yield_time = duration_ms(command.yield_time_ms, DEFAULT_EXEC_YIELD_MS, 250, 30_000);
        let _interaction = session.begin_interaction();
        let result = session
            .wait_for_output(yield_time, command.max_output_tokens, started_at)
            .await;
        if result.exit_code.is_some() {
            self.sessions.lock().await.remove(session_id);
        }
        result
    }

    pub(crate) async fn write_stdin(&self, request: WriteStdin) -> ExecCommandResult {
        let started_at = Instant::now();
        let session = self.sessions.lock().await.get(request.session_id);
        let Some(session) = session else {
            return ExecCommandResult::failed(
                started_at.elapsed(),
                format!("unknown or completed exec session {}", request.session_id),
            );
        };

        let _interaction = session.begin_interaction();
        if !request.chars.is_empty() {
            if let Err(error) = session.write(&request.chars).await {
                return ExecCommandResult::failed(
                    started_at.elapsed(),
                    format!(
                        "failed to write to exec session {}: {error}",
                        request.session_id
                    ),
                );
            }
        }
        let (default, minimum, maximum) = if request.chars.is_empty() {
            (DEFAULT_POLL_YIELD_MS, 5_000, 300_000)
        } else {
            (DEFAULT_WRITE_YIELD_MS, 250, 30_000)
        };
        let yield_time = duration_ms(request.yield_time_ms, default, minimum, maximum);
        let result = session
            .wait_for_output(yield_time, request.max_output_tokens, started_at)
            .await;
        if result.exit_code.is_some() {
            self.sessions.lock().await.remove(request.session_id);
        }
        result
    }
}

#[derive(Default)]
struct SessionStore {
    sessions: HashMap<i64, Arc<Session>>,
    recency: VecDeque<i64>,
}

impl SessionStore {
    fn insert(&mut self, session: Arc<Session>) -> Option<Arc<Session>> {
        let pruned = (self.sessions.len() >= MAX_LIVE_SESSIONS)
            .then(|| {
                let protected_from = self.recency.len().saturating_sub(8);
                self.recency.iter().take(protected_from).position(|id| {
                    self.sessions
                        .get(id)
                        .is_some_and(|session| !session.is_active())
                })
            })
            .flatten()
            .and_then(|index| {
                let id = self.recency.remove(index)?;
                self.sessions.remove(&id)
            });
        self.recency.push_back(session.id);
        self.sessions.insert(session.id, session);
        pruned
    }

    fn get(&mut self, id: i64) -> Option<Arc<Session>> {
        let session = self.sessions.get(&id).cloned()?;
        self.touch(id);
        Some(session)
    }

    fn remove(&mut self, id: i64) -> Option<Arc<Session>> {
        if let Some(index) = self.recency.iter().position(|candidate| *candidate == id) {
            self.recency.remove(index);
        }
        self.sessions.remove(&id)
    }

    fn touch(&mut self, id: i64) {
        if let Some(index) = self.recency.iter().position(|candidate| *candidate == id) {
            self.recency.remove(index);
        }
        self.recency.push_back(id);
    }
}

struct Session {
    id: i64,
    child: Mutex<process::ProcessChild>,
    stdin: Mutex<Option<process::ProcessStdin>>,
    process_group: Mutex<process::ProcessGroupGuard>,
    drains: Mutex<Option<Vec<JoinHandle<()>>>>,
    captured: Arc<Mutex<CapturedOutput>>,
    secrets: Vec<String>,
    next_chunk_id: AtomicU64,
    active_interactions: AtomicU64,
}

impl Session {
    fn new(id: i64, spawned: process::SpawnedProcess, secrets: Vec<String>) -> Arc<Self> {
        let captured = Arc::new(Mutex::new(CapturedOutput::default()));
        let drains = match spawned.output {
            process::ProcessOutput::Pipes { stdout, stderr } => vec![
                tokio::spawn(output::drain(
                    stdout,
                    Arc::clone(&captured),
                    MAX_CAPTURE_BYTES,
                )),
                tokio::spawn(output::drain(
                    stderr,
                    Arc::clone(&captured),
                    MAX_CAPTURE_BYTES,
                )),
            ],
            process::ProcessOutput::Pty(reader) => vec![output::drain_blocking(
                reader,
                Arc::clone(&captured),
                MAX_CAPTURE_BYTES,
            )],
        };
        Arc::new(Self {
            id,
            child: Mutex::new(spawned.child),
            stdin: Mutex::new(spawned.stdin),
            process_group: Mutex::new(spawned.process_group),
            drains: Mutex::new(Some(drains)),
            captured,
            secrets,
            next_chunk_id: AtomicU64::new(1),
            active_interactions: AtomicU64::new(0),
        })
    }

    fn begin_interaction(&self) -> ActiveInteraction<'_> {
        self.active_interactions.fetch_add(1, Ordering::AcqRel);
        ActiveInteraction { session: self }
    }

    fn is_active(&self) -> bool {
        self.active_interactions.load(Ordering::Acquire) > 0
    }

    async fn terminate(&self) {
        let _ = self.process_group.lock().await.terminate_and_disarm();
    }

    async fn write(&self, chars: &str) -> std::io::Result<()> {
        let mut stdin = self.stdin.lock().await;
        let stdin = stdin.as_mut().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "stdin is closed")
        })?;
        stdin.write(chars.as_bytes()).await
    }

    async fn wait_for_output(
        &self,
        yield_time: Duration,
        max_output_tokens: Option<i64>,
        started_at: Instant,
    ) -> ExecCommandResult {
        let status = {
            let mut child = self.child.lock().await;
            timeout(yield_time, child.wait()).await
        };
        let exit_code = match status {
            Ok(Ok(exit_code)) => {
                self.process_group.lock().await.disarm();
                self.finish_drains().await;
                Some(exit_code)
            }
            Ok(Err(error)) => {
                let _ = self.process_group.lock().await.terminate_and_disarm();
                let message = format!("failed to wait for shell command: {error}");
                self.captured
                    .lock()
                    .await
                    .push(message.as_bytes(), MAX_CAPTURE_BYTES);
                Some(1)
            }
            Err(_) => None,
        };
        let (output, original_token_count) = self.take_output(max_output_tokens).await;
        ExecCommandResult {
            chunk_id: Some(format!(
                "{}-{}",
                self.id,
                self.next_chunk_id.fetch_add(1, Ordering::Relaxed)
            )),
            wall_time_seconds: started_at.elapsed().as_secs_f64(),
            exit_code,
            session_id: exit_code.is_none().then_some(self.id),
            original_token_count,
            output,
        }
    }

    async fn finish_drains(&self) {
        let handles = self.drains.lock().await.take();
        let Some(handles) = handles else {
            return;
        };
        for mut handle in handles {
            if timeout(DRAIN_GRACE, &mut handle).await.is_err() {
                handle.abort();
                let _ = handle.await;
            }
        }
    }

    async fn take_output(&self, max_output_tokens: Option<i64>) -> (String, Option<usize>) {
        let captured = self.captured.lock().await.take();
        let raw = String::from_utf8_lossy(&captured.with_omission_marker()).into_owned();
        let limit = output::effective_token_limit(max_output_tokens);
        let (output, limited) = output::redact_and_limit(raw, &self.secrets, limit);
        let was_truncated = limited || captured.omitted_bytes > 0;
        (
            output,
            was_truncated.then_some(captured.total_bytes.saturating_add(3) / 4),
        )
    }
}

struct ActiveInteraction<'a> {
    session: &'a Session,
}

impl Drop for ActiveInteraction<'_> {
    fn drop(&mut self) {
        self.session
            .active_interactions
            .fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Default)]
pub(super) struct CapturedOutput {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    omitted_bytes: usize,
}

impl CapturedOutput {
    pub(super) fn push(&mut self, bytes: &[u8], limit: usize) {
        if bytes.is_empty() {
            return;
        }
        if limit == 0 {
            self.omitted_bytes = self.omitted_bytes.saturating_add(bytes.len());
            return;
        }

        let head_budget = limit / 2;
        let tail_budget = limit.saturating_sub(head_budget);
        let head_len = head_budget.saturating_sub(self.head.len()).min(bytes.len());
        self.head.extend_from_slice(&bytes[..head_len]);
        let tail_bytes = &bytes[head_len..];
        if tail_bytes.len() >= tail_budget {
            let kept_from = tail_bytes.len().saturating_sub(tail_budget);
            self.omitted_bytes = self
                .omitted_bytes
                .saturating_add(self.tail.len())
                .saturating_add(kept_from);
            self.tail.clear();
            self.tail.extend(&tail_bytes[kept_from..]);
        } else {
            self.tail.extend(tail_bytes);
            let excess = self.tail.len().saturating_sub(tail_budget);
            if excess > 0 {
                drop(self.tail.drain(..excess));
                self.omitted_bytes = self.omitted_bytes.saturating_add(excess);
            }
        }
    }

    fn take(&mut self) -> CapturedChunk {
        let head = std::mem::take(&mut self.head);
        let tail = std::mem::take(&mut self.tail);
        let omitted_bytes = std::mem::take(&mut self.omitted_bytes);
        CapturedChunk {
            total_bytes: head
                .len()
                .saturating_add(tail.len())
                .saturating_add(omitted_bytes),
            head,
            tail,
            omitted_bytes,
        }
    }
}

struct CapturedChunk {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    omitted_bytes: usize,
    total_bytes: usize,
}

impl CapturedChunk {
    fn with_omission_marker(&self) -> Vec<u8> {
        let marker = (self.omitted_bytes > 0)
            .then(|| format!("... {} bytes omitted ...", self.omitted_bytes));
        let marker_length = marker.as_ref().map_or(0, |marker| marker.len() + 2);
        let mut output = Vec::with_capacity(
            self.head
                .len()
                .saturating_add(self.tail.len())
                .saturating_add(marker_length),
        );
        output.extend_from_slice(&self.head);
        if let Some(marker) = marker {
            output.push(b'\n');
            output.extend_from_slice(marker.as_bytes());
            output.push(b'\n');
        }
        output.extend(self.tail.iter().copied());
        output
    }
}

impl ExecCommandResult {
    fn failed(wall_time: Duration, output: String) -> Self {
        Self {
            chunk_id: None,
            wall_time_seconds: wall_time.as_secs_f64(),
            exit_code: Some(1),
            session_id: None,
            original_token_count: None,
            output,
        }
    }
}

fn resolve_workdir(workspace: &Path, requested: Option<&str>) -> PathBuf {
    let requested = requested.filter(|workdir| !workdir.is_empty());
    match requested.map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        Some(path) => workspace.join(path),
        None => workspace.to_owned(),
    }
}

fn duration_ms(requested: Option<i64>, default: u64, minimum: u64, maximum: u64) -> Duration {
    let requested = requested
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(default);
    Duration::from_millis(requested.clamp(minimum, maximum))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{CapturedOutput, ExecCommand, ShellSessions, WriteStdin};

    #[test]
    fn bounded_capture_keeps_head_and_tail_then_accepts_the_next_poll() {
        let mut captured = CapturedOutput::default();
        captured.push(b"abcdefgh", 4);
        let first = captured.take();
        let first = String::from_utf8(first.with_omission_marker()).expect("ASCII output");
        assert!(first.starts_with("ab\n... 4 bytes omitted ...\n"));
        assert!(first.ends_with("gh"));

        captured.push(b"next", 4);
        let second = captured.take();
        assert_eq!(second.with_omission_marker(), b"next");
    }

    #[tokio::test]
    async fn yielded_command_accepts_stdin_and_completes() {
        let sessions = ShellSessions::new();
        let first = sessions
            .execute(
                ExecCommand::new(
                    "read value; printf 'got:%s' \"$value\"".to_owned(),
                    None,
                    None,
                    Some(false),
                    false,
                    Some(250),
                    None,
                ),
                std::path::Path::new("/"),
            )
            .await;
        assert_eq!(first.session_id, Some(1));

        let second = sessions
            .write_stdin(WriteStdin::new(1, "hello\n".to_owned(), Some(1_000), None))
            .await;
        assert_eq!(second.exit_code, Some(0));
        assert_eq!(second.output, "got:hello");
    }

    #[tokio::test]
    async fn successful_command_leaves_background_process_running()
    -> Result<(), Box<dyn std::error::Error>> {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "harness-background-process-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory)?;
        let marker = directory.join("survived");
        let sessions = ShellSessions::new();

        let result = sessions
            .execute(
                ExecCommand::new(
                    format!(
                        "(sleep 1; printf survived > '{}') >/dev/null 2>&1 &",
                        marker.display()
                    ),
                    None,
                    None,
                    Some(false),
                    false,
                    Some(1_000),
                    None,
                ),
                std::path::Path::new("/"),
            )
            .await;

        tokio::time::timeout(Duration::from_secs(3), async {
            while !marker.is_file() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("background process should survive successful shell exit");
        let contents = std::fs::read_to_string(&marker)?;
        std::fs::remove_dir_all(directory)?;

        assert_eq!(result.exit_code, Some(0));
        assert_eq!(contents, "survived");
        Ok(())
    }

    #[tokio::test]
    async fn tty_command_has_a_terminal_and_accepts_stdin() {
        let ready = std::env::temp_dir().join(format!("harness-pty-ready-{}", std::process::id()));
        let _ = std::fs::remove_file(&ready);
        let sessions = ShellSessions::new();
        let first = sessions
            .execute(
                ExecCommand::new(
                    format!(
                        "test -t 0 && test -t 1 && test -t 2; stty -echo; : > '{}'; printf ready; read value; printf 'got:%s' \"$value\"",
                        ready.display()
                    ),
                    None,
                    None,
                    Some(false),
                    true,
                    Some(1_000),
                    None,
                ),
                std::path::Path::new("/"),
            )
            .await;
        assert_eq!(first.session_id, Some(1));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !ready.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("PTY command should disable echo before stdin is written");
        std::fs::remove_file(&ready).expect("PTY readiness marker should be removable");

        let second = sessions
            .write_stdin(WriteStdin::new(1, "hello\n".to_owned(), Some(1_000), None))
            .await;
        assert_eq!(second.exit_code, Some(0));
        assert_eq!(
            format!("{}{}", first.output, second.output),
            "readygot:hello"
        );
    }

    #[tokio::test]
    async fn explicit_shell_runs_bash_syntax() {
        if !std::path::Path::new("/bin/bash").is_file() {
            return;
        }
        let sessions = ShellSessions::new();
        let result = sessions
            .execute(
                ExecCommand::new(
                    "[[ codex == codex ]] && printf bash".to_owned(),
                    None,
                    Some("/bin/bash".to_owned()),
                    Some(false),
                    false,
                    Some(1_000),
                    None,
                ),
                std::path::Path::new("/"),
            )
            .await;

        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.output, "bash");
    }
}
