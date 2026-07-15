mod agent;
mod stream;
mod wire;

use std::{
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

use clap::ValueEnum;
use serde::Serialize;

use self::wire::Usage;
use crate::{
    AgentError, Result,
    protocol::{EventWriter, Task},
};

const TRANSPORT: &str = "responses_websocket_v2";
const COST_STATUS: &str = "not_reported_by_responses_api";

/// OpenAI-specific settings for the deliberately single-provider harness.
pub struct ModelConfig {
    pub model: String,
    pub api_key: String,
    pub effort: ReasoningEffort,
    pub websocket_url: String,
    pub max_model_calls: u32,
}

#[derive(Clone, Copy, Default, ValueEnum)]
pub enum ReasoningEffort {
    #[default]
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ReasoningEffort {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

#[allow(clippy::struct_field_names)]
#[derive(Default, Serialize)]
struct UsageTotals {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

impl UsageTotals {
    fn add(&mut self, usage: &Usage) {
        self.input_tokens += usage.input_tokens;
        self.cached_input_tokens += usage.input_tokens_details.cached_tokens;
        self.cache_write_input_tokens += usage.input_tokens_details.cache_write_tokens;
        self.output_tokens += usage.output_tokens;
        self.reasoning_output_tokens += usage.output_tokens_details.reasoning_tokens;
        self.total_tokens += usage.total_tokens;
    }
}

#[derive(Default)]
struct RunStats {
    model_calls: u32,
    tool_calls: u32,
    model_duration_ns: u64,
    tool_duration_ns: u64,
    usage: UsageTotals,
    last_response_id: Option<String>,
}

struct ModelResponse {
    id: String,
    status: String,
    text: String,
    has_message: bool,
    function_calls: Vec<FunctionCall>,
    usage: Usage,
    time_to_first_event_ns: u64,
    time_to_first_output_ns: Option<u64>,
}

struct FunctionCall {
    call_id: String,
    name: String,
    arguments: String,
    caller: wire::Caller,
}

pub(crate) async fn run<W: Write>(
    events: &mut EventWriter<W>,
    task: &Task,
    config: &ModelConfig,
) -> Result<()> {
    agent::ModelRun::new(events, task, config).execute().await
}

fn resolve_workspace(requested: Option<&str>) -> Result<String> {
    let requested = PathBuf::from(requested.unwrap_or("."));
    let resolved =
        std::fs::canonicalize(&requested).map_err(|source| AgentError::ResolveWorkspace {
            path: requested,
            source,
        })?;
    if !resolved.is_dir() {
        return Err(AgentError::WorkspaceNotDirectory { path: resolved }.into());
    }
    resolved
        .into_os_string()
        .into_string()
        .map_err(|path| AgentError::WorkspaceNotUtf8 {
            path: PathBuf::from(path),
        })
        .map_err(Into::into)
}

fn terminal_payload<'a>(
    terminal_status: &'static str,
    elapsed: Duration,
    config: &'a ModelConfig,
    metrics: &'a RunStats,
) -> TerminalPayload<'a> {
    TerminalPayload {
        status: terminal_status,
        model: &config.model,
        effort: config.effort.as_str(),
        transport: TRANSPORT,
        model_calls: metrics.model_calls,
        tool_calls: metrics.tool_calls,
        duration_ms: duration_ms(elapsed),
        duration_ns: duration_ns(elapsed),
        model_duration_ns: metrics.model_duration_ns,
        tool_duration_ns: metrics.tool_duration_ns,
        last_response_id: metrics.last_response_id.as_deref(),
        usage: &metrics.usage,
        cost_usd: None,
        cost_status: COST_STATUS,
    }
}

#[derive(Serialize)]
struct RunStarted<'a> {
    mode: &'static str,
    model: &'a str,
    effort: &'static str,
    transport: &'static str,
    websocket_url: &'a str,
    workspace: Option<&'a str>,
    instruction_bytes: usize,
    max_model_calls: u32,
}

#[derive(Serialize)]
struct AssistantMessage<'a> {
    text: &'a str,
}

#[derive(Serialize)]
struct RunError<'a> {
    message: &'a str,
}

#[derive(Serialize)]
struct TerminalPayload<'a> {
    status: &'static str,
    model: &'a str,
    effort: &'static str,
    transport: &'static str,
    model_calls: u32,
    tool_calls: u32,
    duration_ms: u64,
    duration_ns: u64,
    model_duration_ns: u64,
    tool_duration_ns: u64,
    last_response_id: Option<&'a str>,
    usage: &'a UsageTotals,
    cost_usd: Option<f64>,
    cost_status: &'static str,
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn elapsed_ns(started_at: Instant) -> u64 {
    duration_ns(started_at.elapsed())
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn display_endpoint(endpoint: &str) -> &str {
    endpoint.split_once('?').map_or(endpoint, |(base, _)| base)
}
