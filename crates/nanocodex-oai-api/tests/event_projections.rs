use nanocodex_oai_api::events::{
    AgentEvent, AgentEventData, AgentEventKind, AssistantEvent, ContextEvent, ModelEvent, RunEvent,
    ToolEvent, TransportEvent,
};
use serde_json::{Value, json};

fn event(kind: AgentEventKind, payload: Value) -> AgentEvent {
    serde_json::from_value(json!({
        "protocol_version": 1,
        "request_id": "request-1",
        "seq": 1,
        "type": kind,
        "payload": payload,
    }))
    .expect("canonical event should decode")
}

#[test]
fn events_only_decodes_common_lifecycle_projections() {
    let assistant = event(
        AgentEventKind::AssistantDelta,
        json!({
            "model_call_index": 2,
            "item_id": "item-1",
            "phase": "final_answer",
            "text": "done"
        }),
    );
    let AgentEventData::Assistant(AssistantEvent::Delta(delta)) = assistant
        .data()
        .expect("assistant projection should decode")
    else {
        panic!("expected assistant delta")
    };
    assert_eq!(delta.text, "done");

    let run = event(
        AgentEventKind::RunStarted,
        json!({
            "mode": "managed",
            "model": "gpt-5.6-sol",
            "reasoning_mode": "summary",
            "effort": "high",
            "transport": "managed",
            "orchestration": "single",
            "websocket_url": "wss://example.invalid/responses",
            "workspace": null,
            "instruction_bytes": 4
        }),
    );
    let AgentEventData::Run(RunEvent::Started(started)) =
        run.data().expect("run projection should decode")
    else {
        panic!("expected run start")
    };
    assert_eq!(started.instruction_bytes, 4);

    let tool = event(
        AgentEventKind::ToolCall,
        json!({
            "call_id": "call-1",
            "tool": "lookup",
            "arguments": {"query": "rust"},
            "model_call_index": 2
        }),
    );
    let AgentEventData::Tool(ToolEvent::Call(call)) =
        tool.data().expect("tool projection should decode")
    else {
        panic!("expected tool call")
    };
    assert_eq!(call.decode_arguments::<Value>().unwrap()["query"], "rust");

    let model = event(
        AgentEventKind::ModelCallFailed,
        json!({
            "call_index": 2,
            "model": "gpt-5.6-sol",
            "duration_ns": 42,
            "error": "unavailable"
        }),
    );
    let AgentEventData::Model(ModelEvent::CallFailed(failed)) =
        model.data().expect("model projection should decode")
    else {
        panic!("expected failed model call")
    };
    assert_eq!(failed.duration_ns, 42);

    let context = event(
        AgentEventKind::ModelCompactionStarted,
        json!({
            "after_model_call_index": 2,
            "active_context_tokens": 200000,
            "auto_compact_token_limit": 180000
        }),
    );
    let AgentEventData::Context(ContextEvent::CompactionStarted(started)) =
        context.data().expect("context projection should decode")
    else {
        panic!("expected compaction start")
    };
    assert_eq!(started.active_context_tokens, 200_000);

    let transport = event(
        AgentEventKind::ModelAttemptRetrying,
        json!({"attempt": 1, "next_attempt": 2}),
    );
    let AgentEventData::Transport(transport) = transport
        .data()
        .expect("transport projection should decode")
    else {
        panic!("expected transport diagnostic")
    };
    assert_eq!(transport.kind(), AgentEventKind::ModelAttemptRetrying);
    assert_eq!(
        transport.decode_payload::<Value>().unwrap()["next_attempt"],
        2
    );
}

#[cfg(not(feature = "client"))]
#[test]
fn events_only_keeps_provider_frames_as_lossless_transport_diagnostics() {
    let frame = event(
        AgentEventKind::ApiEvent,
        json!({
            "direction": "received",
            "transport": "managed",
            "phase": "generation",
            "model_call_index": 1,
            "event": {"type": "response.output_text.delta", "delta": "hello"}
        }),
    );
    let AgentEventData::Transport(frame) = frame.data().expect("raw frame should be retained")
    else {
        panic!("events-only should not expose the OpenAI frame projection")
    };
    assert_eq!(frame.kind(), AgentEventKind::ApiEvent);
    assert_eq!(
        frame.decode_payload::<Value>().unwrap()["event"]["delta"],
        "hello"
    );
}

#[test]
fn transport_projection_retains_raw_payload() {
    let transport = event(AgentEventKind::ModelAttemptStarted, json!({"attempt": 7}));
    let AgentEventData::Transport(transport) = transport.data().unwrap() else {
        panic!("expected transport diagnostic")
    };
    assert_eq!(
        TransportEvent::kind(&transport),
        AgentEventKind::ModelAttemptStarted
    );
    assert_eq!(transport.raw_payload().get(), r#"{"attempt":7}"#);
}
