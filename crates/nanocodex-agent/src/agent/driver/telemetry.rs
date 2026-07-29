use super::*;

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
        model = nanocodex_oai_api::MODEL,
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
    pub(super) mode: ReasoningMode,
    pub(super) effort: Thinking,
}
