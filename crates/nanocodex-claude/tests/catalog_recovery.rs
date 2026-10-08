//! Catalog recovery through public prompt API and real streaming HTTP.
use axum::{Json, Router, response::IntoResponse, routing::post};
use nanocodex_agent::Nanocodex;
use nanocodex_claude::{Claude, ClaudeClient, ToolDefinition};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

fn response(blocks: Vec<Value>, stop: &str) -> String {
    let mut events = vec![
        json!({"type":"message_start","message":{"id":"response","role":"assistant","model":"test","content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
    ];
    for (index, block) in blocks.into_iter().enumerate() {
        events.push(json!({"type":"content_block_start","index":index,"content_block":block}));
        events.push(json!({"type":"content_block_stop","index":index}));
    }
    events.push(
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":4096}}),
    );
    events.push(json!({"type":"message_stop"}));
    events
        .into_iter()
        .map(|e| format!("data: {e}\n\n"))
        .collect()
}
async fn fixture(
    responses: Vec<String>,
) -> (
    ClaudeClient,
    Arc<Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let received = log.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let log = received.clone();
            let responses = responses.clone();
            async move {
                let index = {
                    let mut log = log.lock().unwrap();
                    log.push(body.clone());
                    log.len() - 1
                };
                if std::env::var_os("NANOCLAUDE_CATALOG_TRACE").is_some() {
                    eprintln!("{}", json!({"request_index":index,"request":body}));
                }
                (
                    [("content-type", "text/event-stream")],
                    responses
                        .get(index)
                        .expect("unexpected provider request")
                        .clone(),
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "synthetic",
        ),
        log,
        server,
    )
}
fn tool(id: &str, name: &str) -> Value {
    json!({"type":"tool_use","id":id,"name":name,"input":{}})
}
fn effect() -> ToolDefinition {
    ToolDefinition {
        name: "effect".into(),
        description: "synthetic effect".into(),
        input_schema: json!({"type":"object"}),
        strict: None,
        defer_loading: false,
    }
}
#[tokio::test]
async fn unknown_call_is_paired_without_blocking_admitted_effects_or_expanding_catalog() {
    for name in ["Write", "memories__write"] {
        let (client, log, server) = fixture(vec![
            response(vec![tool("completed", "effect")], "tool_use"),
            response(
                vec![tool("wrong", name), tool("also-admitted", "effect")],
                "tool_use",
            ),
            response(vec![json!({"type":"text","text":"recovered"})], "end_turn"),
        ])
        .await;
        let effects = Arc::new(AtomicUsize::new(0));
        let counter = effects.clone();
        let (agent, _events) = Nanocodex::builder(Claude::new(client, "test"))
            .max_tokens(128_000)
            .tool(effect(), move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Ok("committed".into()) }
            })
            .build()
            .unwrap();
        let result = agent
            .prompt("finish the task")
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        assert_eq!(result.final_message(), "recovered");
        assert_eq!(effects.load(Ordering::SeqCst), 2);
        {
            let requests = log.lock().unwrap();
            assert_eq!(requests.len(), 3);
            assert!(requests.iter().all(|r| r["tools"] == requests[0]["tools"]));
            let messages = requests[2]["messages"].as_array().unwrap();
            let receipts: Vec<_> = messages
                .iter()
                .flat_map(|m| m["content"].as_array().unwrap())
                .filter(|b| b["type"] == "tool_result")
                .collect();
            assert_eq!(receipts.len(), 3);
            assert!(!receipts[0]["is_error"].as_bool().unwrap_or(false));
            assert_eq!(receipts[1]["is_error"], true);
            assert!(!receipts[2]["is_error"].as_bool().unwrap_or(false));
            eprintln!(
                "catalog={name}; requests=3; effect executions=2; recovered={}",
                result.final_message()
            );
        }
        agent.shutdown().await.unwrap();
        server.abort();
    }
}
#[tokio::test]
async fn repeated_unknown_calls_continue_but_ids_cannot_be_replayed() {
    for reuse in [false, true] {
        let (client, log, server) = fixture(vec![
            response(vec![tool("rejected", "Write")], "tool_use"),
            response(
                vec![tool(
                    if reuse { "rejected" } else { "second" },
                    if reuse { "effect" } else { "Write" },
                )],
                "tool_use",
            ),
            response(
                vec![json!({"type":"text","text":"new turn works"})],
                "end_turn",
            ),
        ])
        .await;
        let effects = Arc::new(AtomicUsize::new(0));
        let counter = effects.clone();
        let (agent, _events) = Nanocodex::builder(Claude::new(client, "test"))
            .max_tokens(128_000)
            .tool(effect(), move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Ok("unexpected".into()) }
            })
            .build()
            .unwrap();
        let outcome = agent.prompt("finish").await.unwrap().result().await;
        if reuse {
            assert!(
                outcome
                    .unwrap_err()
                    .to_string()
                    .contains("reused an admitted")
            );
            assert_eq!(log.lock().unwrap().len(), 2);
            let result = agent
                .prompt("continue safely")
                .await
                .unwrap()
                .result()
                .await
                .unwrap();
            assert_eq!(result.final_message(), "new turn works");
        } else {
            assert_eq!(outcome.unwrap().final_message(), "new turn works");
            assert_eq!(log.lock().unwrap().len(), 3);
            let requests = log.lock().unwrap();
            let receipts: Vec<_> = requests[2]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|message| message["content"].as_array().unwrap())
                .filter(|block| block["type"] == "tool_result")
                .collect();
            assert_eq!(receipts.len(), 2);
            assert!(receipts.iter().all(|receipt| receipt["is_error"] == true));
        }
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        eprintln!("reuse={reuse}; unknown calls paired; reused IDs rejected; executions=0");
        agent.shutdown().await.unwrap();
        server.abort();
    }
}
#[tokio::test]
async fn undiscovered_tool_must_be_searched_before_recovery_can_execute_it() {
    let (client, log, server) = fixture(vec![
        response(vec![tool("wrong", "Write")], "tool_use"),
        response(vec![tool("undiscovered", "effect")], "tool_use"),
        response(vec![json!({"type":"tool_use","id":"search","name":"ToolSearch","input":{"query":"effect"}})], "tool_use"),
        response(vec![tool("discovered", "effect")], "tool_use"),
        response(vec![json!({"type":"text","text":"discovered and finished"})], "end_turn"),
    ]).await;
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let mut definition = effect();
    definition.defer_loading = true;
    let (agent, _events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .client_tool_search()
        .tool(definition, move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("committed".into()) }
        })
        .build()
        .unwrap();
    let error = agent
        .prompt("use the effect")
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("before discovery"));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    let result = agent
        .prompt("discover the effect first")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "discovered and finished");
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(log.lock().unwrap().len(), 5);
    eprintln!(
        "unadmitted -> recovery -> undiscovered still rejected -> new turn ToolSearch -> effect; executions=1"
    );
    agent.shutdown().await.unwrap();
    server.abort();
}
#[tokio::test]
async fn server_effect_before_invalid_response_is_not_automatically_retried() {
    let (client, log, server) = fixture(vec![response(vec![
        json!({"type":"server_tool_use","id":"server","name":"web_search","input":{"query":"synthetic"}}),
        tool("wrong", "Write"),
        tool("wrong", "Write"),
    ], "tool_use")]).await;
    let (agent, _events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .build()
        .unwrap();
    let error = agent
        .prompt("finish")
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("duplicate or empty Claude tool_use id"),
        "{error}"
    );
    assert_eq!(log.lock().unwrap().len(), 1);
    eprintln!("server effect + duplicate call ID: requests=1; caller reconciliation required");
    agent.shutdown().await.unwrap();
    server.abort();
}
