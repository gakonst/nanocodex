mod agent;
mod agents_md;
mod compaction;
mod stream;
mod wire;

use std::{
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

use clap::ValueEnum;
use serde::{Serialize, Serializer};
use serde_json::{Value, value::RawValue};

use self::wire::Usage;
use crate::{
    AgentError, Result,
    protocol::{EventWriter, Task},
};

const TRANSPORT: &str = "responses_websocket_v2";
const COST_STATUS: &str = "not_reported_by_responses_api";
const SYSTEM_PROMPT: &str = include_str!("prompts/system.md");

#[derive(Serialize)]
struct ApiEvent<'a> {
    direction: &'static str,
    transport: &'static str,
    phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_call_index: Option<u32>,
    #[serde(serialize_with = "serialize_compact_raw_json")]
    event: &'a RawValue,
}

fn serialize_compact_raw_json<S>(
    event: &&RawValue,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let value = serde_json::from_str::<Value>(event.get()).map_err(serde::ser::Error::custom)?;
    value.serialize(serializer)
}

/// OpenAI-specific settings for the deliberately single-provider harness.
pub struct ModelConfig {
    pub model: String,
    pub api_key: String,
    pub effort: ReasoningEffort,
    pub websocket_url: String,
}

impl ModelConfig {
    pub(super) const fn orchestration() -> &'static str {
        "local_code_mode"
    }

    pub(super) const fn system_prompt() -> &'static str {
        SYSTEM_PROMPT
    }
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

#[derive(Default, Serialize)]
struct RunStats {
    model_calls: u32,
    compactions: u32,
    tool_calls: u32,
    connection_attempts: u32,
    websocket_reconnects: u32,
    connection_duration_ns: u64,
    model_duration_ns: u64,
    warmup_duration_ns: u64,
    tool_work_duration_ns: u64,
    tool_wall_duration_ns: u64,
    usage: UsageTotals,
    warmup_usage: UsageTotals,
    last_response_id: Option<String>,
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
    stats: &'a RunStats,
) -> TerminalPayload<'a> {
    TerminalPayload {
        status: terminal_status,
        model: &config.model,
        effort: config.effort.as_str(),
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
struct RunStarted<'a> {
    mode: &'static str,
    model: &'a str,
    effort: &'static str,
    transport: &'static str,
    orchestration: &'static str,
    websocket_url: &'a str,
    workspace: Option<&'a str>,
    instruction_bytes: usize,
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

fn elapsed_ns(started_at: Instant) -> u64 {
    duration_ns(started_at.elapsed())
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn display_endpoint(endpoint: &str) -> &str {
    endpoint.split_once('?').map_or(endpoint, |(base, _)| base)
}

#[cfg(test)]
mod tests {
    use serde_json::value::RawValue;

    use super::{ApiEvent, TRANSPORT};
    use crate::protocol::EventWriter;

    #[test]
    fn pretty_api_events_remain_one_jsonl_record() {
        let raw = RawValue::from_string("{\n  \"type\": \"error\"\n}".to_owned())
            .expect("the regression fixture should be valid JSON");
        let mut output = Vec::new();
        EventWriter::new(&mut output, "test".to_owned())
            .emit(
                "api.event",
                ApiEvent {
                    direction: "inbound",
                    transport: TRANSPORT,
                    phase: "generation",
                    model_call_index: Some(1),
                    event: &raw,
                },
            )
            .expect("the event should serialize");

        let text = String::from_utf8(output).expect("JSONL should be UTF-8");
        assert_eq!(text.lines().count(), 1);
        serde_json::from_str::<serde_json::Value>(&text).expect("JSONL should parse");
    }
}
