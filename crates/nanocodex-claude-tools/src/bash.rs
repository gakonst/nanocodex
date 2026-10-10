//! Claude-style Bash capability adapter, not an ambient shell implementation.
//!
//! A host must inject an executor whose command execution is confined to its
//! authorized sandbox. In particular the executor must enforce the requested
//! wall-clock deadline (including termination of the command and descendants)
//! and bound output *while capturing it*. Truncating a returned value here is
//! only a second line of defense; it cannot bound an executor's own memory use.
//! No subprocess, current directory, or host shell is created by this module.

use crate::SessionEnvironment;
use serde_json::{Value, json};

/// Maximum command size, measured in UTF-8 bytes.
pub const MAX_COMMAND_BYTES: usize = 1024 * 1024;
/// Maximum description size, measured in UTF-8 bytes.
pub const MAX_DESCRIPTION_BYTES: usize = 1024;
/// Maximum returned bytes for each of stdout and stderr.
pub const MAX_STREAM_BYTES: usize = 30_000;
/// Default deadline passed to the sandbox executor, in milliseconds.
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// Maximum accepted deadline, in milliseconds.
pub const MAX_TIMEOUT_MS: u64 = 600_000;

/// One foreground command, with limits that the host executor must enforce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BashRequest {
    /// UTF-8 shell command, to interpret only inside the authorized sandbox.
    pub command: String,
    /// Optional human-readable intent (never shell input).
    pub description: Option<String>,
    /// Deadline including sandbox command termination and cleanup.
    pub timeout_ms: u64,
    /// Maximum stdout bytes to capture before truncation.
    pub max_stdout_bytes: usize,
    /// Maximum stderr bytes to capture before truncation.
    pub max_stderr_bytes: usize,
    /// Launching session identity. Executors must export it to the command
    /// with [`Self::apply_session`], which overrides caller-supplied values
    /// and clears inherited ones when no session is bound.
    pub session: Option<SessionEnvironment>,
}

impl BashRequest {
    /// Exports this request's session identity (`CODEX_THREAD_ID` and
    /// `NANOCODEX_ROOT_SESSION_ID`) to `command`. Call it after any other
    /// environment configuration so a spoofed value cannot survive.
    #[cfg(not(target_family = "wasm"))]
    pub fn apply_session(&self, command: &mut std::process::Command) {
        SessionEnvironment::apply_or_clear(self.session.as_ref(), command);
    }
}

/// Bounded command result supplied by a sandbox executor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BashResult {
    /// Captured UTF-8 stdout (lossy decoding, if needed, is the host's job).
    pub stdout: String,
    /// Captured UTF-8 stderr (lossy decoding, if needed, is the host's job).
    pub stderr: String,
    /// Process exit status; a nonzero code remains a result, not an adapter error.
    pub exit_code: i32,
    /// Whether either stream was truncated by the executor.
    pub truncated: bool,
}

/// Host-implemented, explicitly sandboxed asynchronous command capability.
///
/// This trait does not grant shell access by itself. Implementors must isolate
/// the command, cap capture before allocating unbounded output, and kill the
/// command (including descendants) on timeout or cancellation, and export the
/// request's session identity with [`BashRequest::apply_session`]. An executor
/// failure, including a timeout, is returned as an error.
pub trait SandboxBashExecutor: Send + Sync {
    /// Execute a foreground command in the host-authorized sandbox.
    fn execute(
        &self,
        request: BashRequest,
    ) -> impl std::future::Future<Output = Result<BashResult, String>> + Send;
}

/// A Claude Bash tool facade backed only by the supplied sandbox capability.
pub struct ClaudeBash<E: SandboxBashExecutor> {
    executor: E,
}

impl<E: SandboxBashExecutor> ClaudeBash<E> {
    /// Install an explicitly host-authorized sandbox executor.
    pub const fn new(executor: E) -> Self {
        Self { executor }
    }

    /// Model-visible Claude Bash tool definition; no Codex schema is exposed.
    #[must_use]
    pub fn definitions() -> Vec<Value> {
        vec![json!({
            "name": "Bash",
            "description": "Run a foreground command in the host-authorized sandbox. Background execution and sandbox bypass are unavailable.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "minLength": 1, "maxLength": MAX_COMMAND_BYTES},
                    "description": {"type": "string", "maxLength": MAX_DESCRIPTION_BYTES},
                    "timeout": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_MS, "default": DEFAULT_TIMEOUT_MS},
                    "run_in_background": {"type": "boolean", "description": "Only false is supported; no background session capability is installed."},
                    "dangerouslyDisableSandbox": {"type": "boolean", "description": "Unsupported: sandbox bypass is never permitted."}
                },
                "required": ["command"],
                "additionalProperties": false
            }
        })]
    }

    /// Validate a Bash invocation and delegate it to the injected sandbox.
    ///
    /// The return value is JSON text containing `stdout`, `stderr`,
    /// `exit_code`, and `truncated`. Escaped JSON is under 64 KiB even for
    /// control-character-heavy output. A command's nonzero exit is reported in
    /// the result instead of being turned into an adapter error.
    pub async fn execute(&self, name: &str, input: Value) -> Result<String, String> {
        self.execute_in_session(name, input, None).await
    }

    /// Like [`Self::execute`], launching the command on behalf of `session`.
    ///
    /// The executor receives the identity in [`BashRequest::session`] and
    /// exports it as `CODEX_THREAD_ID` and `NANOCODEX_ROOT_SESSION_ID`.
    pub async fn execute_in_session(
        &self,
        name: &str,
        input: Value,
        session: Option<SessionEnvironment>,
    ) -> Result<String, String> {
        if name != "Bash" {
            return Err(format!("unknown Claude Bash tool: {name}"));
        }
        let fields = input.as_object().ok_or("Bash input must be an object")?;
        for key in fields.keys() {
            if !matches!(
                key.as_str(),
                "command"
                    | "description"
                    | "timeout"
                    | "run_in_background"
                    | "dangerouslyDisableSandbox"
            ) {
                return Err(format!("unsupported Bash option: {key}"));
            }
        }
        // Presence is rejected, even when false: callers cannot negotiate a bypass.
        if fields.contains_key("dangerouslyDisableSandbox") {
            return Err("dangerouslyDisableSandbox is never supported".into());
        }
        let background = fields
            .get("run_in_background")
            .map_or(Some(false), Value::as_bool)
            .ok_or("invalid run_in_background")?;
        if background {
            return Err("run_in_background requires an unavailable session capability".into());
        }
        let command = fields
            .get("command")
            .and_then(Value::as_str)
            .ok_or("missing or invalid command")?;
        if command.trim().is_empty() || command.len() > MAX_COMMAND_BYTES {
            return Err(format!(
                "command must be nonblank and at most {MAX_COMMAND_BYTES} bytes"
            ));
        }
        let description = match fields.get("description") {
            None => None,
            Some(value) => {
                let text = value.as_str().ok_or("invalid description")?;
                if text.len() > MAX_DESCRIPTION_BYTES {
                    return Err(format!("description exceeds {MAX_DESCRIPTION_BYTES} bytes"));
                }
                Some(text.to_owned())
            }
        };
        let timeout_ms = fields
            .get("timeout")
            .map_or(Some(DEFAULT_TIMEOUT_MS), Value::as_u64)
            .ok_or("invalid timeout")?;
        if !(1..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
            return Err(format!(
                "timeout must be 1 to {MAX_TIMEOUT_MS} milliseconds"
            ));
        }
        let request = BashRequest {
            command: command.to_owned(),
            description,
            timeout_ms,
            max_stdout_bytes: MAX_STREAM_BYTES,
            max_stderr_bytes: MAX_STREAM_BYTES,
            session,
        };
        let result = self.executor.execute(request).await.map_err(|e| {
            let (bounded, _) = cap_utf8(&e, MAX_STREAM_BYTES);
            format!("sandbox executor: {bounded}")
        })?;
        let (stdout, stdout_cut) = cap_utf8(&result.stdout, MAX_STREAM_BYTES);
        let (stderr, stderr_cut) = cap_utf8(&result.stderr, MAX_STREAM_BYTES);
        Ok(json!({
            "stdout": stdout,
            "stderr": stderr,
            "exit_code": result.exit_code,
            "interrupted": false,
            "truncated": result.truncated || stdout_cut || stderr_cut
        })
        .to_string())
    }
}

fn cap_utf8(value: &str, max: usize) -> (&str, bool) {
    if value.len() <= max {
        return (value, false);
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (&value[..end], true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeExecutor {
        requests: Mutex<Vec<BashRequest>>,
        result: Mutex<Option<Result<BashResult, String>>>,
    }

    impl SandboxBashExecutor for FakeExecutor {
        async fn execute(&self, request: BashRequest) -> Result<BashResult, String> {
            self.requests.lock().unwrap().push(request);
            self.result.lock().unwrap().clone().unwrap_or_else(|| {
                Ok(BashResult {
                    stdout: "ok".into(),
                    stderr: String::new(),
                    exit_code: 0,
                    truncated: false,
                })
            })
        }
    }

    #[tokio::test]
    async fn schema_and_forwarded_request() {
        let schema = ClaudeBash::<FakeExecutor>::definitions();
        assert_eq!(schema.len(), 1);
        assert_eq!(schema[0]["name"], "Bash");
        assert_eq!(schema[0]["input_schema"]["required"], json!(["command"]));
        assert_eq!(
            schema[0]["input_schema"]["properties"]["timeout"]["maximum"],
            600000
        );
        let bash = ClaudeBash::new(FakeExecutor::default());
        let output = bash
            .execute("Bash", json!({"command":"printf hi", "description":"test", "timeout":5000, "run_in_background":false}))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap()["stdout"],
            "ok"
        );
        assert_eq!(
            bash.executor.requests.lock().unwrap()[0],
            BashRequest {
                command: "printf hi".into(),
                description: Some("test".into()),
                timeout_ms: 5000,
                max_stdout_bytes: MAX_STREAM_BYTES,
                max_stderr_bytes: MAX_STREAM_BYTES,
                session: None,
            }
        );
    }

    #[tokio::test]
    async fn rejects_invalid_arguments_without_invoking_executor() {
        let bash = ClaudeBash::new(FakeExecutor::default());
        for (name, input) in [
            ("Other", json!({"command":"pwd"})),
            ("Bash", json!(null)),
            ("Bash", json!({})),
            ("Bash", json!({"command":" \t "})),
            ("Bash", json!({"command":0})),
            ("Bash", json!({"command":"x".repeat(MAX_COMMAND_BYTES + 1)})),
            ("Bash", json!({"command":"x", "description":false})),
            (
                "Bash",
                json!({"command":"x", "description":"x".repeat(MAX_DESCRIPTION_BYTES + 1)}),
            ),
            ("Bash", json!({"command":"x", "timeout":0})),
            ("Bash", json!({"command":"x", "timeout":600001})),
            ("Bash", json!({"command":"x", "timeout":1.5})),
            ("Bash", json!({"command":"x", "run_in_background":true})),
            ("Bash", json!({"command":"x", "run_in_background":"false"})),
            (
                "Bash",
                json!({"command":"x", "dangerouslyDisableSandbox":true}),
            ),
            (
                "Bash",
                json!({"command":"x", "dangerouslyDisableSandbox":false}),
            ),
            ("Bash", json!({"command":"x", "bogus":1})),
        ] {
            assert!(bash.execute(name, input).await.is_err());
        }
        assert!(bash.executor.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reports_exit_error_and_output_caps() {
        let fake = FakeExecutor::default();
        *fake.result.lock().unwrap() = Some(Ok(BashResult {
            stdout: "💡".repeat(MAX_STREAM_BYTES),
            stderr: "\u{0000}".repeat(MAX_STREAM_BYTES + 1),
            exit_code: 37,
            truncated: false,
        }));
        let bash = ClaudeBash::new(fake);
        let output = bash
            .execute("Bash", json!({"command":"false"}))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["exit_code"], 37);
        assert_eq!(parsed["truncated"], true);
        assert!(parsed["stdout"].as_str().unwrap().len() <= MAX_STREAM_BYTES);
        assert_eq!(parsed["stderr"].as_str().unwrap().len(), MAX_STREAM_BYTES);
        assert!(output.len() < 512 * 1024);
        assert_eq!(
            bash.executor.requests.lock().unwrap()[0].timeout_ms,
            DEFAULT_TIMEOUT_MS
        );
        *bash.executor.result.lock().unwrap() = Some(Err("failed".repeat(3000)));
        let error = bash
            .execute("Bash", json!({"command":"pwd"}))
            .await
            .unwrap_err();
        assert!(error.starts_with("sandbox executor: failed"));
        assert!(error.len() < MAX_STREAM_BYTES + 40);
    }
}
