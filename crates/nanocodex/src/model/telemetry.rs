use std::time::Duration;

#[cfg(not(target_family = "wasm"))]
use std::path::PathBuf;

use nanocodex_core::{ModelConfig, Usage};
use serde::Serialize;
use serde_json::value::RawValue;
use web_time::Instant;

#[cfg(not(target_family = "wasm"))]
use crate::NanocodexError;
use crate::Result;
use nanocodex_service::{TRANSPORT, TransportStatsDelta};
use nanocodex_tools::ToolOutputBody;

const COST_STATUS: &str = "not_reported_by_responses_api";

#[derive(Serialize)]
pub(super) struct ModelCallStarted<'a> {
    pub(super) call_index: u32,
    pub(super) model: &'a str,
    pub(super) effort: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) previous_response_id: Option<&'a str>,
}

#[derive(Serialize)]
pub(super) struct WarmupStarted<'a> {
    pub(super) model: &'a str,
    pub(super) prompt_cache_key: &'a str,
}

#[derive(Serialize)]
pub(super) struct WarmupCompleted<'a> {
    pub(super) response_id: &'a str,
    pub(super) attempt: u32,
    pub(super) connection_generation: u32,
    pub(super) duration_ns: u64,
    pub(super) usage: Option<&'a Usage>,
}

#[derive(Serialize)]
pub(super) struct WarmupFailed<'a> {
    pub(super) duration_ns: u64,
    pub(super) error: &'a str,
}

#[derive(Serialize)]
pub(super) struct ModelCallCompleted<'a> {
    pub(super) call_index: u32,
    pub(super) model: &'a str,
    pub(super) response_id: &'a str,
    pub(super) attempt: u32,
    pub(super) connection_generation: u32,
    pub(super) status: &'a str,
    pub(super) duration_ns: u64,
    pub(super) time_to_first_event_ns: u64,
    pub(super) time_to_first_output_ns: Option<u64>,
    pub(super) tool_calls: usize,
    pub(super) usage: Option<&'a Usage>,
}

#[derive(Serialize)]
pub(super) struct ModelCallFailed<'a> {
    pub(super) call_index: u32,
    pub(super) model: &'a str,
    pub(super) duration_ns: u64,
    pub(super) error: &'a str,
}

#[derive(Serialize)]
pub(super) struct CompactionStarted<'a> {
    pub(super) after_model_call_index: u32,
    pub(super) active_context_tokens: u64,
    pub(super) auto_compact_token_limit: u64,
    pub(super) previous_response_id: &'a str,
}

#[derive(Serialize)]
pub(super) struct CompactionCompleted<'a> {
    pub(super) after_model_call_index: u32,
    pub(super) response_id: &'a str,
    pub(super) attempt: u32,
    pub(super) connection_generation: u32,
    pub(super) status: &'a str,
    pub(super) duration_ns: u64,
    pub(super) time_to_first_event_ns: u64,
    pub(super) time_to_first_output_ns: Option<u64>,
    pub(super) usage: Option<&'a Usage>,
}

#[derive(Serialize)]
pub(super) struct CompactionFailed<'a> {
    pub(super) after_model_call_index: u32,
    pub(super) duration_ns: u64,
    pub(super) error: &'a str,
}

#[derive(Serialize)]
pub(super) struct ToolCallEvent<'a, T> {
    pub(super) call_id: &'a str,
    pub(super) tool: &'a str,
    pub(super) arguments: T,
    pub(super) model_call_index: u32,
}

#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum ToolCallArguments<'a> {
    Raw(&'a RawValue),
    Text(&'a str),
}

#[derive(Serialize)]
pub(super) struct ToolResultEvent<'a> {
    pub(super) call_id: &'a str,
    pub(super) tool: &'a str,
    pub(super) status: &'static str,
    pub(super) duration_ns: u64,
    pub(super) result: &'a ToolOutputBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) metadata: Option<&'a RawValue>,
}

#[allow(clippy::struct_field_names)]
#[derive(Default, Serialize)]
pub(super) struct UsageTotals {
    pub(super) input_tokens: u64,
    pub(super) cached_input_tokens: u64,
    pub(super) cache_write_input_tokens: u64,
    pub(super) output_tokens: u64,
    pub(super) reasoning_output_tokens: u64,
    pub(super) total_tokens: u64,
}

impl UsageTotals {
    pub(super) fn add(&mut self, usage: &Usage) {
        self.input_tokens += usage.input_tokens;
        self.cached_input_tokens += usage
            .input_tokens_details
            .as_ref()
            .map_or(0, |details| details.cached_tokens);
        self.cache_write_input_tokens += usage
            .input_tokens_details
            .as_ref()
            .map_or(0, |details| details.cache_write_tokens);
        self.output_tokens += usage.output_tokens;
        self.reasoning_output_tokens += usage
            .output_tokens_details
            .as_ref()
            .map_or(0, |details| details.reasoning_tokens);
        self.total_tokens += usage.total_tokens;
    }
}

#[derive(Default, Serialize)]
pub(super) struct RunStats {
    pub(super) model_calls: u32,
    pub(super) steers: u32,
    pub(super) compactions: u32,
    pub(super) tool_calls: u32,
    pub(super) connection_attempts: u32,
    pub(super) websocket_reconnects: u32,
    pub(super) response_attempts: u32,
    pub(super) response_retries: u32,
    pub(super) connection_duration_ns: u64,
    pub(super) retry_backoff_duration_ns: u64,
    pub(super) model_duration_ns: u64,
    pub(super) warmup_duration_ns: u64,
    pub(super) tool_work_duration_ns: u64,
    pub(super) tool_wall_duration_ns: u64,
    pub(super) usage: UsageTotals,
    pub(super) warmup_usage: UsageTotals,
    pub(super) last_response_id: Option<String>,
}

impl RunStats {
    pub(super) fn apply_transport(&mut self, delta: TransportStatsDelta) {
        self.connection_attempts = delta.connection_attempts;
        self.websocket_reconnects = delta.websocket_reconnects;
        self.response_attempts = delta.response_attempts;
        self.response_retries = delta.response_retries;
        self.connection_duration_ns = delta.connection_duration_ns;
        self.retry_backoff_duration_ns = delta.retry_backoff_duration_ns;
    }
}

#[cfg(not(target_family = "wasm"))]
pub(super) fn resolve_workspace(requested: Option<&str>) -> Result<String> {
    let requested = PathBuf::from(requested.unwrap_or("."));
    let resolved =
        std::fs::canonicalize(&requested).map_err(|source| NanocodexError::ResolveWorkspace {
            path: requested,
            source,
        })?;
    if !resolved.is_dir() {
        return Err(NanocodexError::WorkspaceNotDirectory { path: resolved });
    }
    resolved
        .into_os_string()
        .into_string()
        .map_err(|path| NanocodexError::WorkspaceNotUtf8 {
            path: PathBuf::from(path),
        })
}

#[cfg(target_family = "wasm")]
#[expect(
    clippy::unnecessary_wraps,
    reason = "matches the native workspace-resolution contract"
)]
pub(super) fn resolve_workspace(requested: Option<&str>) -> Result<String> {
    Ok(requested.unwrap_or(".").to_owned())
}

pub(super) fn terminal_payload<'a>(
    terminal_status: &'static str,
    elapsed: Duration,
    config: &'a ModelConfig,
    stats: &'a RunStats,
) -> TerminalPayload<'a> {
    TerminalPayload {
        status: terminal_status,
        model: nanocodex_core::MODEL,
        effort: config.thinking.as_str(),
        transport: TRANSPORT,
        orchestration: ModelConfig::orchestration(),
        duration_ms: duration_ms(elapsed),
        duration_ns: duration_ns(elapsed),
        stats,
        cost_usd: None,
        cost_status: COST_STATUS,
    }
}

#[derive(Serialize)]
pub(super) struct RunStarted<'a> {
    pub(super) mode: &'static str,
    pub(super) model: &'a str,
    pub(super) effort: &'static str,
    pub(super) transport: &'static str,
    pub(super) orchestration: &'static str,
    pub(super) websocket_url: &'a str,
    pub(super) workspace: Option<&'a str>,
    pub(super) instruction_bytes: usize,
}

#[derive(Serialize)]
pub(super) struct RunSteered {
    pub(super) steer_index: u32,
    pub(super) instruction_bytes: usize,
}

#[derive(Serialize)]
pub(super) struct RunError<'a> {
    pub(super) message: &'a str,
}

#[derive(Serialize)]
pub(super) struct TerminalPayload<'a> {
    status: &'static str,
    model: &'a str,
    effort: &'static str,
    transport: &'static str,
    orchestration: &'static str,
    duration_ms: u64,
    duration_ns: u64,
    #[serde(flatten)]
    stats: &'a RunStats,
    cost_usd: Option<f64>,
    cost_status: &'static str,
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub(super) fn elapsed_ns(started_at: Instant) -> u64 {
    duration_ns(started_at.elapsed())
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

pub(super) fn display_endpoint(endpoint: &str) -> &str {
    endpoint.split_once('?').map_or(endpoint, |(base, _)| base)
}
