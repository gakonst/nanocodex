use resume_replay::{HistoryItem, RequestState};

fn item(id: &str, kind: &str, body: &str) -> HistoryItem {
    HistoryItem {
        id: Some(id.into()),
        kind: kind.into(),
        body: body.into(),
    }
}

#[test]
fn healthy_delta_and_reconnect_replay_preserve_the_contract() {
    let mut state = RequestState::new("thread-stable-key");
    state.record(item("user-1", "user", "first"));

    let first = state.build(false);
    assert_eq!(first.previous_response_id, None);
    assert_eq!(first.prompt_cache_key, "thread-stable-key");
    assert_eq!(first.items, vec![item("user-1", "user", "first")]);

    state.complete("response-1");
    state.record(item("call-1", "function_call", "exec"));
    state.record(item("output-1", "function_call_output", "done"));

    let healthy = state.build(false);
    assert_eq!(healthy.previous_response_id.as_deref(), Some("response-1"));
    assert_eq!(healthy.prompt_cache_key, "thread-stable-key");
    assert_eq!(
        healthy.items,
        vec![
            item("call-1", "function_call", "exec"),
            item("output-1", "function_call_output", "done"),
        ]
    );

    let replay = state.build(true);
    assert_eq!(replay.previous_response_id, None);
    assert_eq!(replay.prompt_cache_key, "thread-stable-key");
    assert_eq!(
        replay
            .items
            .iter()
            .map(|entry| (entry.id.as_deref(), entry.kind.as_str(), entry.body.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (None, "user", "first"),
            (None, "function_call", "exec"),
            (None, "function_call_output", "done"),
        ]
    );
}
