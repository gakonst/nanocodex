//! Public facade journey: Claude builders work with the existing durability
//! extension, and a completed request replays after the host reopens its store.
use axum::{Json, Router, routing::post};
use nanocodex::{
    Claude, DurableAgentExt, Nanocodex, PromptRequest,
    claude::ClaudeClient,
    durability::{DurableSession, MemoryStore},
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[tokio::test]
async fn claude_durable_receipt_replays_through_facade() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = calls.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let requests = requests.clone();
        async move {
            assert_eq!(body["model"], "synthetic-claude");
            requests.fetch_add(1, Ordering::SeqCst);
            let frames = [
                json!({"type":"message_start","message":{"id":"synthetic-response","role":"assistant","model":"synthetic-claude","content":[],"usage":{"input_tokens":3,"output_tokens":0}}}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"recorded answer"}}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
                json!({"type":"message_stop"}),
            ];
            let stream: String = frames.into_iter().map(|frame| format!("data: {frame}\n\n")).collect();
            ([("content-type", "text/event-stream")], stream)
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic-key",
    );
    let store = MemoryStore::new().unwrap();
    for _ in 0..2 {
        let state = DurableSession::open(store.clone(), "facade-claude")
            .await
            .unwrap();
        let (agent, _events) = Nanocodex::builder(Claude::new(client.clone(), "synthetic-claude"))
            .max_tokens(1024)
            .durability(state)
            .await
            .unwrap()
            .build()
            .unwrap();
        let result = agent
            .prompt(
                PromptRequest::new("remember synthetic constraint").request_id("stable-request"),
            )
            .await
            .unwrap()
            .await
            .unwrap();
        assert_eq!(result.final_message(), "recorded answer");
        agent.shutdown().await.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
}
