use std::{io::Read, sync::Arc};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::Mutex,
};

use super::CapturedOutput;

const DEFAULT_MAX_OUTPUT_TOKENS: usize = 10_000;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const BYTES_PER_TOKEN: usize = 4;
const READ_BUFFER_LENGTH: usize = 8 * 1024;
const REDACTION: &str = "[REDACTED]";

pub(super) fn effective_token_limit(requested: Option<i64>) -> usize {
    requested.map_or(DEFAULT_MAX_OUTPUT_TOKENS * BYTES_PER_TOKEN, |value| {
        usize::try_from(value)
            .unwrap_or(0)
            .saturating_mul(BYTES_PER_TOKEN)
            .min(MAX_OUTPUT_BYTES)
    })
}

pub(super) async fn drain(
    mut pipe: Option<impl AsyncRead + Unpin>,
    captured: Arc<Mutex<CapturedOutput>>,
    capture_limit: usize,
) {
    let Some(mut pipe) = pipe.take() else {
        return;
    };
    let mut buffer = [0_u8; READ_BUFFER_LENGTH];
    loop {
        match pipe.read(&mut buffer).await {
            Ok(0) => return,
            Ok(length) => captured.lock().await.push(&buffer[..length], capture_limit),
            Err(error) => {
                captured.lock().await.push(
                    format!("failed to read command output: {error}").as_bytes(),
                    capture_limit,
                );
                return;
            }
        }
    }
}

pub(super) fn drain_blocking(
    mut pipe: Box<dyn Read + Send>,
    captured: Arc<Mutex<CapturedOutput>>,
    capture_limit: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut buffer = [0_u8; READ_BUFFER_LENGTH];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => return,
                Ok(length) => captured
                    .blocking_lock()
                    .push(&buffer[..length], capture_limit),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                // PTY masters commonly report EIO when their slave closes.
                Err(_) => return,
            }
        }
    })
}

pub(super) fn redact_and_limit(
    mut output: String,
    secrets: &[String],
    limit: usize,
) -> (String, bool) {
    for secret in secrets {
        output = output.replace(secret, REDACTION);
    }
    if output.len() <= limit {
        return (output, false);
    }

    let original_token_count = output.len().saturating_add(3) / BYTES_PER_TOKEN;
    let total_lines = output.lines().count();
    let left_budget = limit / 2;
    let right_budget = limit.saturating_sub(left_budget);
    let prefix_end = floor_char_boundary(&output, left_budget);
    let suffix_start =
        ceil_char_boundary(&output, output.len().saturating_sub(right_budget)).max(prefix_end);
    let removed_tokens = output.len().saturating_sub(limit).saturating_add(3) / BYTES_PER_TOKEN;
    let truncated = format!(
        "Warning: truncated output (original token count: {original_token_count})\nTotal output lines: {total_lines}\n\n{}…{removed_tokens} tokens truncated…{}",
        &output[..prefix_end],
        &output[suffix_start..],
    );
    (truncated, true)
}

fn floor_char_boundary(output: &str, target: usize) -> usize {
    let mut boundary = target.min(output.len());
    while !output.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    boundary
}

fn ceil_char_boundary(output: &str, target: usize) -> usize {
    let mut boundary = target.min(output.len());
    while !output.is_char_boundary(boundary) {
        boundary = boundary.saturating_add(1);
    }
    boundary
}
