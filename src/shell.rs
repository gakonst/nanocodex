use std::{
    env,
    ffi::{OsStr, OsString},
    path::Path,
    time::Duration,
    time::Instant,
};

use serde::{Deserialize, Serialize};
use tokio::{process::Command as AsyncCommand, time::timeout};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 10_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecCommandArgs {
    pub(crate) cmd: String,
    #[serde(default)]
    pub(crate) workdir: Option<String>,
    #[serde(default)]
    pub(crate) tty: bool,
    // Accepted and decoded to keep parity with Codex's standard tool shape;
    // completion-only execution intentionally ignores the yield hint.
    #[serde(default, rename = "yield_time_ms")]
    pub(crate) _yield_time_ms: Option<u64>,
    #[serde(default)]
    pub(crate) max_output_tokens: Option<u64>,
    #[serde(default)]
    pub(crate) shell: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct ExecCommandResult {
    pub(crate) output: String,
    pub(crate) wall_time_seconds: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) original_token_count: Option<u64>,
}

impl ExecCommandResult {
    pub(crate) fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }

    pub(crate) fn tool_error(output: String) -> Self {
        Self {
            output,
            wall_time_seconds: 0.0,
            exit_code: None,
            original_token_count: None,
        }
    }
}

pub(crate) async fn execute_command(
    args: ExecCommandArgs,
    default_workdir: &str,
) -> ExecCommandResult {
    let started_at = Instant::now();
    if args.cmd.trim().is_empty() {
        return command_error(started_at, "exec_command requires a non-empty cmd");
    }
    if args.tty {
        return command_error(
            started_at,
            "tty=true is not available in this completion-only exec_command; rerun without tty",
        );
    }

    let workdir = args
        .workdir
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map_or_else(
            || Path::new(default_workdir).to_path_buf(),
            |value| {
                let path = Path::new(value);
                if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    Path::new(default_workdir).join(path)
                }
            },
        );
    let shell = args
        .shell
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("/bin/sh");
    let max_output_tokens = args
        .max_output_tokens
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS)
        .clamp(1, DEFAULT_MAX_OUTPUT_TOKENS);
    let (environment, secrets) = sanitized_environment();

    let mut command = AsyncCommand::new(shell);
    command
        .args(["-lc", &args.cmd])
        .current_dir(workdir)
        .env_clear()
        .envs(environment)
        .kill_on_drop(true);

    let result = timeout(COMMAND_TIMEOUT, command.output()).await;
    let elapsed = started_at.elapsed();
    match result {
        Ok(Ok(process_output)) => {
            let exit_code = process_output.status.code();
            let combined = redact_secrets(
                combine_output(&process_output.stdout, &process_output.stderr),
                &secrets,
            );
            let (output, original_token_count) = truncate_output(combined, max_output_tokens);
            ExecCommandResult {
                output,
                wall_time_seconds: elapsed.as_secs_f64(),
                exit_code,
                original_token_count,
            }
        }
        Ok(Err(error)) => command_error(started_at, &format!("failed to start command: {error}")),
        Err(_) => command_error(
            started_at,
            &format!(
                "command exceeded the {} second execution limit and was terminated",
                COMMAND_TIMEOUT.as_secs()
            ),
        ),
    }
}

fn sanitized_environment() -> (Vec<(OsString, OsString)>, Vec<String>) {
    let mut environment = Vec::new();
    let mut secrets = Vec::new();
    for (name, value) in env::vars_os() {
        if is_sensitive_environment_name(&name) {
            if let Some(value) = value.to_str().filter(|value| value.len() >= 8) {
                secrets.push(value.to_owned());
            }
        } else {
            environment.push((name, value));
        }
    }
    secrets.sort_unstable_by_key(|secret| std::cmp::Reverse(secret.len()));
    secrets.dedup();
    (environment, secrets)
}

fn is_sensitive_environment_name(name: &OsStr) -> bool {
    name.to_string_lossy()
        .to_ascii_uppercase()
        .split('_')
        .any(|part| {
            matches!(
                part,
                "AUTH"
                    | "AUTHORIZATION"
                    | "COOKIE"
                    | "CREDENTIAL"
                    | "CREDENTIALS"
                    | "KEY"
                    | "PASS"
                    | "PASSWD"
                    | "PASSWORD"
                    | "SECRET"
                    | "TOKEN"
            )
        })
}

fn redact_secrets(mut output: String, secrets: &[String]) -> String {
    for secret in secrets {
        output = output.replace(secret, "[REDACTED]");
    }
    output
}

fn command_error(started_at: Instant, message: &str) -> ExecCommandResult {
    ExecCommandResult {
        output: message.to_owned(),
        wall_time_seconds: started_at.elapsed().as_secs_f64(),
        exit_code: None,
        original_token_count: None,
    }
}

fn combine_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, true) => stdout.into_owned(),
        (true, false) => stderr.into_owned(),
        (true, true) => String::new(),
        (false, false) => format!("{stdout}\n[stderr]\n{stderr}"),
    }
}

fn truncate_output(output: String, max_tokens: u64) -> (String, Option<u64>) {
    let character_count = output.chars().count();
    let original_token_count = u64::try_from(character_count.div_ceil(4)).unwrap_or(u64::MAX);
    if original_token_count <= max_tokens {
        return (output, None);
    }

    let max_characters = usize::try_from(max_tokens.saturating_mul(4)).unwrap_or(usize::MAX);
    let head_length = max_characters / 2;
    let tail_length = max_characters.saturating_sub(head_length);
    let head: String = output.chars().take(head_length).collect();
    let mut tail: Vec<char> = output.chars().rev().take(tail_length).collect();
    tail.reverse();
    (
        format!(
            "{head}\n\n... output truncated (approximately {original_token_count} tokens) ...\n\n{}",
            tail.into_iter().collect::<String>()
        ),
        Some(original_token_count),
    )
}
