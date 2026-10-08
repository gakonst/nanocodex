//! Public-boundary regression: reopening must not expand an admitted tool catalog.
#![cfg(all(feature = "claude", feature = "sqlite"))]

use axum::{Json, Router, routing::post};
use nanocodex_agent::{Nanocodex, PromptRequest};
use nanocodex_claude::{Claude, ClaudeClient, ToolDefinition};
use nanocodex_durability::{DurableAgentExt, DurableSession, SqliteStore};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

fn sse(tool: bool) -> String {
    let block = if tool {
        json!({"type":"tool_use","id":"new-effect","name":"new-host-effect","input":{}})
    } else {
        json!({"type":"text","text":"done"})
    };
    [
        json!({"type":"message_start","message":{"id":"synthetic","role":"assistant","model":"test","content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":block}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":if tool {"tool_use"} else {"end_turn"}},"usage":{"output_tokens":5}}),
        json!({"type":"message_stop"}),
    ].into_iter().map(|frame|format!("data: {frame}\n\n")).collect()
}

#[tokio::test]
async fn reopened_pending_turn_cannot_dispatch_a_tool_outside_its_frozen_catalog() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for block_index in [1, 2] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let app = Router::new().route(
            "/v1/messages",
            post({
                let (started, release, requests) =
                    (started.clone(), release.clone(), requests.clone());
                move |Json(body): Json<Value>| {
                    let (started, release, requests) =
                        (started.clone(), release.clone(), requests.clone());
                    async move {
                        let index = {
                            let mut log = requests.lock().unwrap();
                            log.push(body);
                            log.len()
                        };
                        if index == block_index {
                            started.notify_one();
                            release.notified().await;
                        }
                        (
                            [("content-type", "text/event-stream")],
                            sse(index <= if block_index == 1 { 2 } else { 3 })
                                .replace("new-effect", &format!("new-effect-{index}")),
                        )
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ClaudeClient::new(reqwest::Client::new(), endpoint, "synthetic");
        let state = || async {
            DurableSession::open(SqliteStore::open(&path).unwrap(), "synthetic-catalog")
                .await
                .unwrap()
        };
        let request =
            || PromptRequest::new("only the admitted capabilities").request_id("frozen-turn");
        // The first owner has no tools. Its admitted request stays in-flight.
        let (old, old_events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
            .max_tokens(4096)
            .durability(state().await)
            .await
            .unwrap()
            .build()
            .unwrap();
        let old_turn = old.prompt(request()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), started.notified())
            .await
            .unwrap();
        let effects = Arc::new(AtomicUsize::new(0));
        let counter = effects.clone();
        // A host upgrade adds a tool. The old operation must retain its zero-tool catalog.
        let (recovered, recovered_events) = Nanocodex::builder(Claude::new(client, "new-model"))
            .max_tokens(4096)
            .client_tool_search()
            .tool(
                ToolDefinition {
                    name: "new-host-effect".into(),
                    description: "new capability".into(),
                    input_schema: json!({"type":"object"}),
                    strict: None,
                    defer_loading: true,
                },
                move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async { Ok("effect happened".into()) }
                },
            )
            .durability(state().await)
            .await
            .unwrap()
            .build()
            .unwrap();
        let outcome = recovered.prompt(request()).await.unwrap().result().await;
        release.notify_one();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), old_turn.result())
                .await
                .unwrap()
                .is_err()
        );
        let log = requests.lock().unwrap().clone();
        assert_eq!(
            log.len(),
            block_index + 2,
            "unknown tools continue through paired receipts, including reopen"
        );
        assert_eq!(
            log[block_index - 1],
            log[block_index],
            "recovery must keep the frozen request body"
        );
        assert_eq!(
            log[1]["tools"], log[2]["tools"],
            "retry cannot expand the admitted catalog"
        );
        assert_eq!(outcome.as_ref().unwrap().final_message(), "done");
        let receipts: Vec<_> = log.last().unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["content"].as_array().unwrap())
            .filter(|block| block["type"] == "tool_result")
            .collect();
        assert_eq!(receipts.len(), block_index);
        assert!(receipts.iter().all(|receipt| receipt["is_error"] == true));
        assert!(
            log[1]["tools"].as_array().is_none_or(Vec::is_empty),
            "the admitted request has no tools"
        );
        let observed = effects.load(Ordering::SeqCst);
        eprintln!(
            "frozen catalog requests={}; unexpected newly attached effect executions={observed}; block_index={block_index}; recovered outcome={outcome:?}",
            log.len()
        );
        recovered.shutdown().await.unwrap();
        let _ = old.shutdown().await;
        drop((old, old_events, recovered, recovered_events));
        server.abort();
        assert_eq!(
            observed, 0,
            "a recovered frozen catalog cannot authorize a newly attached host tool"
        );
    }
}
