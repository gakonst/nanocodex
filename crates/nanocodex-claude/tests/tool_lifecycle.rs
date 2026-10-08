//! Tool identity must remain single-use across continuation and compaction.
use axum::{Json, Router, routing::post};
use nanocodex_agent::Nanocodex;
use nanocodex_claude::{Claude, ClaudeClient, ToolDefinition};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

fn stream(block: Value, stop: &str) -> String {
    let is_tool = block["type"] == "tool_use";
    let mut events = vec![
        json!({"type":"message_start","message":{"id":"msg","role":"assistant","model":"test","content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":if is_tool {json!({"type":"tool_use","id":block["id"],"name":"effect","input":{}})} else {block}}),
    ];
    if is_tool {
        events.push(json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}));
    }
    events.extend([
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":5}}),
        json!({"type":"message_stop"}),
    ]);
    events
        .into_iter()
        .map(|e| format!("data: {e}\n\n"))
        .collect()
}

async fn replay_is_rejected(after_compaction: bool) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let log = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = log.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let received = received.clone();
        async move {
            let n = { let mut r = received.lock().unwrap(); r.push(body); r.len() };
            let replay_at = if after_compaction {4} else {2};
            let (block,stop) = if n==1 || n==replay_at {
                (json!({"type":"tool_use","id":"effect-original","name":"effect","input":{}}),"tool_use")
            } else {
                (json!({"type":"text","text":"Completed effect; preserve its receipt."}),"end_turn")
            };
            ([("content-type","text/event-stream")],stream(block,stop))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .tool(
            ToolDefinition {
                name: "effect".into(),
                description: "Synthetic counted effect".into(),
                input_schema: json!({"type":"object"}),
                strict: None,
                defer_loading: false,
            },
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Ok("committed receipt".into()) }
            },
        )
        .build()
        .unwrap();
    let first = agent.prompt("perform once").await.unwrap().result().await;
    let failed = if after_compaction {
        first.unwrap();
        agent.compact().await.unwrap();
        agent.prompt("continue").await.unwrap().result().await
    } else {
        first
    };
    assert!(
        failed.is_err(),
        "reused tool identity must fail before executing"
    );
    assert_eq!(
        effects.load(Ordering::SeqCst),
        1,
        "a replayed ID must never repeat a side effect"
    );
    agent
        .prompt("recover without tools")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let requests = log.lock().unwrap();
    assert_eq!(requests.len(), if after_compaction { 5 } else { 3 });
    server.abort();
}

#[tokio::test]
async fn replayed_tool_id_is_rejected_in_followup() {
    replay_is_rejected(false).await;
}
#[tokio::test]
async fn replayed_tool_id_is_rejected_after_compaction() {
    replay_is_rejected(true).await;
}
