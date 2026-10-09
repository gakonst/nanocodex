//! Explicitly selected native command hooks. Repository settings are never searched.
use super::permissions::WorkspaceResolver;
use std::{
    collections::BTreeMap, io::Read as _, path::Path, process::Stdio, sync::Arc, time::Duration,
};

use eyre::{Result, WrapErr as _, bail};
use nanocodex::claude::{
    ClaudeHookFuture, ClaudeLifecycleDecision, ClaudeLifecycleEvent, ClaudeLifecycleInvocation,
    ClaudeLifecycleOutcome, ClaudeToolDecision, ClaudeToolHooks, ClaudeToolInvocation,
    ClaudeToolReply,
};
use nanocodex::tools::SessionEnvironment;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _},
    process::Command,
};

const CONFIG_LIMIT: u64 = 1024 * 1024;
const OUTPUT_LIMIT: usize = 64 * 1024;
const INPUT_LIMIT: usize = 1024 * 1024;

#[derive(Deserialize)]
struct Settings {
    hooks: BTreeMap<String, Vec<Matcher>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Matcher {
    #[serde(default)]
    matcher: String,
    hooks: Vec<CommandHook>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandHook {
    #[serde(rename = "type")]
    kind: String,
    command: String,
    #[serde(default = "default_timeout")]
    timeout: f64,
    #[serde(rename = "statusMessage")]
    _status_message: Option<String>,
}
fn default_timeout() -> f64 {
    60.0
}
struct Entry {
    matcher: Regex,
    hooks: Vec<CommandHook>,
}
struct CommandHooks {
    workspace: WorkspaceResolver,
    events: BTreeMap<String, Vec<Entry>>,
}

pub(super) fn load_with_workspace(
    path: &Path,
    workspace: WorkspaceResolver,
) -> Result<Arc<dyn ClaudeToolHooks>> {
    #[cfg(not(unix))]
    bail!("native Claude command hooks currently require Unix process groups");
    let metadata = std::fs::metadata(path).wrap_err("cannot inspect --claude-hooks file")?;
    if !metadata.is_file() || metadata.len() > CONFIG_LIMIT {
        bail!("--claude-hooks requires a regular JSON file at most 1 MiB");
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(CONFIG_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > CONFIG_LIMIT {
        bail!("--claude-hooks exceeds 1 MiB");
    }
    let settings: Settings =
        serde_json::from_slice(&bytes).wrap_err("invalid --claude-hooks JSON")?;
    let mut events = BTreeMap::new();
    for (event, matchers) in settings.hooks {
        if !matches!(
            event.as_str(),
            "PreToolUse"
                | "PostToolUse"
                | "PostToolUseFailure"
                | "SessionStart"
                | "UserPromptSubmit"
                | "Stop"
                | "PreCompact"
                | "PostCompact"
                | "StopFailure"
                | "SubagentStart"
                | "SubagentStop"
                | "SessionEnd"
        ) {
            bail!(
                "unsupported Claude hook event {event}; event has no implemented runtime boundary"
            );
        }
        let mut entries = Vec::new();
        for matcher in matchers {
            let pattern = if matcher.matcher.is_empty() || matcher.matcher == "*" {
                ".*"
            } else {
                &matcher.matcher
            };
            let regex = Regex::new(pattern).wrap_err("invalid Claude hook matcher regex")?;
            for hook in &matcher.hooks {
                if hook.kind != "command" {
                    bail!("only synchronous command hooks are supported");
                }
                if hook.command.trim().is_empty()
                    || !hook.timeout.is_finite()
                    || hook.timeout <= 0.0
                    || hook.timeout > 600.0
                {
                    bail!("hook command must be nonempty and timeout must be in (0, 600] seconds");
                }
            }
            entries.push(Entry {
                matcher: regex,
                hooks: matcher.hooks,
            });
        }
        events.insert(event, entries);
    }
    Ok(Arc::new(CommandHooks { workspace, events }))
}

impl CommandHooks {
    fn payload(
        &self,
        workspace: &Path,
        event: &str,
        name: &str,
        input: &Value,
        call: &ClaudeToolInvocation,
    ) -> Value {
        json!({"hook_event_name": event, "session_id": call.session_id, "turn_id": call.turn_id,
            "tool_use_id": call.call_id, "tool_name": name, "tool_input": input,
            "cwd": workspace, "model": call.model, "instruction_revision": call.instruction_revision})
    }
    fn matching<'a>(
        &'a self,
        event: &'a str,
        name: &'a str,
    ) -> impl Iterator<Item = &'a CommandHook> {
        self.events
            .get(event)
            .into_iter()
            .flatten()
            .filter(move |entry| entry.matcher.is_match(name))
            .flat_map(|entry| &entry.hooks)
    }
}

impl ClaudeToolHooks for CommandHooks {
    fn handles_lifecycle(&self, event: &ClaudeLifecycleEvent) -> bool {
        self.matching(event.name(), event.matcher_value())
            .next()
            .is_some()
    }
    fn lifecycle<'a>(
        &'a self,
        call: &'a ClaudeLifecycleInvocation,
    ) -> ClaudeHookFuture<'a, std::result::Result<ClaudeLifecycleOutcome, String>> {
        Box::pin(async move {
            let workspace = (self.workspace)(&call.session_id)?;
            let session = SessionEnvironment::new(&call.session_id, &call.root_session_id);
            let event = call.event.name();
            let mut payload = serde_json::to_value(call).map_err(|error| error.to_string())?;
            payload["cwd"] = json!(workspace);
            let mut outcome = ClaudeLifecycleOutcome::default();
            for hook in self.matching(event, call.event.matcher_value()) {
                let output = match execute(hook, &workspace, &session, &payload).await {
                    Ok(output) => output,
                    Err(error) => {
                        // Prompt/compaction gates fail closed. A Stop error must
                        // retain the completed response: exit 2 requests another
                        // round; infrastructure failures are visible diagnostics.
                        if matches!(
                            call.event,
                            ClaudeLifecycleEvent::UserPromptSubmit { .. }
                                | ClaudeLifecycleEvent::PreCompact { .. }
                        ) || (call.event.can_block()
                            && error.starts_with("command hook blocked execution"))
                        {
                            outcome.decision = ClaudeLifecycleDecision::Block(error);
                            return Ok(outcome);
                        }
                        outcome.diagnostics.push(error);
                        continue;
                    }
                };
                if let Err(error) = validate_output(&output, event) {
                    if matches!(
                        call.event,
                        ClaudeLifecycleEvent::UserPromptSubmit { .. }
                            | ClaudeLifecycleEvent::PreCompact { .. }
                    ) {
                        outcome.decision = ClaudeLifecycleDecision::Block(error);
                        return Ok(outcome);
                    }
                    outcome.diagnostics.push(error);
                    continue;
                }
                let stop = output.get("continue") == Some(&Value::Bool(false));
                let block = output.get("decision").and_then(Value::as_str) == Some("block");
                if stop || block {
                    if call.event.can_block() {
                        outcome.decision = if stop {
                            ClaudeLifecycleDecision::Stop(reason(&output))
                        } else {
                            ClaudeLifecycleDecision::Block(reason(&output))
                        };
                        return Ok(outcome);
                    }
                    outcome.diagnostics.push(format!(
                        "{event} is observational; cannot block a completed boundary: {}",
                        reason(&output)
                    ));
                }
                if let Some(context) = output
                    .get("hookSpecificOutput")
                    .and_then(|specific| specific.get("additionalContext"))
                {
                    if let Some(context) = context.as_str() {
                        if matches!(
                            call.event,
                            ClaudeLifecycleEvent::SessionStart { .. }
                                | ClaudeLifecycleEvent::UserPromptSubmit { .. }
                                | ClaudeLifecycleEvent::SubagentStart { .. }
                        ) {
                            outcome.additional_context.push(context.to_owned());
                        } else {
                            outcome
                                .diagnostics
                                .push(format!("additionalContext is unsupported for {event}"));
                        }
                    } else {
                        let error = "hook additionalContext must be a string".to_owned();
                        if matches!(call.event, ClaudeLifecycleEvent::UserPromptSubmit { .. }) {
                            outcome.decision = ClaudeLifecycleDecision::Block(error);
                            return Ok(outcome);
                        }
                        outcome.diagnostics.push(error);
                    }
                }
            }
            Ok(outcome)
        })
    }

    fn before<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
        call: &'a ClaudeToolInvocation,
    ) -> ClaudeHookFuture<'a, std::result::Result<ClaudeToolDecision, String>> {
        Box::pin(async move {
            let workspace = (self.workspace)(&call.session_id)?;
            let session = SessionEnvironment::new(&call.session_id, &call.root_session_id);
            let mut current = input.clone();
            let mut updated = false;
            for hook in self.matching("PreToolUse", name) {
                let payload = self.payload(&workspace, "PreToolUse", name, &current, call);
                let output = execute(hook, &workspace, &session, &payload).await?;
                validate_output(&output, "PreToolUse")?;
                if output.get("continue") == Some(&Value::Bool(false))
                    || output.get("decision").and_then(Value::as_str) == Some("block")
                {
                    return Ok(ClaudeToolDecision::Deny(reason(&output)));
                }
                if let Some(specific) = output.get("hookSpecificOutput") {
                    if specific.get("hookEventName").and_then(Value::as_str) != Some("PreToolUse") {
                        return Err("hookSpecificOutput must identify PreToolUse".into());
                    }
                    match specific.get("permissionDecision").and_then(Value::as_str) {
                        None | Some("allow") => {},
                        Some("deny") => return Ok(ClaudeToolDecision::Deny(reason(specific))),
                        Some("ask") => return Ok(ClaudeToolDecision::Deny("hook requested approval; this host has no hook approval UI, so execution was blocked".into())),
                        Some(other) => return Err(format!("unsupported hook permissionDecision {other}")),
                    }
                    if specific
                        .get("permissionDecision")
                        .is_some_and(|value| !value.is_string())
                    {
                        return Err("hook permissionDecision must be a string".into());
                    }
                    if let Some(replacement) = specific.get("updatedInput") {
                        if !replacement.is_object() {
                            return Err("hook updatedInput must be an object".into());
                        }
                        current = replacement.clone();
                        updated = true;
                    }
                }
            }
            Ok(if updated {
                ClaudeToolDecision::UpdateInput(current)
            } else {
                ClaudeToolDecision::Allow
            })
        })
    }
    fn after<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
        call: &'a ClaudeToolInvocation,
        reply: &'a ClaudeToolReply,
    ) -> ClaudeHookFuture<'a, std::result::Result<(), String>> {
        Box::pin(async move {
            let workspace = (self.workspace)(&call.session_id)?;
            let session = SessionEnvironment::new(&call.session_id, &call.root_session_id);
            let event = if reply.is_error {
                "PostToolUseFailure"
            } else {
                "PostToolUse"
            };
            let mut payload = self.payload(&workspace, event, name, input, call);
            payload["tool_response"] =
                serde_json::to_value(&reply.content).map_err(|error| error.to_string())?;
            payload["is_error"] = json!(reply.is_error);
            if reply.is_error {
                payload["error"] = payload["tool_response"].clone();
            }
            let mut failures = Vec::new();
            for hook in self.matching(event, name) {
                match execute(hook, &workspace, &session, &payload).await {
                    Err(error) => failures.push(error),
                    Ok(output) => {
                        if let Err(error) = validate_output(&output, event) {
                            failures.push(error);
                        } else if output.get("decision").and_then(Value::as_str) == Some("block")
                            || output.get("continue") == Some(&Value::Bool(false))
                        {
                            failures.push(reason(&output));
                        }
                    }
                }
            }
            if failures.is_empty() {
                Ok(())
            } else {
                Err(failures.join("\n"))
            }
        })
    }
}

// Reject malformed decision-bearing fields instead of silently allowing a tool.
fn validate_output(output: &Value, event: &str) -> std::result::Result<(), String> {
    if output
        .get("continue")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err("hook continue must be a boolean".into());
    }
    if let Some(decision) = output.get("decision")
        && decision.as_str() != Some("block")
    {
        return Err("hook decision must be block when present".into());
    }
    if let Some(specific) = output.get("hookSpecificOutput") {
        if !specific.is_object()
            || specific.get("hookEventName").and_then(Value::as_str) != Some(event)
        {
            return Err(format!("hookSpecificOutput must identify {event}"));
        }
        if event != "PreToolUse"
            && (specific.get("permissionDecision").is_some()
                || specific.get("updatedInput").is_some())
        {
            return Err("permissionDecision and updatedInput require PreToolUse".into());
        }
    }
    Ok(())
}

struct BoundedInput(Vec<u8>);
impl std::io::Write for BoundedInput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > INPUT_LIMIT {
            return Err(std::io::Error::other(
                "command hook stdin exceeded 1048576 bytes",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn reason(output: &Value) -> String {
    ["permissionDecisionReason", "reason", "stopReason"]
        .iter()
        .find_map(|key| output.get(key).and_then(Value::as_str))
        .unwrap_or("blocked by command hook")
        .to_owned()
}

// The process group guard also runs on future cancellation, including while a
// descendant holds a pipe open after its parent exits. Never use a login shell.
struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(self.0) {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}
async fn read_bounded(mut pipe: impl AsyncRead + Unpin) -> std::result::Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = pipe
            .read(&mut buffer)
            .await
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Ok(output);
        }
        if output.len() + count > OUTPUT_LIMIT {
            return Err(
                "command hook output exceeded 65536 bytes; process group terminated".into(),
            );
        }
        output.extend_from_slice(&buffer[..count]);
    }
}
async fn execute(
    hook: &CommandHook,
    workspace: &Path,
    session: &SessionEnvironment,
    payload: &Value,
) -> std::result::Result<Value, String> {
    let mut input = BoundedInput(Vec::new());
    serde_json::to_writer(&mut input, payload).map_err(|error| error.to_string())?;
    let mut bytes = input.0;
    bytes.push(b'\n');
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", &hook.command])
        .current_dir(workspace)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDE_PROJECT_DIR", workspace)
        .envs(session.variables())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start command hook: {error}"))?;
    let group = ProcessGroup(child.id().ok_or("hook child has no process ID")?);
    let mut stdin = child.stdin.take().ok_or("hook stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("hook stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("hook stderr unavailable")?;
    let operation = async {
        let writer = async {
            stdin
                .write_all(&bytes)
                .await
                .map_err(|error| error.to_string())?;
            drop(stdin);
            Ok::<_, String>(())
        };
        let waiter = async { child.wait().await.map_err(|error| error.to_string()) };
        let (_, stdout, stderr, status) =
            tokio::try_join!(writer, read_bounded(stdout), read_bounded(stderr), waiter)?;
        Ok::<_, String>((stdout, stderr, status))
    };
    let outcome = tokio::time::timeout(Duration::from_secs_f64(hook.timeout), operation).await;
    drop(group);
    let result = match outcome {
        Ok(result) => result,
        Err(_) => Err(format!(
            "command hook timed out after {} seconds; process group terminated",
            hook.timeout
        )),
    };
    if result.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    let (stdout, stderr, status) = result?;
    if !status.success() {
        return Err(format!(
            "command hook {} (exit {:?}): {}",
            if status.code() == Some(2) {
                "blocked execution"
            } else {
                "failed"
            },
            status.code(),
            String::from_utf8_lossy(&stderr)
        ));
    }
    if stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(json!({}));
    }
    let output: Value = serde_json::from_slice(&stdout)
        .map_err(|error| format!("invalid command hook JSON stdout: {error}"))?;
    if !output.is_object() {
        return Err("command hook stdout must be a JSON object".into());
    }
    Ok(output)
}
