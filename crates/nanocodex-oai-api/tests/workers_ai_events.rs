//! This fixture is emitted by the JS Workers AI adapter, covering every output kind.
use nanocodex_oai_api::responses::ServerEvent;

#[test]
fn workers_ai_adapter_events_are_accepted_by_the_rust_protocol() {
    let mut completed_items = 0;
    let mut terminal = false;
    for line in include_str!("fixtures/workers_ai_events.jsonl").lines() {
        let event: ServerEvent = serde_json::from_str(line).expect("adapter event must parse");
        match event {
            ServerEvent::OutputItemDone { .. } => completed_items += 1,
            ServerEvent::Completed { response } => {
                assert_eq!(response.end_turn, Some(false));
                assert_eq!(response.usage.unwrap().total_tokens, 20);
                terminal = true;
            }
            _ => {}
        }
    }
    assert_eq!(
        completed_items, 5,
        "reasoning, message, custom, function, search"
    );
    assert!(terminal);
}
