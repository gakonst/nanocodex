use super::*;

use serde::Serialize;

pub(super) fn agent_compact_span(
    parent: Option<&tracing::Span>,
    session_id: &str,
    lineage_id: &str,
    origin: &AgentOrigin,
) -> tracing::Span {
    let parent_id = parent.and_then(tracing::Span::id);
    info_span!(
        target: "nanocodex",
        parent: parent_id,
        "agent.compact",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        status = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
        session.id = session_id,
        session.lineage_id = lineage_id,
        parent.session.id = tracing::field::Empty,
        agent.origin = origin.kind,
        agent.depth = origin.depth,
    )
}

pub(super) fn agent_turn_span(
    parent: Option<&tracing::Span>,
    session_id: &str,
    lineage_id: &str,
    origin: &AgentOrigin,
    reasoning: ReasoningSettings,
    turn_index: u64,
    prompt_bytes: usize,
) -> tracing::Span {
    let parent_id = parent.and_then(tracing::Span::id);
    let parented = parent_id.is_some();
    let span = info_span!(
        target: "nanocodex",
        parent: parent_id,
        "agent.turn",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        session.id = session_id,
        session.lineage_id = lineage_id,
        parent.session.id = tracing::field::Empty,
        agent.origin = origin.kind,
        agent.depth = origin.depth,
        trace.parented = parented,
        model = reasoning.model.as_str(),
        reasoning.mode = reasoning.mode.as_str(),
        reasoning.effort = reasoning.effort.as_str(),
        thinking = reasoning.effort.as_str(),
        turn.index = turn_index,
        prompt.bytes = prompt_bytes,
        usage.input_tokens = tracing::field::Empty,
        usage.cached_input_tokens = tracing::field::Empty,
        usage.cache_write_input_tokens = tracing::field::Empty,
        usage.output_tokens = tracing::field::Empty,
        usage.reasoning_output_tokens = tracing::field::Empty,
        usage.total_tokens = tracing::field::Empty,
        cost.usd = tracing::field::Empty,
        cost.status = tracing::field::Empty,
        cost.service_tier = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    if let Some(parent_session_id) = &origin.parent_session_id {
        span.record("parent.session.id", parent_session_id.as_ref());
    }
    span
}

#[derive(Clone, Copy)]
pub(super) struct ReasoningSettings {
    pub(super) model: Model,
    pub(super) mode: ReasoningMode,
    pub(super) effort: Thinking,
}

pub(super) fn emit_replayed_terminal(
    events: &EventSink,
    config: &ModelConfig,
    thinking: Thinking,
    status: &'static str,
    usage: &TurnUsage,
) -> Result<()> {
    let kind = if status == "completed" {
        nanocodex_oai_api::events::AgentEventKind::RunCompleted
    } else {
        nanocodex_oai_api::events::AgentEventKind::RunFailed
    };
    events.emit(
        kind,
        ReplayedTerminal {
            status,
            model: config.model.as_str(),
            reasoning_mode: config.reasoning_mode.as_str(),
            effort: thinking.as_str(),
            transport: config.responses_transport.as_str(),
            orchestration: ModelConfig::orchestration(),
            duration_ms: 0,
            duration_ns: 0,
            model_calls: 0,
            steers: 0,
            compactions: 0,
            tool_calls: 0,
            connection_attempts: 0,
            websocket_reconnects: 0,
            response_attempts: 0,
            response_retries: 0,
            connection_duration_ns: 0,
            retry_backoff_duration_ns: 0,
            model_duration_ns: 0,
            compaction_duration_ns: 0,
            warmup_duration_ns: 0,
            tool_work_duration_ns: 0,
            tool_wall_duration_ns: 0,
            usage: ReplayedUsage::from(usage),
            warmup_usage: ReplayedUsage::default(),
            estimated_cost: usage.estimated_cost(),
            cost_usd: usage.estimated_cost().map(|cost| cost.amount().as_f64()),
            cost_status: usage.cost_status(),
        },
    )?;
    Ok(())
}

#[derive(Serialize)]
struct ReplayedTerminal<'a> {
    status: &'static str,
    model: &'a str,
    reasoning_mode: &'static str,
    effort: &'static str,
    transport: &'static str,
    orchestration: &'static str,
    duration_ms: u64,
    duration_ns: u64,
    model_calls: u32,
    steers: u32,
    compactions: u32,
    tool_calls: u32,
    connection_attempts: u32,
    websocket_reconnects: u32,
    response_attempts: u32,
    response_retries: u32,
    connection_duration_ns: u64,
    retry_backoff_duration_ns: u64,
    model_duration_ns: u64,
    compaction_duration_ns: u64,
    warmup_duration_ns: u64,
    tool_work_duration_ns: u64,
    tool_wall_duration_ns: u64,
    usage: ReplayedUsage,
    warmup_usage: ReplayedUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_cost: Option<&'a nanocodex_oai_api::pricing::EstimatedUsdCost>,
    cost_usd: Option<f64>,
    cost_status: nanocodex_oai_api::pricing::CostStatus,
}

#[derive(Default, Serialize)]
struct ReplayedUsage {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

impl From<&TurnUsage> for ReplayedUsage {
    fn from(usage: &TurnUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens(),
            cached_input_tokens: usage.cached_input_tokens(),
            cache_write_input_tokens: usage.cache_write_input_tokens(),
            output_tokens: usage.output_tokens(),
            reasoning_output_tokens: usage.reasoning_output_tokens(),
            total_tokens: usage.total_tokens(),
        }
    }
}
