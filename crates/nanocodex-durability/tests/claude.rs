//! Claude recovery through loopback Messages HTTP and a reopened SQLite store.
//! Failure cases defined before implementation: terminal receipt replay after
//! restart; completed tool receipts after a failed provider continuation; live
//! cancellation after an effect starts (unknown outcome, never dispatched again);
//! opaque compaction suffixes and container/discovery state across reopen.
//! Pending effects after a process crash follow the store's at-least-once policy.
//! Synthetic models have no catalog limits; configure their output budget explicitly.
//! The 4,096-token fixture budget is also asserted across checkpoint recovery.
#![cfg(all(feature = "claude", feature = "sqlite"))]

use axum::{Json, Router, response::IntoResponse, routing::post};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat};
use nanocodex_agent::{Nanocodex, PromptRequest};
use nanocodex_claude::{Claude, ClaudeClient, ToolDefinition};
use nanocodex_durability::{DurableAgentExt, DurableSession, SqliteStore, StepStatus};
use serde_json::{Value, json};
use std::{
    io::Cursor,
    sync::{Arc, Mutex},
};

fn sse(blocks: Vec<Value>, stop: &str, input: u64) -> String {
    let mut frames = vec![
        json!({"type":"message_start","message":{"id":"synthetic-response","role":"assistant","model":"test","content":[],"usage":{"input_tokens":input,"output_tokens":0},"container":{"id":"stable-container"}}}),
    ];
    for (index, block) in blocks.into_iter().enumerate() {
        frames.push(json!({"type":"content_block_start","index":index,"content_block":block}));
        frames.push(json!({"type":"content_block_stop","index":index}));
    }
    frames.push(
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":5}}),
    );
    frames.push(json!({"type":"message_stop"}));
    frames
        .into_iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect()
}
fn text(value: &str) -> Vec<Value> {
    vec![json!({"type":"text","text":value})]
}
fn invalid_server_boundary(body: &Value) -> bool {
    let mut unresolved = std::collections::HashSet::new();
    for message in body["messages"].as_array().unwrap() {
        for block in message["content"].as_array().unwrap() {
            if message["role"] == "user" && block["type"] != "tool_result" && !unresolved.is_empty()
            {
                return true;
            }
            if block["type"] == "server_tool_use" || block["type"] == "mcp_tool_use" {
                unresolved.insert(block["id"].as_str().unwrap());
            } else if block["type"] != "tool_result"
                && let Some(id) = block["tool_use_id"].as_str()
                && !unresolved.remove(id)
            {
                return true;
            }
        }
    }
    false
}

async fn server(
    respond: impl Fn(usize, &Value) -> String + Send + Sync + 'static,
) -> (
    ClaudeClient,
    Arc<Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    let respond = Arc::new(respond);
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let log = log.clone();
            let respond = respond.clone();
            async move {
                let index = {
                    let mut log = log.lock().unwrap();
                    log.push(body.clone());
                    log.len()
                };
                if std::env::var_os("NANOCLAUDE_DURABILITY_TRACE").is_some() {
                    eprintln!("{}", json!({"request_index":index,"request":body}));
                }
                if invalid_server_boundary(&body) {
                    return (
                        axum::http::StatusCode::BAD_REQUEST,
                        "invalid server tool boundary",
                    )
                        .into_response();
                }
                (
                    [("content-type", "text/event-stream")],
                    respond(index, &body),
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "synthetic",
        ),
        requests,
        task,
    )
}
async fn reopen(path: &std::path::Path) -> DurableSession {
    DurableSession::open(SqliteStore::open(path).unwrap(), "claude-synthetic")
        .await
        .unwrap()
}
fn tool() -> ToolDefinition {
    ToolDefinition {
        name: "effect".into(),
        description: "Synthetic effect".into(),
        input_schema: json!({"type":"object"}),
        strict: None,
        defer_loading: false,
    }
}
fn png_block(width: u32, height: u32) -> Value {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(width, height)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":STANDARD.encode(bytes.into_inner())}})
}

#[tokio::test]
async fn completed_request_receipt_replays_after_sqlite_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) =
        server(|_, _| sse(text("recorded answer"), "end_turn", 12)).await;
    let request = || PromptRequest::new("remember constraint A").request_id("stable-request");
    let mut usage = None;
    for _ in 0..2 {
        let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
            .max_tokens(4096)
            .durability(reopen(&path).await)
            .await
            .unwrap()
            .build()
            .unwrap();
        let result = agent
            .prompt(request())
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        assert_eq!(result.final_message(), "recorded answer");
        let current_usage = result.usage().unwrap().total_tokens();
        assert_eq!(*usage.get_or_insert(current_usage), current_usage);
        let conflict = match agent
            .prompt(PromptRequest::new("different input").request_id("stable-request"))
            .await
        {
            Ok(turn) => turn.result().await.map(|_| ()),
            Err(error) => Err(error),
        };
        assert!(
            conflict.is_err(),
            "an existing request identity must reject different input"
        );
        agent.shutdown().await.unwrap();
        drop((agent, events));
    }
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "receipt replay must not call the provider"
    );
    server.abort();
}

fn signed_round() -> Vec<Value> {
    vec![
        json!({"type":"thinking","thinking":"authorized effects","signature":"opaque-signature","binding":{"opaque":"must survive"}}),
        json!({"type":"redacted_thinking","data":"opaque-redacted","binding":"unchanged"}),
        json!({"type":"tool_use","id":"effect-once","name":"effect","input":{"key":"a"},"caller":{"type":"direct"}}),
    ]
}

#[tokio::test]
async fn retried_model_call_receipt_replays_after_sqlite_reopen() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) = server(|index, request| {
        if index == 1 {
            "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n".into()
        } else if request["messages"].to_string().contains("tool_result") {
            sse(text("completed after retry"), "end_turn", 10)
        } else {
            sse(signed_round(), "tool_use", 10)
        }
    })
    .await;
    let effects = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let counter = effects.clone();
        let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
            .max_tokens(4096)
            .tool(tool(), move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Ok("committed once".into()) }
            })
            .durability(reopen(&path).await)
            .await
            .unwrap()
            .build()
            .unwrap();
        let result = agent
            .prompt(PromptRequest::new("perform effect once").request_id("retried-request"))
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        assert_eq!(result.final_message(), "completed after retry");
        assert_eq!(result.usage().unwrap().total_tokens(), 30);
        agent.shutdown().await.unwrap();
        drop((agent, events));
    }
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 3, "replay must not call the provider");
    assert_eq!(log[0], log[1], "a retry resends the admitted request");
    server.abort();
}

#[tokio::test]
async fn completed_effect_and_opaque_compaction_suffix_survive_reopen() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let fresh = vec![
        json!({"type":"thinking","thinking":"reconcile receipt","signature":"replacement-prefix"}),
        json!({"type":"text","text":"recovered"}),
    ];
    let recovered = fresh.clone();
    let (client, requests, server) = server(move |index, _| match index {
        1 => sse(signed_round(), "tool_use", 70_000),
        2 => sse(text("Preserve the original task."), "end_turn", 10),
        3 => "data: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"synthetic followup failure\"}}\n\n".into(),
        4 => sse(recovered.clone(), "end_turn", 10),
        _ => sse(text("reviewed"), "end_turn", 10),
    }).await;
    let effects = Arc::new(AtomicUsize::new(0));
    let receipt = vec![
        json!({"type":"text","text":"effect committed"}),
        png_block(1, 1),
    ];
    for (prompt, expected) in [
        ("perform effect once", None),
        ("reconcile existing receipt", Some("recovered")),
        ("review after restart", Some("reviewed")),
    ] {
        let counter = effects.clone();
        let returned = receipt.clone();
        let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
            .max_tokens(4096)
            .auto_compact_window_tokens(100_000)
            .tool_blocks(tool(), move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                let returned = returned.clone();
                async move { Ok(returned) }
            })
            .durability(reopen(&path).await)
            .await
            .unwrap()
            .build()
            .unwrap();
        let result = agent
            .prompt(PromptRequest::new(prompt).request_id(prompt))
            .await
            .unwrap()
            .result()
            .await;
        match expected {
            Some(expected) => assert_eq!(result.unwrap().final_message(), expected),
            None => assert!(result.is_err()),
        }
        agent.shutdown().await.unwrap();
        drop((agent, events));
    }
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 5);
    assert_eq!(
        effects.load(Ordering::SeqCst),
        1,
        "completed effects must not repeat after reopen"
    );
    assert_eq!(
        log[2]["messages"][1]["content"],
        json!(&signed_round()[2..])
    );
    assert_eq!(
        log[2]["messages"][2]["content"][0]["content"],
        json!(receipt)
    );
    assert_eq!(
        &log[3]["messages"].as_array().unwrap()[..3],
        log[2]["messages"].as_array().unwrap()
    );
    assert_eq!(
        log[4]["messages"][4]["content"],
        json!(fresh),
        "reasoning received after compaction remains replayable after reopen"
    );
    assert_eq!(log[3]["container"], "stable-container");
    assert_eq!(log[0]["tools"], log[3]["tools"]);
    server.abort();
}

#[tokio::test]
async fn cancelled_started_tool_publishes_one_unknown_terminal_event() {
    use nanocodex_agent::events::AgentEventKind;
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) = server(|index, _| match index {
        1 => sse(signed_round(), "tool_use", 10),
        _ => sse(text("unreachable after cancellation"), "end_turn", 10),
    })
    .await;
    let effects = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let (counter, notify) = (effects.clone(), started.clone());
    let (agent, mut events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .tool(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            notify.notify_one();
            std::future::pending::<Result<String, String>>()
        })
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let turn = agent
        .prompt(PromptRequest::new("perform effect once").request_id("terminal-event"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    turn.cancel().await.unwrap();
    assert!(turn.result().await.is_err());
    let mut lifecycle = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        let ended = matches!(
            event.kind,
            AgentEventKind::RunFailed | AgentEventKind::RunCompleted
        );
        if matches!(
            event.kind,
            AgentEventKind::ToolCall | AgentEventKind::ToolResult
        ) {
            lifecycle.push((
                event.kind == AgentEventKind::ToolResult,
                serde_json::from_str::<Value>(event.payload.get()).unwrap(),
            ));
        }
        if ended {
            break;
        }
    }
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(100), events.recv()).await
    {
        assert!(
            !matches!(
                event.kind,
                AgentEventKind::ToolCall | AgentEventKind::ToolResult
            ),
            "no lifecycle event may follow the run end"
        );
    }
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(
        lifecycle.len(),
        2,
        "one start and one terminal event: {lifecycle:?}"
    );
    let ((call_is_result, call), (result_is_result, result)) = (&lifecycle[0], &lifecycle[1]);
    assert!(!call_is_result && *result_is_result);
    assert_eq!(call["call_id"], "effect-once");
    assert_eq!(result["call_id"], "effect-once");
    assert_eq!(result["tool"], "effect");
    assert_eq!(result["turn_id"], call["turn_id"]);
    assert_eq!(result["status"], "cancelled");
    assert_eq!(result["outcome_unknown"], true);
    assert!(result["duration_ns"].is_u64());
    // The terminal event describes uncertainty; it must not claim "no effect".
    let text = result["result"]["text"].as_str().unwrap();
    assert!(text.contains("outcome unknown") && text.contains("Do not assume it did not run"));
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "cancellation stops the model loop"
    );
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
}

#[tokio::test]
async fn live_interrupted_effect_is_unknown_after_compaction_and_reopen() {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) = server(|index, _| match index {
        1 => sse(signed_round(), "tool_use", 10),
        2 => sse(text("The task requested one effect."), "end_turn", 10),
        _ => sse(text("reconciled"), "end_turn", 10),
    })
    .await;
    let effects = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let counter = effects.clone();
    let notify = started.clone();
    let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .max_tokens(4096)
        .tool(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            notify.notify_one();
            std::future::pending::<Result<String, String>>()
        })
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let turn = agent
        .prompt(PromptRequest::new("perform effect once").request_id("interrupted"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    turn.cancel().await.unwrap();
    assert!(turn.result().await.is_err());
    agent.compact().await.unwrap();
    agent.shutdown().await.unwrap();
    drop((agent, events));
    let counter = effects.clone();
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .tool(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("must not repeat".into()) }
        })
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    agent
        .prompt(PromptRequest::new("reconcile uncertainty").request_id("reconcile"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let log = requests.lock().unwrap().clone();
    assert_eq!(log.len(), 3);
    assert_eq!(
        log[2]["messages"][1]["content"],
        json!(&signed_round()[2..])
    );
    let unknown = &log[2]["messages"][2]["content"][0];
    assert_eq!(unknown["tool_use_id"], "effect-once");
    assert_eq!(unknown["is_error"], true);
    assert!(
        unknown["content"]
            .as_str()
            .unwrap()
            .contains("outcome unknown")
    );
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
}

#[tokio::test]
async fn discovery_and_container_survive_restart_then_compaction_requires_rediscovery() {
    use nanocodex_claude::{ClaudeToolReply, ClaudeTools, ToolResultContent};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) = server(|index, _| match index {
        1 => sse(vec![json!({"type":"tool_use","id":"discover","name":"ToolSearch","input":{"query":"select:effect","max_results":1}})], "tool_use", 10),
        3 | 6 => sse(vec![json!({"type":"tool_use","id":format!("effect-{index}"),"name":"effect","input":{}})], "tool_use", 10),
        _ => sse(text("done"), "end_turn", 10),
    }).await;
    let effects = Arc::new(AtomicUsize::new(0));
    for phase in 0..3 {
        let mut deferred = tool();
        deferred.defer_loading = true;
        let counter = effects.clone();
        let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
            .max_tokens(4096)
            .client_tool_search()
            .tools_factory(|_| {
                let mut search = tool();
                search.name = "ToolSearch".into();
                search.defer_loading = false;
                Ok(ClaudeTools::new().custom_tool_search().tool_with_context(
                    search,
                    |_, _| async {
                        // Preserve a pre-fix/custom discovery receipt in SQLite.
                        Ok(ClaudeToolReply::success(ToolResultContent::Blocks(vec![
                            json!({"type":"tool_reference","tool_name":"effect"}),
                            json!({"type":"text","text":"retained discovery details"}),
                        ])))
                    },
                ))
            })
            .tool(deferred, move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Ok("receipt".into()) }
            })
            .durability(reopen(&path).await)
            .await
            .unwrap()
            .build()
            .unwrap();
        let result = agent
            .prompt(
                PromptRequest::new(
                    ["discover effect", "use discovery", "try stale discovery"][phase],
                )
                .request_id(format!("phase-{phase}")),
            )
            .await
            .unwrap()
            .result()
            .await;
        if phase == 2 {
            assert!(result.unwrap_err().to_string().contains("before discovery"));
        } else {
            result.unwrap();
        }
        if phase == 1 {
            agent.compact().await.unwrap();
        }
        agent.shutdown().await.unwrap();
        drop((agent, events));
    }
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 6);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    for request in &*log {
        for message in request["messages"].as_array().unwrap() {
            for receipt in message["content"].as_array().unwrap() {
                if let Some(blocks) = receipt["content"].as_array()
                    && blocks.iter().any(|b| b["type"] == "tool_reference")
                {
                    assert!(blocks.iter().all(|b| b["type"] == "tool_reference"));
                }
            }
        }
    }
    assert!(
        log[2]["messages"]
            .to_string()
            .contains("retained discovery details")
    );
    assert!(log[2]["messages"].to_string().contains("tool_reference"));
    assert!(!log[5]["messages"].to_string().contains("tool_reference"));
    assert_eq!(log[2]["container"], "stable-container");
    assert!(
        log.iter()
            .all(|request| request["tools"] == log[0]["tools"])
    );
    server.abort();
}

#[tokio::test]
async fn server_interruption_notice_survives_lossy_summary_and_sqlite_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) = server(|index, _| match index {
        1 => {
            // Remote execution may have happened, but the stream ends before a
            // completed message. No assistant/result block may be invented.
            let mut body = String::new();
            for frame in [
                json!({"type":"message_start","message":{"id":"interrupted-server","role":"assistant","model":"test","content":[],"usage":{"input_tokens":10,"output_tokens":0},"container":{"id":"stable-container"}}}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"unknown-server-effect","name":"bash_code_execution","input":{"command":"synthetic mutation"}}}),
                json!({"type":"content_block_stop","index":0}),
            ] { body.push_str(&format!("data: {frame}\n\n")); }
            body
        },
        2 => sse(text("The user requested a synthetic mutation."), "end_turn", 10),
        _ => sse(text("reconciled"), "end_turn", 10),
    }).await;
    let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .max_tokens(4096)
        .server_tool(nanocodex_claude::ServerToolDefinition::code_execution_current())
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    assert!(
        agent
            .prompt(
                PromptRequest::new("perform server effect once").request_id("server-interrupted")
            )
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    agent.compact().await.unwrap();
    agent.shutdown().await.unwrap();
    drop((agent, events));
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .server_tool(nanocodex_claude::ServerToolDefinition::code_execution_current())
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    agent
        .prompt(PromptRequest::new("reconcile before continuing").request_id("server-reconcile"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = requests.lock().unwrap().clone();
    assert_eq!(
        log.len(),
        3,
        "interrupted remote execution must not retry automatically"
    );
    let messages = log[2]["messages"].as_array().unwrap();
    let notice = messages
        .iter()
        .flat_map(|message| message["content"].as_array().unwrap())
        .filter_map(|block| block["text"].as_str())
        .find(|text| text.contains("outcome unknown"))
        .expect("uncertainty must survive a summary that omits the notice and a SQLite reopen");
    assert!(notice.contains("unknown-server-effect"));
    assert!(notice.contains("automatically repeat"));
    assert!(
        !messages
            .iter()
            .any(|message| message["role"] == "assistant")
    );
    assert!(!log[2]["messages"].to_string().contains("tool_result"));
    assert_eq!(log[2]["container"], "stable-container");
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
}

struct FaultStore {
    inner: SqliteStore,
    writes: Arc<std::sync::atomic::AtomicUsize>,
    fail_at: Option<usize>,
    after_commit: bool,
    fail_when_armed: Option<Arc<std::sync::atomic::AtomicBool>>,
}
impl nanocodex_durability::StateStore for FaultStore {
    fn read_record<'a>(
        &'a mut self,
        state_id: &'a str,
        key: &'a str,
    ) -> nanocodex_durability::StoreFuture<
        'a,
        Result<Option<String>, nanocodex_durability::StoreError>,
    > {
        self.inner.read_record(state_id, key)
    }
    fn acquire<'a>(
        &'a mut self,
        id: &'a str,
        owner: nanocodex_durability::OwnerId,
    ) -> nanocodex_durability::StoreFuture<
        'a,
        Result<nanocodex_durability::OwnedState, nanocodex_durability::StoreError>,
    > {
        self.inner.acquire(id, owner)
    }
    fn replace<'a>(
        &'a mut self,
        id: &'a str,
        owner: &'a nanocodex_durability::OwnerToken,
        revision: u64,
        payload: &'a str,
        records: &'a [nanocodex_durability::StoreRecord],
    ) -> nanocodex_durability::StoreFuture<'a, Result<u64, nanocodex_durability::StoreError>> {
        use std::sync::atomic::Ordering;
        Box::pin(async move {
            let ordinal = self.writes.fetch_add(1, Ordering::SeqCst);
            if self.fail_at == Some(ordinal)
                || self
                    .fail_when_armed
                    .as_ref()
                    .is_some_and(|armed| armed.swap(false, Ordering::SeqCst))
            {
                if self.after_commit {
                    self.inner
                        .replace(id, owner, revision, payload, records)
                        .await?;
                    return Err(nanocodex_durability::StoreError::Backend(
                        "synthetic lost commit acknowledgement".into(),
                    ));
                }
                return Err(nanocodex_durability::StoreError::NotCommitted(
                    "synthetic precommit interruption".into(),
                ));
            }
            self.inner
                .replace(id, owner, revision, payload, records)
                .await
        })
    }
}

#[derive(Clone, Copy)]
enum CompactionJourney {
    Automatic,
    ContextRecovery,
    ExhaustionAfterRecovery,
    OutputExhaustion,
}

async fn transaction_recovery(
    fail_at: Option<usize>,
    after_commit: bool,
    journey: CompactionJourney,
) -> usize {
    let context_exhaustion = matches!(
        journey,
        CompactionJourney::ContextRecovery | CompactionJourney::ExhaustionAfterRecovery
    );
    let output_exhaustion = matches!(journey, CompactionJourney::OutputExhaustion);
    let repeated_exhaustion = matches!(
        journey,
        CompactionJourney::ExhaustionAfterRecovery | CompactionJourney::OutputExhaustion
    );
    let terminal_error = if output_exhaustion {
        "after 3 continuations"
    } else {
        "context window exhausted after recovery"
    };
    use nanocodex_claude_tools::ClaudeTasks;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) = server(move |_, request| {
        if request["tool_choice"]["type"] == "none" {
            return sse(
                text("Retain the synthetic task and committed receipt."),
                "end_turn",
                10,
            );
        }
        let has_receipt = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["content"].as_array().unwrap())
            .any(|block| block["type"] == "tool_result");
        if context_exhaustion
            && has_receipt
            && !request["messages"].to_string().contains("completed-fetch")
        {
            sse(
                vec![
                    json!({"type":"thinking","thinking":"partial reasoning","signature":"signed-exhaustion"}),
                    json!({"type":"server_tool_use","id":"completed-fetch","name":"web_fetch","input":{"url":"https://example.org"}}),
                    json!({"type":"web_fetch_tool_result","tool_use_id":"completed-fetch","content":{"type":"web_fetch_result","url":"https://example.org","content":"page"}}),
                    json!({"type":"text","text":"partial answer"}),
                ],
                "model_context_window_exceeded",
                10,
            )
        } else if has_receipt {
            sse(
                text("completed exactly once"),
                if output_exhaustion { "max_tokens" } else if repeated_exhaustion { "model_context_window_exceeded" } else { "end_turn" },
                10,
            )
        } else {
            sse(signed_round(), if output_exhaustion { "max_tokens" } else { "tool_use" }, if context_exhaustion || output_exhaustion { 10 } else { 70_000 })
        }
    })
    .await;
    let effects = Arc::new(AtomicUsize::new(0));
    let writes = Arc::new(AtomicUsize::new(0));
    let state = DurableSession::open(
        FaultStore {
            inner: SqliteStore::open(&path).unwrap(),
            writes: writes.clone(),
            fail_at,
            after_commit,
            fail_when_armed: None,
        },
        "claude-synthetic",
    )
    .await
    .unwrap();
    let counter = effects.clone();
    let board = Arc::new(ClaudeTasks::new());
    let handler_board = board.clone();
    let request =
        || PromptRequest::new("complete one synthetic effect").request_id("transaction-request");
    let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .max_tokens(4096)
        .auto_compact_window_tokens(100_000)
        .server_tool(nanocodex_claude::ServerToolDefinition::web_fetch_basic(1))
        .tasks(board.clone())
        .tool(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            let board = handler_board.clone();
            async move {
                board
                    .execute(
                        "TaskCreate",
                        json!({"subject":"matrix task","description":"created once"}),
                    )
                    .await
            }
        })
        .durability(state)
        .await
        .unwrap()
        .build()
        .unwrap();
    let first = match agent.prompt(request()).await {
        Ok(turn) => turn.result().await,
        Err(error) => Err(error),
    };
    if fail_at.is_some() {
        assert!(
            first.is_err(),
            "injected write {fail_at:?}/{after_commit} must interrupt the first driver"
        );
    } else if repeated_exhaustion {
        assert!(first.unwrap_err().to_string().contains(terminal_error));
    } else {
        first.unwrap();
    }
    let operation_writes = writes.load(Ordering::SeqCst);
    let original_provider_requests = requests.lock().unwrap().len();
    let _ = agent.shutdown().await;
    drop((agent, events, board));
    let counter = effects.clone();
    let board = Arc::new(ClaudeTasks::new());
    let handler_board = board.clone();
    let mut builder = Nanocodex::builder(Claude::new(client, "changed-model"))
        .system("changed system after restart")
        .max_tokens(256)
        .automatic_cache(true)
        .adaptive_thinking()
        .auto_compact_window_tokens(50_000)
        .server_tool(nanocodex_claude::ServerToolDefinition::web_fetch_basic(1))
        .tasks(board.clone());
    if !after_commit || effects.load(Ordering::SeqCst) == 0 {
        builder = builder.tool(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            let board = handler_board.clone();
            async move {
                board
                    .execute(
                        "TaskCreate",
                        json!({"subject":"matrix task","description":"created once"}),
                    )
                    .await
            }
        });
    }
    let (agent, events) = builder
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let result = match agent.prompt(request()).await {
        Ok(turn) => turn.result().await,
        Err(error) => Err(error),
    };
    if repeated_exhaustion {
        let error = result.unwrap_err();
        assert!(
            error.to_string().contains(terminal_error),
            "recovery at {fail_at:?}/{after_commit}: {error}"
        );
    } else {
        assert_eq!(
            result
                .unwrap_or_else(|error| panic!("recovery at {fail_at:?}/{after_commit}: {error}"))
                .final_message(),
            "completed exactly once"
        );
    }
    if after_commit || fail_at.is_none() {
        assert_eq!(
            effects.load(Ordering::SeqCst),
            1,
            "committed tool receipt must prevent repeat at {fail_at:?}/{after_commit}"
        );
    } else {
        assert!(
            (1..=2).contains(&effects.load(Ordering::SeqCst)),
            "an uncommitted effect may execute at most once on each of two attempts"
        );
    }
    let listing: Value =
        serde_json::from_str(&board.execute("TaskList", json!({})).await.unwrap()).unwrap();
    assert_eq!(
        listing["tasks"].as_array().unwrap().len(),
        1,
        "task receipt and board must agree at {fail_at:?}/{after_commit}"
    );
    assert_eq!(listing["tasks"][0]["id"], "1");
    if original_provider_requests > 0 {
        let log = requests.lock().unwrap();
        for request in log.iter() {
            assert_eq!(
                request["model"], "test",
                "unfinished request must retain original model at {fail_at:?}/{after_commit}"
            );
            assert_eq!(request["max_tokens"], 4096);
            assert!(request.get("system").is_none());
            if context_exhaustion && request["tool_choice"]["type"] == "none" {
                assert_eq!(request["thinking"], json!({"type":"disabled"}));
            } else {
                assert!(request.get("thinking").is_none());
            }
            assert!(request.get("cache_control").is_none());
        }
    }
    let provider_calls = requests.lock().unwrap().len();
    if context_exhaustion {
        let log = requests.lock().unwrap();
        let continuation = log.last().unwrap()["messages"].to_string();
        assert!(!continuation.contains("signed-exhaustion"));
        assert!(continuation.contains("completed-fetch"));
        if after_commit || fail_at.is_none() {
            assert_eq!(log.len(), 4, "committed model responses must not repeat");
        }
    }
    if output_exhaustion {
        let log = requests.lock().unwrap();
        let continuation = log.last().unwrap()["messages"].to_string();
        assert!(continuation.contains("opaque-signature"));
        assert!(continuation.contains("tool_result"));
        if after_commit || fail_at.is_none() {
            assert_eq!(
                log.len(),
                4,
                "restart must retain the three-continuation cap and replay committed provider responses"
            );
        }
    }
    let replay = match agent.prompt(request()).await {
        Ok(turn) => turn.result().await,
        Err(error) => Err(error),
    };
    assert_eq!(replay.is_err(), repeated_exhaustion);
    assert_eq!(
        requests.lock().unwrap().len(),
        provider_calls,
        "recovered terminal receipt must replay"
    );
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
    operation_writes
}

#[tokio::test]
async fn every_sqlite_write_recovers_before_commit_and_after_lost_acknowledgement() {
    // Learn the write boundaries by running the public journey, without coupling
    // fault positions to private state layouts or hard-coded revision numbers.
    let count = transaction_recovery(None, false, CompactionJourney::Automatic).await;
    for after_commit in [false, true] {
        for ordinal in 0..count {
            transaction_recovery(Some(ordinal), after_commit, CompactionJourney::Automatic).await;
        }
    }
}

#[tokio::test]
async fn context_exhaustion_recovers_across_every_sqlite_write() {
    for journey in [
        CompactionJourney::ContextRecovery,
        CompactionJourney::ExhaustionAfterRecovery,
    ] {
        let count = transaction_recovery(None, false, journey).await;
        for after_commit in [false, true] {
            for ordinal in 0..count {
                transaction_recovery(Some(ordinal), after_commit, journey).await;
            }
        }
    }
}

#[tokio::test]
async fn image_receipt_stays_original_while_replayed_history_is_bounded() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) = server(|index, _| match index {
        1 => sse(signed_round(), "tool_use", 10),
        _ => sse(text("image receipt restored"), "end_turn", 10),
    })
    .await;
    let request = || PromptRequest::new("capture a synthetic image once").request_id("capture");
    let receipt = vec![
        json!({"type":"text","text":"capture committed"}),
        png_block(9001, 1),
    ];
    let lost_ack = Arc::new(AtomicBool::new(false));
    let effects = Arc::new(AtomicUsize::new(0));
    let state = DurableSession::open(
        FaultStore {
            inner: SqliteStore::open(&path).unwrap(),
            writes: Arc::new(AtomicUsize::new(0)),
            fail_at: None,
            after_commit: true,
            fail_when_armed: Some(lost_ack.clone()),
        },
        "claude-synthetic",
    )
    .await
    .unwrap();
    let build = |state| {
        let arm = lost_ack.clone();
        let counter = effects.clone();
        let returned = receipt.clone();
        Nanocodex::builder(Claude::new(client.clone(), "test"))
            .max_tokens(4096)
            .tool_blocks(tool(), move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                // The next durability write records this completed receipt.
                // Commit it, then lose its acknowledgement before batch advance.
                arm.store(true, Ordering::SeqCst);
                let returned = returned.clone();
                async move { Ok(returned) }
            })
            .durability(state)
    };
    let (agent, events) = build(state).await.unwrap().build().unwrap();
    assert!(
        agent
            .prompt(request())
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    let _ = agent.shutdown().await;
    drop((agent, events));

    let state = reopen(&path).await;
    let retained = state.state().await.unwrap();
    let StepStatus::Completed(output) =
        &retained.operation("capture").unwrap().steps["tool-0-effect-once"].status
    else {
        panic!("the image receipt must commit before the lost acknowledgement");
    };
    let stored: Value = state.resolve(output).await.unwrap().decode().unwrap();
    assert_eq!(
        stored["result"]["content"],
        json!(receipt),
        "the durable receipt keeps the handler's exact output"
    );
    let (agent, events) = build(state).await.unwrap().build().unwrap();
    assert_eq!(
        agent
            .prompt(request())
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "image receipt restored"
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let log = requests.lock().unwrap().clone();
    assert_eq!(log.len(), 2);
    let replayed = &log[1]["messages"][2]["content"][0]["content"];
    assert_eq!(replayed[0], receipt[0]);
    let prepared = STANDARD
        .decode(replayed[1]["source"]["data"].as_str().unwrap())
        .unwrap();
    let prepared = image::load_from_memory(&prepared).unwrap();
    assert_eq!(
        (prepared.width(), prepared.height()),
        (3000, 1),
        "replayed provider history carries the bounded image"
    );
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
}

#[tokio::test]
async fn output_exhaustion_cap_and_completed_effect_survive_every_sqlite_write() {
    let count = transaction_recovery(None, false, CompactionJourney::OutputExhaustion).await;
    for after_commit in [false, true] {
        for ordinal in 0..count {
            transaction_recovery(
                Some(ordinal),
                after_commit,
                CompactionJourney::OutputExhaustion,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn completed_task_mutation_replays_without_handler_into_reconstructed_board() {
    use nanocodex_claude_tools::ClaudeTasks;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) = server(|index, _| match index {
        1 => sse(signed_round(), "tool_use", 10),
        _ => sse(text("task receipt restored"), "end_turn", 10),
    })
    .await;
    let request = || PromptRequest::new("create a synthetic task once").request_id("task-create");
    let lost_ack = Arc::new(AtomicBool::new(false));
    let effects = Arc::new(AtomicUsize::new(0));
    let board = Arc::new(ClaudeTasks::new());
    let handler_board = board.clone();
    let arm = lost_ack.clone();
    let counter = effects.clone();
    let state = DurableSession::open(
        FaultStore {
            inner: SqliteStore::open(&path).unwrap(),
            writes: Arc::new(AtomicUsize::new(0)),
            fail_at: None,
            after_commit: true,
            fail_when_armed: Some(lost_ack),
        },
        "claude-synthetic",
    )
    .await
    .unwrap();
    let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .max_tokens(4096)
        .tasks(board.clone())
        .tool(tool(), move |_| {
            let board = handler_board.clone();
            let arm = arm.clone();
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let receipt = board.execute("TaskCreate", json!({"subject":"durable synthetic task","description":"preserve committed task and ID"})).await?;
                // The next durability write records this completed tool receipt.
                // Commit it, then lose its acknowledgement before batch advance.
                arm.store(true, Ordering::SeqCst);
                Ok(receipt)
            }
        })
        .durability(state).await.unwrap().build().unwrap();
    assert!(
        agent
            .prompt(request())
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let _ = agent.shutdown().await;
    drop((agent, events, board));

    let restored_board = Arc::new(ClaudeTasks::new());
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        // The effect handler is intentionally absent from this fresh host.
        .tasks(restored_board.clone())
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt(request())
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "task receipt restored"
    );
    let listing: Value =
        serde_json::from_str(&restored_board.execute("TaskList", json!({})).await.unwrap())
            .unwrap();
    assert_eq!(listing["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(listing["tasks"][0]["subject"], "durable synthetic task");
    assert_eq!(listing["tasks"][0]["id"], "1");
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let log = requests.lock().unwrap().clone();
    assert_eq!(
        log.len(),
        2,
        "completed model and tool receipts both replay without redispatch"
    );
    assert_eq!(
        log[1]["tools"], log[0]["tools"],
        "unfinished request uses the frozen catalog even when a host handler is removed"
    );
    let receipt: Value = serde_json::from_str(
        log[1]["messages"][2]["content"][0]["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["task"]["id"], "1");
    assert_ne!(log[1]["messages"][2]["content"][0]["is_error"], true);
    let next: Value = serde_json::from_str(
        &restored_board
            .execute(
                "TaskCreate",
                json!({"subject":"next task","description":"new task"}),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        next["task"]["id"], "2",
        "task ID watermark must be restored with the receipt"
    );
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
}

async fn blocked_provider_owner_journey(fence: bool) {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().route("/v1/messages", post({
        let started = started.clone();
        let release = release.clone();
        let log = requests.clone();
        move |Json(body): Json<Value>| {
            let started = started.clone();
            let release = release.clone();
            let log = log.clone();
            async move {
                let index = { let mut log = log.lock().unwrap(); log.push(body.clone()); log.len() };
                if std::env::var_os("NANOCLAUDE_DURABILITY_TRACE").is_some() {
                    eprintln!("{}", json!({"scenario":if fence {"stale-owner"} else {"detached-client"},"request_index":index,"request":body}));
                }
                if index == 1 { started.notify_one(); release.notified().await; }
                let body = if body["messages"].to_string().contains("tool_result") {
                    sse(text("owner completed"), "end_turn", 10)
                } else { sse(signed_round(), "tool_use", 10) };
                ([("content-type", "text/event-stream")], body).into_response()
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(reqwest::Client::new(), endpoint, "synthetic");
    let old_effects = Arc::new(AtomicUsize::new(0));
    let counter = old_effects.clone();
    let state = reopen(&path).await;
    let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .max_tokens(4096)
        .tool(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("old host receipt".into()) }
        })
        .durability(state.clone())
        .await
        .unwrap()
        .build()
        .unwrap();
    let request =
        || PromptRequest::new("one effect while client waits").request_id("blocked-owner");
    let turn = agent.prompt(request()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    if fence {
        let new_effects = Arc::new(AtomicUsize::new(0));
        let counter = new_effects.clone();
        let (recovered, recovered_events) =
            Nanocodex::builder(Claude::new(client, "changed-host-model"))
                .max_tokens(4096)
                .system("changed-host-system")
                .tool(tool(), move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async { Ok("new host receipt".into()) }
                })
                .durability(reopen(&path).await)
                .await
                .unwrap()
                .build()
                .unwrap();
        assert_eq!(
            recovered
                .prompt(request())
                .await
                .unwrap()
                .result()
                .await
                .unwrap()
                .final_message(),
            "owner completed"
        );
        release.notify_one();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), turn.result())
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(
            old_effects.load(Ordering::SeqCst),
            0,
            "a fenced owner must not dispatch a tool from its late model response"
        );
        assert_eq!(new_effects.load(Ordering::SeqCst), 1);
        assert_eq!(
            recovered
                .prompt(request())
                .await
                .unwrap()
                .result()
                .await
                .unwrap()
                .final_message(),
            "owner completed",
            "late stale-owner completion must not alter the current owner's terminal receipt"
        );
        let log = requests.lock().unwrap().clone();
        assert_eq!(log.len(), 3);
        assert_eq!(
            log[0], log[1],
            "pending provider attempt must resend the frozen request after ownership transfer"
        );
        recovered.shutdown().await.unwrap();
        drop((recovered, recovered_events));
    } else {
        drop(turn);
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if state
                    .state()
                    .await
                    .unwrap()
                    .operation("blocked-owner")
                    .is_some_and(|operation| {
                        matches!(
                            operation.status,
                            nanocodex_durability::OperationStatus::Completed { .. }
                        )
                    })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(old_effects.load(Ordering::SeqCst), 1);
        agent.shutdown().await.unwrap();
        let (recovered, recovered_events) = Nanocodex::builder(Claude::new(client, "test"))
            .max_tokens(4096)
            .durability(reopen(&path).await)
            .await
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            recovered
                .prompt(request())
                .await
                .unwrap()
                .result()
                .await
                .unwrap()
                .final_message(),
            "owner completed"
        );
        assert_eq!(
            requests.lock().unwrap().len(),
            2,
            "detached caller must still leave a durable receipt for replay"
        );
        recovered.shutdown().await.unwrap();
        drop((recovered, recovered_events));
    }
    let _ = agent.shutdown().await;
    drop((agent, events));
    server.abort();
}

#[tokio::test]
async fn detached_client_still_commits_effect_and_replayable_receipt() {
    blocked_provider_owner_journey(false).await;
}

#[tokio::test]
async fn fenced_owner_cannot_dispatch_late_response_and_new_owner_replays_frozen_request() {
    blocked_provider_owner_journey(true).await;
}

// P1 recovery regressions: committed receipts must be reconciled before an
// admission-time cancellation; an absent host capability must remain recoverable;
// dropping a lifecycle caller must not strand an accepted operation or claim.
async fn pending_task_receipt_fixture() -> (
    tempfile::TempDir,
    ClaudeClient,
    Arc<Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    use nanocodex_claude_tools::ClaudeTasks;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) = server(|index, _| match index {
        1 => sse(signed_round(), "tool_use", 10),
        _ => sse(text("task reconciled"), "end_turn", 10),
    })
    .await;
    let armed = Arc::new(AtomicBool::new(false));
    let effects = Arc::new(AtomicUsize::new(0));
    let board = Arc::new(ClaudeTasks::new());
    let handler_board = board.clone();
    let arm = armed.clone();
    let counter = effects.clone();
    let state = DurableSession::open(
        FaultStore {
            inner: SqliteStore::open(&path).unwrap(),
            writes: Arc::new(AtomicUsize::new(0)),
            fail_at: None,
            after_commit: true,
            fail_when_armed: Some(armed),
        },
        "claude-synthetic",
    )
    .await
    .unwrap();
    let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .max_tokens(4096)
        .tasks(board)
        .tool(tool(), move |_| {
            let board = handler_board.clone(); let arm = arm.clone(); let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let receipt = board.execute("TaskCreate", json!({"subject":"committed before interruption","description":"must survive reconciliation"})).await?;
                arm.store(true, Ordering::SeqCst);
                Ok(receipt)
            }
        })
        .durability(state).await.unwrap().build().unwrap();
    assert!(
        agent
            .prompt(PromptRequest::new("create a durable task").request_id("pending-task"))
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let _ = agent.shutdown().await;
    drop((agent, events));
    (directory, client, requests, server, effects)
}

#[tokio::test]
async fn recovery_cancel_on_admission_preserves_committed_tool_and_task_receipt() {
    use nanocodex_claude_tools::ClaudeTasks;
    use std::sync::atomic::Ordering;
    let (directory, client, requests, server, effects) = pending_task_receipt_fixture().await;
    let path = directory.path().join("state.sqlite");
    let board = Arc::new(ClaudeTasks::new());
    let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .max_tokens(4096)
        .tasks(board.clone())
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let cancelled = agent
        .prompt(
            PromptRequest::new("create a durable task")
                .request_id("pending-task")
                .cancel_on_admission(),
        )
        .await;
    match cancelled {
        Ok(turn) => assert!(turn.result().await.is_err()),
        Err(error) => assert!(error.to_string().contains("cancel"), "{error}"),
    }
    let listing: Value =
        serde_json::from_str(&board.execute("TaskList", json!({})).await.unwrap()).unwrap();
    assert_eq!(
        listing["tasks"].as_array().unwrap().len(),
        1,
        "cancellation must restore committed task receipt before retiring pending work"
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "cancellation recovery must not call the provider"
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    agent.shutdown().await.unwrap();
    drop((agent, events, board));
    let board = Arc::new(ClaudeTasks::new());
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .tasks(board.clone())
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let listing: Value =
        serde_json::from_str(&board.execute("TaskList", json!({})).await.unwrap()).unwrap();
    assert_eq!(
        listing["tasks"][0]["id"], "1",
        "cancelled checkpoint must retain task state after another reopen"
    );
    agent
        .prompt(PromptRequest::new("review the committed receipt").request_id("review-task"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = requests.lock().unwrap().clone();
    assert_eq!(log[1]["messages"][1]["content"], json!(signed_round()));
    let receipt = &log[1]["messages"][2]["content"][0];
    assert_ne!(
        receipt["is_error"], true,
        "a committed receipt must not become outcome unknown"
    );
    let receipt: Value = serde_json::from_str(receipt["content"].as_str().unwrap()).unwrap();
    assert_eq!(receipt["task"]["id"], "1");
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
}

#[tokio::test]
async fn recovery_missing_task_board_leaves_pending_operation_recoverable() {
    use nanocodex_claude_tools::ClaudeTasks;
    use std::sync::atomic::Ordering;
    let (directory, client, requests, server, effects) = pending_task_receipt_fixture().await;
    let path = directory.path().join("state.sqlite");
    let unconfigured = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .max_tokens(4096)
        .durability(reopen(&path).await)
        .await;
    match unconfigured {
        Err(error) => assert!(error.to_string().contains("task"), "{error}"),
        Ok(builder) => match builder.build() {
            Err(error) => assert!(error.to_string().contains("task"), "{error}"),
            Ok((agent, events)) => {
                let error = match agent
                    .prompt(PromptRequest::new("create a durable task").request_id("pending-task"))
                    .await
                {
                    Ok(turn) => turn.result().await.unwrap_err(),
                    Err(error) => error,
                };
                assert!(error.to_string().contains("task"), "{error}");
                let _ = agent.shutdown().await;
                drop((agent, events));
            }
        },
    }
    let state = reopen(&path).await;
    assert!(
        matches!(
            state
                .state()
                .await
                .unwrap()
                .operation("pending-task")
                .unwrap()
                .status,
            nanocodex_durability::OperationStatus::Pending
        ),
        "missing host task board must not settle recoverable work as failed"
    );
    let board = Arc::new(ClaudeTasks::new());
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .tasks(board.clone())
        .durability(state)
        .await
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt(PromptRequest::new("create a durable task").request_id("pending-task"))
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "task reconciled"
    );
    let listing: Value =
        serde_json::from_str(&board.execute("TaskList", json!({})).await.unwrap()).unwrap();
    assert_eq!(listing["tasks"][0]["id"], "1");
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(requests.lock().unwrap().len(), 2);
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
}

struct AdmissionBarrierStore {
    inner: SqliteStore,
    pause_next: Arc<std::sync::atomic::AtomicBool>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl nanocodex_durability::StateStore for AdmissionBarrierStore {
    fn read_record<'a>(
        &'a mut self,
        id: &'a str,
        key: &'a str,
    ) -> nanocodex_durability::StoreFuture<
        'a,
        Result<Option<String>, nanocodex_durability::StoreError>,
    > {
        self.inner.read_record(id, key)
    }
    fn acquire<'a>(
        &'a mut self,
        id: &'a str,
        owner: nanocodex_durability::OwnerId,
    ) -> nanocodex_durability::StoreFuture<
        'a,
        Result<nanocodex_durability::OwnedState, nanocodex_durability::StoreError>,
    > {
        self.inner.acquire(id, owner)
    }
    fn replace<'a>(
        &'a mut self,
        id: &'a str,
        owner: &'a nanocodex_durability::OwnerToken,
        revision: u64,
        payload: &'a str,
        records: &'a [nanocodex_durability::StoreRecord],
    ) -> nanocodex_durability::StoreFuture<'a, Result<u64, nanocodex_durability::StoreError>> {
        Box::pin(async move {
            let next = self
                .inner
                .replace(id, owner, revision, payload, records)
                .await?;
            if self
                .pause_next
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                self.entered.notify_one();
                self.release.notified().await;
            }
            Ok(next)
        })
    }
}

async fn aborted_lifecycle_caller_journey(compaction: bool) {
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) = server(|_, request| {
        if request["tool_choice"]["type"] == "none" {
            sse(text("summary survives caller cancellation"), "end_turn", 10)
        } else {
            sse(text("accepted work completed"), "end_turn", 10)
        }
    })
    .await;
    let pause_next = Arc::new(AtomicBool::new(!compaction));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let state = DurableSession::open(
        AdmissionBarrierStore {
            inner: SqliteStore::open(&path).unwrap(),
            pause_next: pause_next.clone(),
            entered: entered.clone(),
            release: release.clone(),
        },
        "claude-synthetic",
    )
    .await
    .unwrap();
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .durability(state.clone())
        .await
        .unwrap()
        .build()
        .unwrap();
    if compaction {
        agent
            .prompt(PromptRequest::new("retain this task").request_id("seed"))
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        pause_next.store(true, Ordering::SeqCst);
    }
    let caller = agent.clone();
    let submitted = tokio::spawn(async move {
        if compaction {
            caller.compact().await.map(|_| ())
        } else {
            caller
                .prompt(
                    PromptRequest::new("accepted before caller disappears")
                        .request_id("aborted-admission"),
                )
                .await
                .map(|_| ())
        }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    assert_eq!(
        requests.lock().unwrap().len(),
        usize::from(compaction),
        "caller must be aborted during admission, before provider dispatch"
    );
    submitted.abort();
    assert!(submitted.await.unwrap_err().is_cancelled());
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let retained = state.state().await.unwrap();
            if retained.pending_operations().is_empty()
                && requests.lock().unwrap().len() == 1 + usize::from(compaction)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("accepted lifecycle operation must complete after its awaiting caller is aborted");
    if compaction {
        agent
            .prompt(PromptRequest::new("continue from summary").request_id("after-compact"))
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        assert!(
            requests.lock().unwrap()[2]["messages"]
                .to_string()
                .contains("summary survives caller cancellation"),
            "detached compaction must commit its replacement context"
        );
    } else {
        assert_eq!(
            agent
                .prompt(
                    PromptRequest::new("accepted before caller disappears")
                        .request_id("aborted-admission")
                )
                .await
                .unwrap()
                .result()
                .await
                .unwrap()
                .final_message(),
            "accepted work completed"
        );
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "aborted admission caller must leave a replayable receipt, not an active stranded claim"
        );
    }
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
}

#[tokio::test]
async fn aborted_admission_future_does_not_strand_accepted_operation() {
    aborted_lifecycle_caller_journey(false).await;
}

#[tokio::test]
async fn aborted_compaction_future_does_not_strand_claim_or_context_swap() {
    aborted_lifecycle_caller_journey(true).await;
}

// P2 lifecycle regressions: summary streams are cancellable owned work; manual
// compaction interrupts an active turn before taking its safe context boundary.
async fn cancelled_summary_reopens_safely(context_exhaustion: bool) {
    use axum::body::Body;
    use futures_util::{StreamExt, stream};
    use std::{convert::Infallible, time::Duration};
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let summary_started = Arc::new(tokio::sync::Notify::new());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().route("/v1/messages", post({
        let summary_started = summary_started.clone();
        let requests = requests.clone();
        move |Json(body): Json<Value>| {
            let summary_started = summary_started.clone();
            let requests = requests.clone();
            async move {
                requests.lock().unwrap().push(body.clone());
                if std::env::var_os("NANOCLAUDE_DURABILITY_TRACE").is_some() {
                    eprintln!("{}", json!({"scenario":"stalled-summary-shutdown","request":body}));
                }
                if body["tool_choice"]["type"] == "none" {
                    let chunks = stream::once(async move {
                        summary_started.notify_one();
                        Ok::<_, Infallible>(format!("data: {}\n\ndata: {}\n\n",
                            json!({"type":"message_start","message":{"id":"stalled-summary","role":"assistant","model":"test","content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
                            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"partial summary must not commit"}})))
                    }).chain(stream::pending());
                    return ([("content-type", "text/event-stream")], Body::from_stream(chunks)).into_response();
                }
                let stop = if context_exhaustion && requests.lock().unwrap().len() == 1 {
                    "model_context_window_exceeded"
                } else {
                    "end_turn"
                };
                ([("content-type", "text/event-stream")], sse(text("original retained answer"), stop, 10)).into_response()
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(reqwest::Client::new(), endpoint, "synthetic");
    let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .max_tokens(4096)
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let turn = agent
        .prompt(PromptRequest::new("retain the seed constraint").request_id("summary-seed"))
        .await
        .unwrap();
    let summary = if context_exhaustion {
        tokio::spawn(async move { turn.result().await.map(|_| ()) })
    } else {
        turn.result().await.unwrap();
        let compacting = agent.clone();
        tokio::spawn(async move { compacting.compact().await.map(|_| ()) })
    };
    tokio::time::timeout(Duration::from_secs(3), summary_started.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), agent.shutdown())
        .await
        .expect(
            "shutdown must cancel an owned stalled summary before waiting for its admission lock",
        )
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), summary)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    drop((agent, events));
    let state = reopen(&path).await;
    let retained = state.state().await.unwrap();
    assert!(
        retained.pending_operations().is_empty(),
        "interrupted compaction must settle safely instead of blocking reopen"
    );
    assert!(
        retained.operations().values().any(|operation| matches!(
            operation.status,
            nanocodex_durability::OperationStatus::Cancelled { .. }
        )),
        "shutdown must durably record the stalled compaction as cancelled"
    );
    drop(retained);
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .durability(state)
        .await
        .unwrap()
        .build()
        .unwrap();
    agent
        .prompt(PromptRequest::new("continue after stopped summary").request_id("summary-recovery"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = requests.lock().unwrap().clone();
    assert_eq!(
        log.len(),
        3,
        "reopen must not resume the cancelled summary stream"
    );
    let continuation = log[2]["messages"].to_string();
    assert!(continuation.contains("retain the seed constraint"));
    assert!(continuation.contains("original retained answer"));
    assert!(!continuation.contains("partial summary must not commit"));
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
}

#[tokio::test]
async fn stalled_manual_summary_is_cancelled_by_shutdown_and_reopens_safely() {
    cancelled_summary_reopens_safely(false).await;
}

#[tokio::test]
async fn context_exhaustion_summary_cancellation_retains_output_across_reopen() {
    cancelled_summary_reopens_safely(true).await;
}

#[tokio::test]
async fn manual_compaction_cancels_active_tool_then_preserves_safe_context_on_reopen() {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) = server(|index, request| {
        if request["tool_choice"]["type"] == "none" {
            sse(text("The task requested one effect."), "end_turn", 10)
        } else if index == 1 {
            sse(signed_round(), "tool_use", 10)
        } else {
            sse(text("reconciled after compaction"), "end_turn", 10)
        }
    })
    .await;
    let started = Arc::new(tokio::sync::Notify::new());
    let notify = started.clone();
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .max_tokens(4096)
        .tool(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            notify.notify_one();
            std::future::pending::<Result<String, String>>()
        })
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let turn = agent
        .prompt(PromptRequest::new("perform the effect once").request_id("active-before-compact"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), agent.compact())
        .await
        .expect("manual compact must cancel the active turn before waiting for its context")
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), turn.result())
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(requests.lock().unwrap().len(), 2);
    agent.shutdown().await.unwrap();
    drop((agent, events));
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    agent
        .prompt(
            PromptRequest::new("reconcile the interrupted effect")
                .request_id("after-active-compact"),
        )
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = requests.lock().unwrap().clone();
    assert_eq!(log.len(), 3);
    assert_eq!(
        log[2]["messages"][1]["content"],
        json!(&signed_round()[2..])
    );
    let receipt = &log[2]["messages"][2]["content"][0];
    assert_eq!(receipt["tool_use_id"], "effect-once");
    assert_eq!(receipt["is_error"], true);
    assert!(
        receipt["content"]
            .as_str()
            .unwrap()
            .contains("outcome unknown")
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
}

// Terminal failures and cancellations turn unresolved native calls into bounded
// evidence. Real SQLite reopen, cancelled new input, lossy summary and exact
// terminal receipt replay must neither resurrect that input nor repeat effects.
#[tokio::test]
async fn uncertain_paused_server_turn_survives_compaction_cancellation_and_sqlite_reopen() {
    use axum::{body::Body, http::StatusCode};
    use futures_util::{StreamExt, stream};
    use nanocodex_agent::events::AgentEventKind;
    use std::{
        convert::Infallible,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    for cancel_active in [false, true] {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("paused-state.sqlite");
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let log = requests.clone();
        let effects = Arc::new(AtomicUsize::new(0));
        let counter = effects.clone();
        let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
            let log = log.clone();
            let counter = counter.clone();
            async move {
                let index = { let mut rows = log.lock().unwrap(); rows.push(body.clone()); rows.len() };
                if std::env::var_os("NANOCLAUDE_DURABILITY_TRACE").is_some() {
                    eprintln!("{}", json!({"cancel_active":cancel_active,"request_index":index,"request":body}));
                }
                if invalid_server_boundary(&body) {
                    return (StatusCode::BAD_REQUEST, "invalid server tool boundary").into_response();
                }
                // Increment only after admission, and on EVERY native replay.
                // This detects accidental repeats as well as unexpected HTTP.
                if body["tool_choice"]["type"] != "none" && body["messages"].as_array().unwrap().iter()
                    .flat_map(|m| m["content"].as_array().unwrap())
                    .any(|b| b["type"] == "server_tool_use" && b["id"] == "paused-mutation") {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
                if index == 3 {
                    let partial = [
                        json!({"type":"message_start","message":{"id":"uncertain-resume","role":"assistant","model":"test","content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
                        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"continuation admitted"}}),
                    ].into_iter().map(|frame| format!("data: {frame}\n\n")).collect::<String>();
                    if cancel_active {
                        return ([("content-type","text/event-stream")], Body::from_stream(
                            stream::once(async { Ok::<_, Infallible>(partial) }).chain(stream::pending())
                        )).into_response();
                    }
                    return ([("content-type","text/event-stream")], partial).into_response();
                }
                let output = match index {
                    1 => sse(vec![
                        json!({"type":"thinking","thinking":"perform the authorized mutation","signature":"opaque-paused-signature"}),
                        json!({"type":"server_tool_use","id":"paused-mutation","name":"bash_code_execution","input":{"command":"synthetic mutation"},"opaque":"retain-evidence"}),
                        json!({"type":"text","text":"多字節 provider evidence ".repeat(8_000)}),
                    ], "pause_turn", 70_000),
                    2 => sse(text("Perform the authorized synthetic operation."), "end_turn", 10),
                    4 => sse(text("A deliberately lossy summary."), "end_turn", 10),
                    _ => sse(text("reconciled current request"), "end_turn", 10),
                };
                ([("content-type","text/event-stream")], output).into_response()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "synthetic",
        );
        let first_request = || PromptRequest::new("perform operation").request_id("paused-first");
        let cancelled_request = || {
            PromptRequest::new("CANCELLED_USER_REQUEST never execute this")
                .request_id("cancelled-new")
        };
        let final_request = || {
            PromptRequest::new("CURRENT_USER_REQUEST reconcile before acting")
                .request_id("after-uncertain-reopen")
        };
        let (agent, mut events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
            .max_tokens(4096)
            .auto_compact_window_tokens(100_000)
            .server_tool(nanocodex_claude::ServerToolDefinition::code_execution_current())
            .durability(reopen(&path).await)
            .await
            .unwrap()
            .build()
            .unwrap();
        let turn = agent.prompt(first_request()).await.unwrap();
        if cancel_active {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let event = events.next().await.unwrap();
                    if event.kind == AgentEventKind::AssistantDelta {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            turn.cancel().await.unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(3), turn.result())
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(requests.lock().unwrap().len(), 3);
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        agent.shutdown().await.unwrap();
        drop((agent, events));

        // Reopen before any new input. Replaying the terminal ID cannot resume
        // the failed/cancelled continuation or issue another native effect.
        let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
            .max_tokens(4096)
            .server_tool(nanocodex_claude::ServerToolDefinition::code_execution_current())
            .durability(reopen(&path).await)
            .await
            .unwrap()
            .build()
            .unwrap();
        assert!(
            agent
                .prompt(first_request())
                .await
                .unwrap()
                .result()
                .await
                .is_err()
        );
        assert!(
            agent
                .prompt(cancelled_request().cancel_on_admission())
                .await
                .unwrap()
                .result()
                .await
                .is_err()
        );
        assert_eq!(
            requests.lock().unwrap().len(),
            3,
            "terminal replay and cancelled new input must not reach HTTP"
        );
        agent.compact().await.unwrap();
        assert_eq!(requests.lock().unwrap().len(), 4);
        agent.shutdown().await.unwrap();
        drop((agent, events));

        let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
            .max_tokens(4096)
            .server_tool(nanocodex_claude::ServerToolDefinition::code_execution_current())
            .durability(reopen(&path).await)
            .await
            .unwrap()
            .build()
            .unwrap();
        let result = agent
            .prompt(final_request())
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        assert_eq!(result.final_message(), "reconciled current request");
        let usage = result.usage().unwrap().total_tokens();
        agent.shutdown().await.unwrap();
        drop((agent, events));

        let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
            .max_tokens(4096)
            .server_tool(nanocodex_claude::ServerToolDefinition::code_execution_current())
            .durability(reopen(&path).await)
            .await
            .unwrap()
            .build()
            .unwrap();
        let replay = agent
            .prompt(final_request())
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        assert_eq!(replay.final_message(), "reconciled current request");
        assert_eq!(replay.usage().unwrap().total_tokens(), usage);
        assert!(
            agent
                .prompt(cancelled_request())
                .await
                .unwrap()
                .result()
                .await
                .is_err()
        );
        agent.shutdown().await.unwrap();
        drop((agent, events));

        let log = requests.lock().unwrap();
        assert_eq!(log.len(), 5, "terminal receipt replay must not call HTTP");
        assert_eq!(
            effects.load(Ordering::SeqCst),
            1,
            "uncertain effects must not repeat"
        );
        assert_eq!(log[2]["messages"][1]["content"][0]["id"], "paused-mutation");
        assert!(!log[1]["messages"].to_string().contains("paused-mutation"));
        for request in &log[3..] {
            assert!(
                request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|m| m["role"] == "user"),
                "uncertain transcript must be data, without native calls or fabricated results"
            );
            let evidence = request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|m| m["content"].as_array().unwrap())
                .filter_map(|b| b["text"].as_str())
                .find(|text| text.contains("paused-mutation"))
                .expect("prior server evidence");
            assert!(evidence.contains("outcome unknown"));
            assert!(!evidence.contains("opaque-paused-signature"));
            assert!(evidence.contains("retain-evidence"));
            assert!(evidence.contains("provider transcript truncated"));
            assert!(evidence.len() <= 66_000, "bounded UTF-8 evidence");
            assert!(
                !request["messages"]
                    .to_string()
                    .contains("CANCELLED_USER_REQUEST")
            );
        }
        assert_eq!(
            log[4]["messages"]
                .to_string()
                .matches("CURRENT_USER_REQUEST")
                .count(),
            1
        );
        server.abort();
    }
}

// A store failure leaves the operation unfinished, unlike a provider failure.
// Reopen must use the prepared native cursor: settled model receipts replay,
// while an admitted effect with no committed receipt remains at least once.
#[tokio::test]
async fn paused_server_cursor_replays_across_store_failure_without_terminalizing() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    for after_commit in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pending-server.sqlite");
        let armed = Arc::new(AtomicBool::new(false));
        let arm = armed.clone();
        let effects = Arc::new(AtomicUsize::new(0));
        let counter = effects.clone();
        let (client, requests, server) = server(move |_, body| {
            let has_pending_call = body["messages"].as_array().unwrap().iter()
                .flat_map(|m| m["content"].as_array().unwrap())
                .any(|b| b["type"] == "server_tool_use" && b["id"] == "durable-pause");
            if has_pending_call {
                counter.fetch_add(1, Ordering::SeqCst);
                arm.store(true, Ordering::SeqCst);
                sse(vec![
                    json!({"type":"bash_code_execution_tool_result","tool_use_id":"durable-pause","content":{"type":"bash_code_execution_result","stdout":"committed","stderr":"","return_code":0,"content":[]}}),
                    json!({"type":"text","text":"recovered prepared server turn"}),
                ], "end_turn", 10)
            } else {
                sse(vec![
                    json!({"type":"thinking","thinking":"run once","signature":"durable-signature"}),
                    json!({"type":"server_tool_use","id":"durable-pause","name":"bash_code_execution","input":{"command":"synthetic effect"}}),
                ], "pause_turn", 10)
            }
        }).await;
        let state = DurableSession::open(
            FaultStore {
                inner: SqliteStore::open(&path).unwrap(),
                writes: Arc::new(AtomicUsize::new(0)),
                fail_at: None,
                after_commit,
                fail_when_armed: Some(armed),
            },
            "claude-synthetic",
        )
        .await
        .unwrap();
        let request =
            || PromptRequest::new("perform durable server operation").request_id("pending-server");
        let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "original-model"))
            .max_tokens(4096)
            .server_tool(nanocodex_claude::ServerToolDefinition::code_execution_current())
            .durability(state)
            .await
            .unwrap()
            .build()
            .unwrap();
        let error = agent
            .prompt(request())
            .await
            .unwrap()
            .result()
            .await
            .unwrap_err();
        assert!(error.execution_policy_disposition().is_some(), "{error}");
        assert_eq!(requests.lock().unwrap().len(), 2);
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        let _ = agent.shutdown().await;
        drop((agent, events));

        let (agent, events) = Nanocodex::builder(Claude::new(client, "different-model"))
            .max_tokens(4096)
            .durability(reopen(&path).await)
            .await
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            agent
                .prompt(request())
                .await
                .unwrap()
                .result()
                .await
                .unwrap()
                .final_message(),
            "recovered prepared server turn"
        );
        let expected = if after_commit { 1 } else { 2 };
        assert_eq!(effects.load(Ordering::SeqCst), expected);
        assert_eq!(requests.lock().unwrap().len(), 1 + expected);
        agent
            .prompt(request())
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        assert_eq!(
            requests.lock().unwrap().len(),
            1 + expected,
            "terminal replay must not execute again"
        );
        assert_eq!(effects.load(Ordering::SeqCst), expected);
        agent.shutdown().await.unwrap();
        drop((agent, events));
        let log = requests.lock().unwrap();
        if !after_commit {
            assert_eq!(
                log[1], log[2],
                "unfinished operation must replay its original frozen native request"
            );
        }
        assert!(
            log.iter()
                .all(|request| request["model"] == "original-model")
        );
        server.abort();
    }
}

// Compatibility fixture for a version-1 terminal failed checkpoint emitted
// before failure finalization converted unresolved server calls. Seed it through
// the public store API, then exercise the real provider boundary after reopen.
#[tokio::test]
async fn legacy_failed_server_snapshot_accepts_new_input_without_native_replay() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-failure.sqlite");
    let session = reopen(&path).await;
    session
        .admit("legacy-failed", &json!({"legacy":"fetch once"}))
        .await
        .unwrap();
    session.begin_attempt("legacy-failed").await.unwrap();
    session.fail("legacy-failed", &json!({
        "provider":"claude", "version":1, "discovered":[], "tasks":null,
        "conversation":{
            "admitted_tool_ids":[], "recovery_notices":[],
            "messages":[
                {"role":"user","content":[{"type":"text","text":"original authorized fetch"}]},
                {"role":"assistant","content":[
                    {"type":"thinking","thinking":"fetch once","signature":"legacy-signed-evidence"},
                    {"type":"server_tool_use","id":"legacy-pending-fetch","name":"web_fetch","input":{"url":"https://example.org"}}
                ]}
            ],
            "summary":"", "active_context_tokens":70_000, "pending_continuation":true,
            "auto_compaction_suppressed":true, "rapid_compactions":1, "rounds_since_compaction":0,
            "previous_message_id":"legacy-pause", "container":"legacy-container"
        }
    }), "synthetic rejected continuation").await.unwrap();
    drop(session);
    let (client, requests, server) =
        server(|_, _| sse(text("legacy state reconciled"), "end_turn", 10)).await;
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .server_tool(nanocodex_claude::ServerToolDefinition::web_fetch_basic(1))
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("NEW_USER_REQUEST reconcile old fetch")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "legacy state reconciled"
    );
    agent.shutdown().await.unwrap();
    drop((agent, events));
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 1);
    let messages = &log[0]["messages"];
    assert!(
        messages
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] == "user")
    );
    assert!(messages.to_string().contains("legacy-pending-fetch"));
    assert!(messages.to_string().contains("legacy-signed-evidence"));
    assert!(messages.to_string().contains("outcome unknown"));
    assert_eq!(messages.to_string().matches("NEW_USER_REQUEST").count(), 1);
    assert_eq!(log[0]["container"], "legacy-container");
    server.abort();
}

/// Tool-only host policies must not silently add lifecycle journal writes.
/// Exercise public builders, SQLite journals, native dispatch and actual HTTP.
#[tokio::test]
async fn tool_only_policies_preserve_default_wire_and_durable_write_count() {
    use nanocodex_claude::{
        ClaudeHookFuture, ClaudeToolDecision, ClaudeToolHooks, ClaudeToolInvocation,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct ToolPolicy(Arc<AtomicUsize>);
    impl ClaudeToolHooks for ToolPolicy {
        fn before<'a>(
            &'a self,
            _: &'a str,
            _: &'a Value,
            _: &'a ClaudeToolInvocation,
        ) -> ClaudeHookFuture<'a, std::result::Result<ClaudeToolDecision, String>> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(ClaudeToolDecision::Allow)
            })
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let (client, requests, server) = server(|index, _| {
        if index % 2 == 1 {
            sse(
                vec![json!({"type":"tool_use","id":"default-effect","name":"effect","input":{}})],
                "tool_use",
                10,
            )
        } else {
            sse(text("default-policy-answer"), "end_turn", 10)
        }
    })
    .await;
    let mut observations = Vec::new();
    for policies in [0, 4] {
        let session = DurableSession::open(
            SqliteStore::open(directory.path().join(format!("policies-{policies}.sqlite")))
                .unwrap(),
            "default-policy-session",
        )
        .await
        .unwrap();
        let invoked = Arc::new(AtomicUsize::new(0));
        let mut builder = Nanocodex::builder(Claude::new(client.clone(), "test"))
            .max_tokens(4096)
            .tool(tool(), |_| async { Ok("real-tool-receipt".into()) });
        for _ in 0..policies {
            builder = builder.tool_hooks(Arc::new(ToolPolicy(invoked.clone())));
        }
        let (agent, events) = builder
            .durability(session.clone())
            .await
            .unwrap()
            .build()
            .unwrap();
        let result = agent
            .prompt(PromptRequest::new("default host journey").request_id("default-request"))
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        assert_eq!(result.final_message(), "default-policy-answer");
        agent.shutdown().await.unwrap();
        drop((agent, events));
        let journal = session.state().await.unwrap();
        assert_eq!(
            journal.operations().len(),
            1,
            "tool-only host added a lifecycle operation"
        );
        assert_eq!(invoked.load(Ordering::SeqCst), policies);
        observations.push(json!({"tool_policies":policies,"revision":journal.revision(),"operations":journal.operations().keys().collect::<Vec<_>>()}));
    }
    assert_eq!(
        observations[0]["revision"], observations[1]["revision"],
        "tool-only policies added lifecycle writes"
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[..2],
        requests[2..],
        "tool-only lifecycle defaults changed provider wire"
    );
    let artifact = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/lifecycle-default-journal.json");
    std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    std::fs::write(&artifact, serde_json::to_vec_pretty(&json!({"observations":observations,"provider_requests":*requests,"observed":"identical wire and revision count; four before policies still executed"})).unwrap()).unwrap();
    eprintln!("default lifecycle journal evidence: {}", artifact.display());
    server.abort();
}

/// Public turn API + actual Messages HTTP + reopened SQLite receipt journey.
#[tokio::test]
async fn identified_steering_receipts_withdrawal_and_recovery() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    use futures_util::StreamExt;
    use std::time::Duration;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steering.sqlite");
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let recovered_started = Arc::new(tokio::sync::Notify::new());
    let recovered_release = Arc::new(tokio::sync::Notify::new());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().route("/v1/messages", post({
        let recovered_started = recovered_started.clone();
        let recovered_release = recovered_release.clone();
        let started = started.clone(); let release = release.clone(); let requests = requests.clone();
        move |Json(body): Json<Value>| {
            let recovered_started = recovered_started.clone();
            let recovered_release = recovered_release.clone();
            let started = started.clone(); let release = release.clone(); let requests = requests.clone();
            async move {
                let index = { let mut log = requests.lock().unwrap(); log.push(body.clone()); log.len() };
                eprintln!("{}", json!({"scenario":"identified-steering","request_index":index,"request":body}));
                if index == 1 { started.notify_one(); release.notified().await; }
                if index == 2 { recovered_started.notify_one(); recovered_release.notified().await; }
                ([("content-type", "text/event-stream")], sse(text("steering completed"), "end_turn", 10)).into_response()
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(reqwest::Client::new(), endpoint, "synthetic");
    let state = reopen(&path).await;
    let (agent, mut events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .max_tokens(4096)
        .durability(state.clone())
        .await
        .unwrap()
        .build()
        .unwrap();
    let turn = agent
        .prompt(PromptRequest::new("original").request_id("identified-turn"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    turn.steer_with_id("keep".into(), "keep constraint")
        .await
        .unwrap();
    turn.steer_with_id("keep".into(), "keep constraint")
        .await
        .unwrap();
    assert!(
        turn.steer_with_id("keep".into(), "conflicting constraint")
            .await
            .is_err()
    );
    turn.steer_with_id("remove".into(), "withdraw constraint")
        .await
        .unwrap();
    assert!(!turn.withdraw_steer("keep".into()).await.unwrap());
    assert!(turn.withdraw_steer("remove".into()).await.unwrap());
    assert!(
        turn.steer_with_id("remove".into(), "withdraw constraint")
            .await
            .is_err()
    );
    // Fence the blocked owner and replay the unfinished operation with a real reopened store.
    let recovered_state = reopen(&path).await;
    let (recovered, mut recovered_events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .durability(recovered_state.clone())
        .await
        .unwrap()
        .build()
        .unwrap();
    assert!(
        turn.steer_with_id("fenced-new".into(), "must not be accepted")
            .await
            .is_err(),
        "fenced owner cannot accept fresh input"
    );
    let recovered_turn = recovered
        .prompt(PromptRequest::new("original").request_id("identified-turn"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), recovered_started.notified())
        .await
        .unwrap();
    recovered_turn
        .steer_with_id("keep".into(), "keep constraint")
        .await
        .unwrap();
    assert!(
        recovered_turn
            .steer_with_id("keep".into(), "different after restart")
            .await
            .is_err()
    );
    assert!(
        recovered_turn
            .steer_with_id("remove".into(), "withdraw constraint")
            .await
            .is_err()
    );
    recovered_release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), recovered_turn.result())
            .await
            .unwrap()
            .unwrap()
            .final_message(),
        "steering completed"
    );
    let journal = recovered_state.state().await.unwrap();
    let operation = journal.operation("identified-turn").unwrap();
    assert_eq!(operation.steer_receipts.len(), 2);
    assert!(!operation.steer_receipts["keep"].withdrawn);
    assert!(operation.steer_receipts["remove"].withdrawn);
    let transcript = requests.lock().unwrap().clone();
    assert_eq!(transcript.len(), 3);
    let final_request = transcript.last().unwrap()["messages"].to_string();
    assert_eq!(final_request.matches("keep constraint").count(), 1);
    assert!(!final_request.contains("withdraw constraint"));
    let mut markers = Vec::new();
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(50), recovered_events.next()).await
    {
        if event.kind == nanocodex_agent::events::AgentEventKind::RunSteered {
            markers.push(serde_json::from_str::<Value>(event.payload.get()).unwrap());
        }
    }
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0]["message_id"], "keep");
    eprintln!(
        "{}",
        json!({"scenario":"identified-steering","receipts":operation.steer_receipts,"consumed_markers":markers,"outcome":"dedup conflict withdrawal reopen all passed"})
    );
    release.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), turn.result())
            .await
            .unwrap()
            .is_err()
    );
    recovered.shutdown().await.unwrap();
    let _ = agent.shutdown().await;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(10), events.next()).await {}
    server.abort();
}

/// The native CLI uses this same public turn API without a durability policy.
#[tokio::test]
async fn native_identified_steering_without_durability_policy() {
    use futures_util::StreamExt;
    use std::time::Duration;
    let (client, requests, server) = server(|index, _| {
        if index == 1 {
            sse(signed_round(), "tool_use", 10)
        } else {
            sse(text("native steer completed"), "end_turn", 10)
        }
    })
    .await;
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (agent, mut events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .tool(tool(), {
            let started = started.clone();
            let release = release.clone();
            move |_| {
                let started = started.clone();
                let release = release.clone();
                async move {
                    started.notify_one();
                    release.notified().await;
                    Ok("native effect completed".into())
                }
            }
        })
        .build()
        .unwrap();
    let turn = agent.prompt("native original prompt").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    turn.steer_with_id("native-keep".into(), "native keep constraint")
        .await
        .unwrap();
    turn.steer_with_id("native-keep".into(), "native keep constraint")
        .await
        .unwrap();
    assert!(
        turn.steer_with_id("native-keep".into(), "native conflicting input")
            .await
            .is_err()
    );
    turn.steer_with_id("native-remove".into(), "native withdraw constraint")
        .await
        .unwrap();
    assert!(!turn.withdraw_steer("native-keep".into()).await.unwrap());
    assert!(turn.withdraw_steer("native-remove".into()).await.unwrap());
    assert!(
        turn.steer_with_id("native-remove".into(), "native withdraw constraint")
            .await
            .is_err()
    );
    release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), turn.result())
            .await
            .unwrap()
            .unwrap()
            .final_message(),
        "native steer completed"
    );
    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let final_input = requests[1]["messages"].to_string();
    assert_eq!(final_input.matches("native keep constraint").count(), 1);
    assert!(!final_input.contains("native withdraw constraint"));
    let mut markers = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(50), events.next()).await
    {
        if event.kind == nanocodex_agent::events::AgentEventKind::RunSteered {
            markers.push(serde_json::from_str::<Value>(event.payload.get()).unwrap());
        }
    }
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0]["message_id"], "native-keep");
    eprintln!(
        "{}",
        json!({"scenario":"native-identified-steering-no-policy","requests":requests,"consumed_markers":markers,"outcome":"identified admission dedup conflict withdrawal marker all passed"})
    );
    agent.shutdown().await.unwrap();
    server.abort();
}

/// Synthetic legacy continuation in the actual SQLite store, recovered through HTTP.
#[tokio::test]
async fn legacy_model_zero_steer_reaches_first_new_boundary_without_repeating_write() {
    use futures_util::StreamExt;
    use nanocodex_durability::{OwnerId, StateStore, StoreRecord};
    use sha2::{Digest, Sha256};
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-steering.sqlite");
    let old_started = Arc::new(tokio::sync::Notify::new());
    let old_release = Arc::new(tokio::sync::Notify::new());
    let resumed_started = Arc::new(tokio::sync::Notify::new());
    let resumed_release = Arc::new(tokio::sync::Notify::new());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().route(
        "/v1/messages",
        post({
            let old_started = old_started.clone();
            let old_release = old_release.clone();
            let resumed_started = resumed_started.clone();
            let resumed_release = resumed_release.clone();
            let requests = requests.clone();
            move |Json(body): Json<Value>| {
                let old_started = old_started.clone();
                let old_release = old_release.clone();
                let resumed_started = resumed_started.clone();
                let resumed_release = resumed_release.clone();
                let requests = requests.clone();
                async move {
                    let index = {
                        let mut log = requests.lock().unwrap();
                        log.push(body.clone());
                        log.len()
                    };
                    eprintln!(
                        "{}",
                        json!({"scenario":"legacy-model-zero","request_index":index,"request":body})
                    );
                    if index == 3 {
                        old_started.notify_one();
                        old_release.notified().await;
                    }
                    if index == 4 {
                        resumed_started.notify_one();
                        resumed_release.notified().await;
                    }
                    let response = if index == 1 {
                        sse(signed_round(), "tool_use", 10)
                    } else {
                        sse(text("legacy recovered"), "end_turn", 10)
                    };
                    ([("content-type", "text/event-stream")], response).into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(reqwest::Client::new(), endpoint, "synthetic");
    let writes = Arc::new(AtomicUsize::new(0));
    let counter = writes.clone();
    let (agent, _) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .max_tokens(4096)
        .tool(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("prior Write completed".into()) }
        })
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    agent
        .prompt(PromptRequest::new("perform prior Write").request_id("prior-write"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let old_turn = agent
        .prompt(PromptRequest::new("legacy active task").request_id("legacy-turn"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), old_started.notified())
        .await
        .unwrap();
    // Convert only this fixture's current continuation and pending effect into
    // the pre-receipt serialized format; prior completed Write history stays intact.
    let mut store = SqliteStore::open(&path).unwrap();
    let owned = store
        .acquire("claude-synthetic", OwnerId::new())
        .await
        .unwrap();
    let mut head: Value = serde_json::from_str(owned.state.payload.as_ref().unwrap()).unwrap();
    let operation = &mut head["nanocodex_durable_state"]["operations"]["legacy-turn"];
    let reference = operation["continuation"].as_str().unwrap();
    let record = store
        .read_record("claude-synthetic", reference)
        .await
        .unwrap()
        .unwrap();
    let mut cursor: Value = serde_json::from_str(record.strip_prefix('=').unwrap()).unwrap();
    cursor.as_object_mut().unwrap().remove("model_step_offset");
    cursor
        .as_object_mut()
        .unwrap()
        .remove("model_receipt_start");
    let cursor_json = serde_json::to_string(&cursor).unwrap();
    let key: String = Sha256::digest(cursor_json.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    operation["continuation"] = json!(key);
    let mut pending = operation["steps"]
        .as_object_mut()
        .unwrap()
        .remove("model-1")
        .unwrap();
    pending["kind"] = json!("model");
    operation["steps"]["model-0"] = pending;
    store
        .replace(
            "claude-synthetic",
            &owned.owner,
            owned.state.revision,
            &serde_json::to_string(&head).unwrap(),
            &[StoreRecord {
                key,
                value: format!("={cursor_json}"),
            }],
        )
        .await
        .unwrap();
    drop(store);
    let counter = writes.clone();
    let (recovered, mut events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .tool(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("unexpected repeated Write".into()) }
        })
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let turn = recovered
        .prompt(PromptRequest::new("legacy active task").request_id("legacy-turn"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), resumed_started.notified())
        .await
        .unwrap();
    turn.steer_with_id("legacy-correction".into(), "correct the legacy task now")
        .await
        .unwrap();
    resumed_release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), turn.result())
            .await
            .unwrap()
            .unwrap()
            .final_message(),
        "legacy recovered"
    );
    let transcript = requests.lock().unwrap().clone();
    assert_eq!(
        transcript.len(),
        5,
        "one replay and exactly one corrected model call"
    );
    assert!(
        transcript[4]["messages"]
            .to_string()
            .contains("correct the legacy task now")
    );
    assert_eq!(
        writes.load(Ordering::SeqCst),
        1,
        "prior Write must not be repeated"
    );
    let mut markers = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(50), events.next()).await
    {
        if event.kind == nanocodex_agent::events::AgentEventKind::RunSteered {
            markers.push(serde_json::from_str::<Value>(event.payload.get()).unwrap());
        }
    }
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0]["message_id"], "legacy-correction");
    eprintln!(
        "{}",
        json!({"scenario":"legacy-model-zero","model_requests":transcript.len(),"prior_write_count":writes.load(Ordering::SeqCst),"markers":markers,"outcome":"first new boundary corrected; prior Write retained"})
    );
    old_release.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), old_turn.result())
            .await
            .unwrap()
            .is_err()
    );
    recovered.shutdown().await.unwrap();
    let _ = agent.shutdown().await;
    server.abort();
}

// Public native Claude construction + installed tool dispatch against reopened
// SQLite. Journal fixtures deliberately distinguish absence, parse failure,
// incompatible version, and lifecycle loss from successful adoption.
#[tokio::test]
async fn native_claude_journal_adoption_directory_evidence() {
    use nanocodex_claude::ClaudeTools;
    use nanocodex_durability::{OwnerId, StateStore};
    use nanocodex_subagents::{channel, install_claude_tools};
    let child = |checkpoint: bool| {
        let mut entry = json!({
            "descriptor":{"id":1,"session_id":"fixture-child","role":"worker","task":"retained task","parent":null},
            "status":{"state": if checkpoint {"interrupted"} else {"running"}},
            "turn_in_flight":false,"output_schema":{"type":"string"}
        });
        if checkpoint {
            entry["native_checkpoint"] = json!({"model":"claude-haiku-4-5",
                "session_id":"fixture-child","thinking":"none",
                "payload":"{\"messages\":[]}","has_conversation":true});
        }
        entry
    };
    for (case, payload, expected_default, expected_all) in [
        ("absent", None, 0, 0),
        (
            "journal-not-attached",
            Some(json!({"version":1,"agents":[child(true)]}).to_string()),
            0,
            0,
        ),
        (
            "different-durable-id",
            Some(json!({"version":1,"agents":[child(true)]}).to_string()),
            0,
            0,
        ),
        (
            "recoverable",
            Some(json!({"version":1,"agents":[child(true)]}).to_string()),
            1,
            1,
        ),
        (
            "missing-checkpoint",
            Some(json!({"version":1,"agents":[child(false)]}).to_string()),
            0,
            1,
        ),
        ("invalid-json", Some("{broken".into()), 0, 0),
        (
            "unsupported-version",
            Some(json!({"version":999,"agents":[child(true)]}).to_string()),
            0,
            0,
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let original_payload = payload.clone();
        if let Some(payload) = payload {
            let mut store = SqliteStore::open(&path).unwrap();
            let key = "claude-synthetic:subagents";
            let owned = store.acquire(key, OwnerId::new()).await.unwrap();
            store
                .replace(key, &owned.owner, owned.state.revision, &payload, &[])
                .await
                .unwrap();
        }
        let restore_failed = matches!(case, "invalid-json" | "unsupported-version");
        let (client, requests, server) = server(move |index, _| match index {
            1 => sse(
                vec![
                    json!({"type":"tool_use","id":"default","name":"list_agents",
                "input":{"include_completed":false}}),
                ],
                "tool_use",
                12,
            ),
            2 => sse(
                vec![json!({"type":"tool_use","id":"all","name":"list_agents",
                "input":{"include_completed":true}})],
                "tool_use",
                12,
            ),
            3 if restore_failed => sse(vec![json!({
                "type":"tool_use", "id":"mutation", "name":"spawn_agent",
                "input":{"role":"worker", "task":"new task", "output_contract":{"kind":"string"},
                         "harness":null, "model":null, "thinking":null}
            })], "tool_use", 12),
            _ => sse(text("inspected"), "end_turn", 12),
        })
        .await;
        let (registry, control, _updates) = channel(6);
        let install = registry.clone();
        let durable = case != "journal-not-attached";
        let root = if case == "different-durable-id" {
            "other-root"
        } else {
            "claude-synthetic"
        };
        let mut builder = Nanocodex::builder(Claude::new(client.clone(), "test"))
            .max_tokens(4096)
            .session_id(root);
        if durable {
            builder = builder
                .durability(
                    DurableSession::open(SqliteStore::open(&path).unwrap(), root)
                        .await
                        .unwrap(),
                )
                .await
                .unwrap();
        }
        let (agent, events) = builder
            .tools_factory(move |handle| {
                assert_eq!(handle.session_id(), root);
                assert_eq!(
                    handle.child_journal().is_some(),
                    durable,
                    "journal attachment"
                );
                install_claude_tools(ClaudeTools::new(), handle, install.clone())
            })
            .build()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            agent
                .prompt("inspect directory")
                .await
                .unwrap()
                .result()
                .await
                .unwrap()
        })
        .await
        .unwrap();
        let captured = requests.lock().unwrap().clone();
        let result = |id: &str| -> Value {
            for message in captured.last().unwrap()["messages"].as_array().unwrap() {
                for block in message["content"].as_array().unwrap() {
                    if block["type"] == "tool_result" && block["tool_use_id"] == id {
                        let content = &block["content"];
                        let value = content.as_str().map(str::to_owned).unwrap_or_else(|| {
                            content
                                .as_array()
                                .unwrap()
                                .iter()
                                .filter_map(|b| b["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("")
                        });
                        if block["is_error"] == true {
                            return json!({"is_error":true,"error":value});
                        }
                        return serde_json::from_str(&value).unwrap();
                    }
                }
            }
            panic!("missing tool result {id}");
        };
        let default = result("default");
        let all = result("all");
        eprintln!(
            "ADOPTION_EVIDENCE {}",
            json!({"case":case,"root":root,"journal_attached":durable,"default":default,"all":all})
        );
        if restore_failed {
            let expected = if case == "invalid-json" {
                "invalid subagent journal"
            } else {
                "unsupported subagent journal version 999"
            };
            let mutation = result("mutation");
            for response in [&default, &all, &mutation] {
                assert_eq!(response["is_error"], true, "{case}: {response}");
                assert!(
                    response["error"].as_str().unwrap().contains(expected),
                    "{response}"
                );
            }
            assert_eq!(default["error"], all["error"]);
            assert_eq!(default["error"], mutation["error"]);
            let direct = registry
                .directory(root, true, false)
                .await
                .err()
                .expect("restoration must fail");
            assert!(direct.to_string().contains(expected));
            let close = registry
                .close(root, "1".parse().unwrap())
                .await
                .err()
                .expect("restoration must fail");
            assert_eq!(direct.to_string(), close.to_string());
            let close_all = control
                .close_all(root)
                .await
                .expect_err("restoration must fail");
            assert_eq!(direct.to_string(), close_all.to_string());
            control.cancel_all(root).await;
            eprintln!(
                "RESTORE_FAILURE_EVIDENCE {}",
                json!({"case":case,"mutation":mutation,"direct":direct.to_string(),"close":close.to_string(),"close_all":close_all.to_string()})
            );
        } else {
            assert_eq!(
                default["agents"].as_array().unwrap().len(),
                expected_default,
                "{case}"
            );
            assert_eq!(
                all["agents"].as_array().unwrap().len(),
                expected_all,
                "{case}"
            );
            if case == "missing-checkpoint" {
                assert_eq!(all["agents"][0]["status"]["state"], "failed");
            }
            if case == "recoverable" {
                assert_eq!(default["agents"][0]["status"]["state"], "interrupted");
            }
        }
        drop(captured);
        if matches!(
            case,
            "invalid-json"
                | "unsupported-version"
                | "journal-not-attached"
                | "different-durable-id"
        ) {
            // Read without acquiring ownership: a test must not fence a bad writer.
            let db = rusqlite::Connection::open(&path).unwrap();
            let retained: String = db
                .query_row(
                    "SELECT payload FROM nanocodex_durable_states WHERE state_id = ?1",
                    ["claude-synthetic:subagents"],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                Some(retained),
                original_payload,
                "untouched retained journal: {case}"
            );
        }
        if case == "recoverable" {
            // Public lifecycle mutation must reach SQLite before a fresh native
            // root can adopt it. Read without acquiring/fencing the active writer.
            registry.close(root, "1".parse().unwrap()).await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let db = rusqlite::Connection::open(&path).unwrap();
                    let payload: String = db
                        .query_row(
                            "SELECT payload FROM nanocodex_durable_states WHERE state_id = ?1",
                            ["claude-synthetic:subagents"],
                            |row| row.get(0),
                        )
                        .unwrap();
                    let saved: Value = serde_json::from_str(&payload).unwrap();
                    if saved["agents"][0]["status"]["state"] == "closed" {
                        eprintln!(
                            "PERSISTENCE_EVIDENCE {}",
                            json!({
                                "case":"native-close-saved", "journal":saved
                            })
                        );
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
        drop(events);
        drop(agent);
        drop(registry);
        if case == "recoverable" {
            let (restored, _control, _updates) = channel(6);
            let install = restored.clone();
            let (reopened, reopened_events) = Nanocodex::builder(Claude::new(client, "test"))
                .max_tokens(4096)
                .durability(
                    DurableSession::open(SqliteStore::open(&path).unwrap(), root)
                        .await
                        .unwrap(),
                )
                .await
                .unwrap()
                .tools_factory(move |handle| {
                    install_claude_tools(ClaudeTools::new(), handle, install.clone())
                })
                .build()
                .unwrap();
            let directory = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                restored.directory(root, true, false),
            )
            .await
            .unwrap()
            .unwrap();
            let directory = serde_json::to_value(directory).unwrap();
            eprintln!(
                "PERSISTENCE_EVIDENCE {}",
                json!({
                    "case":"fresh-native-root-after-close", "all":directory
                })
            );
            assert_eq!(directory.as_array().unwrap().len(), 1);
            assert_eq!(directory[0]["agent_id"], 1);
            assert_eq!(directory[0]["status"]["state"], "closed");
            assert!(
                restored
                    .directory(root, false, false)
                    .await
                    .unwrap()
                    .is_empty()
            );
            drop(reopened_events);
            drop(reopened);
        }
        server.abort();
    }
}

/// One store lists, reads, branches, and resumes Claude sessions, and a fork
/// of a durable Claude root persists as its own session with lineage.
#[tokio::test]
async fn claude_sessions_share_the_family_neutral_catalog() {
    use nanocodex_agent::{ClaudeModel, ForkRequest, HarnessFamily, HarnessModel, Origin};
    use nanocodex_durability::{BranchPoint, SessionRecord, SessionStore, TranscriptItem};
    let prompts = |transcript: &[TranscriptItem]| {
        transcript
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::User(text) => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let home = tempfile::tempdir().unwrap();
    let (client, requests, server) =
        server(|index, _| sse(text(&format!("claude reply {index}")), "end_turn", 12)).await;
    let model = ClaudeModel::Sonnet55;
    let store = SessionStore::open(home.path()).unwrap();
    let root_id = uuid::Uuid::now_v7().to_string();
    let (root, _events) = Nanocodex::builder(Claude::new(client.clone(), model.as_str()))
        .max_tokens(4096)
        .durability(
            store
                .session(SessionRecord::root(
                    root_id.clone(),
                    HarnessModel::Claude(model),
                    Some(home.path().to_path_buf()),
                ))
                .await
                .unwrap(),
        )
        .await
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(root.session_id(), root_id);
    for (id, prompt) in [("turn-1", "remember amber"), ("turn-2", "now say teal")] {
        root.prompt(PromptRequest::new(prompt).request_id(id))
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
    }
    let (fork, _fork_events) = root.fork(ForkRequest::latest()).await.unwrap();
    let fork_id = fork.session_id().to_owned();
    assert_eq!(fork.session().lineage.origin, Origin::Fork);
    assert_eq!(
        fork.persistence().and_then(|p| p.durable_state_id),
        Some(fork_id.clone()),
        "a durable Claude root's fork persists to its own state"
    );
    fork.prompt(PromptRequest::new("fork only").request_id("fork-1"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    fork.shutdown().await.unwrap();
    root.shutdown().await.unwrap();

    let listed = store.list().await.unwrap();
    assert_eq!(listed.len(), 2, "root and fork are listed");
    assert!(
        listed
            .iter()
            .all(|summary| summary.record.family() == HarnessFamily::Claude)
    );
    let loaded = store.load(&root_id).await.unwrap();
    assert_eq!(
        prompts(&loaded.transcript),
        ["remember amber", "now say teal"]
    );
    assert_eq!(loaded.summary.preview.as_deref(), Some("remember amber"));
    let stored_fork = store.load(&fork_id).await.unwrap();
    assert_eq!(stored_fork.summary.record.lineage.origin, Origin::Fork);
    assert_eq!(
        stored_fork
            .summary
            .record
            .lineage
            .parent_session_id
            .as_deref(),
        Some(root_id.as_str())
    );
    assert_eq!(
        prompts(&stored_fork.transcript),
        ["remember amber", "now say teal", "fork only"]
    );

    let branch = store
        .branch(&root_id, BranchPoint::Before("turn-2".into()), None)
        .await
        .unwrap();
    assert_eq!(branch.record.lineage.origin, Origin::Branch);
    let (resumed, _events) = Nanocodex::builder(Claude::new(client.clone(), model.as_str()))
        .max_tokens(4096)
        .durability(store.resume(&branch.record.session_id).await.unwrap())
        .await
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(resumed.session_id(), branch.record.session_id);
    resumed
        .prompt(PromptRequest::new("branch question").request_id("branch-1"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    resumed.shutdown().await.unwrap();
    let sent = requests.lock().unwrap().last().unwrap().to_string();
    assert!(sent.contains("remember amber") && sent.contains("branch question"));
    assert!(
        !sent.contains("now say teal"),
        "the branch drops the later turn"
    );
    server.abort();
}


/// Disconnecting a durable Claude client leaves its accepted turn running to a
/// committed result, refuses new work through every clone, and releases the
/// local owner so a reopened session replays the settled receipt.
#[tokio::test]
async fn disconnect_keeps_accepted_turn_and_releases_local_owner() {
    use std::{sync::atomic::{AtomicUsize, Ordering}, time::Duration};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) = server(|index, _| match index {
        1 => sse(signed_round(), "tool_use", 10),
        _ => sse(text("settled after disconnect"), "end_turn", 10),
    })
    .await;
    let effects = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (counter, notify, gate) = (effects.clone(), started.clone(), release.clone());
    let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .max_tokens(4096)
        .tool(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            notify.notify_one();
            let gate = gate.clone();
            async move {
                gate.notified().await;
                Ok("effect committed".into())
            }
        })
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let turn = agent
        .prompt(PromptRequest::new("perform effect once").request_id("detached"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    let clone = agent.clone();
    tokio::time::timeout(Duration::from_secs(5), agent.disconnect())
        .await
        .expect("disconnect does not wait for the accepted turn")
        .unwrap();
    assert!(
        clone.prompt("new work after disconnect").await.is_err(),
        "a disconnected durable session admits no new work"
    );
    release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .unwrap()
        .expect("the accepted turn is not cancelled by disconnect");
    assert_eq!(result.final_message(), "settled after disconnect");
    drop((agent, clone, events));
    let (reopened, events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .tool(tool(), |_| async { Ok("must not repeat".into()) })
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let replayed = reopened
        .prompt(PromptRequest::new("perform effect once").request_id("detached"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(replayed.final_message(), "settled after disconnect");
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(requests.lock().unwrap().len(), 2, "replay makes no provider call");
    reopened.shutdown().await.unwrap();
    drop((reopened, events));
    server.abort();
}



/// The family-neutral catalog reads a durable Claude session like a Codex one:
/// the transcript keeps visible reasoning and server tools (never signatures or
/// encrypted payloads), the portable checkpoint carries the session's real
/// model and thinking and restores a working session, and an old session stays
/// listed behind more than a list page of newer non-session states.
#[tokio::test]
async fn claude_catalog_checkpoint_transcript_and_listing_match_codex() {
    use nanocodex_agent::{ClaudeModel, HarnessFamily, HarnessModel, Thinking};
    use nanocodex_claude::ServerToolDefinition;
    use nanocodex_durability::{OwnerId, SessionRecord, SessionStore, StateStore, TranscriptItem};
    let home = tempfile::tempdir().unwrap();
    let searched = vec![
        json!({"type":"thinking","thinking":"weigh the sources","signature":"opaque-signature"}),
        json!({"type":"redacted_thinking","data":"opaque-redacted"}),
        json!({"type":"server_tool_use","id":"srv-1","name":"web_search","input":{"query":"nanocodex"}}),
        json!({"type":"web_search_tool_result","tool_use_id":"srv-1","content":[{"type":"web_search_result","title":"Nanocodex","url":"https://example.com/n","encrypted_content":"opaque-search"}]}),
        json!({"type":"text","text":"searched answer"}),
    ];
    let (client, requests, server) = server(move |index, _| match index {
        1 => sse(searched.clone(), "end_turn", 12),
        _ => sse(text("restored answer"), "end_turn", 12),
    })
    .await;
    let model = ClaudeModel::Sonnet55;
    let store = SessionStore::open(home.path()).unwrap();
    let root_id = uuid::Uuid::now_v7().to_string();
    let (root, _events) = Nanocodex::builder(Claude::new(client.clone(), model.as_str()))
        .max_tokens(4096)
        .thinking(Thinking::High)
        .unwrap()
        .server_tool(ServerToolDefinition::web_search_basic(3))
        .durability(
            store
                .session(SessionRecord::root(
                    root_id.clone(),
                    HarnessModel::Claude(model),
                    Some(home.path().to_path_buf()),
                ))
                .await
                .unwrap(),
        )
        .await
        .unwrap()
        .build()
        .unwrap();
    root.prompt(PromptRequest::new("search for nanocodex").request_id("search"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    root.shutdown().await.unwrap();

    // Newer states than one list page: subagent journals and non-sessions.
    let mut raw = nanocodex_durability::SqliteStore::open(SessionStore::path(home.path())).unwrap();
    for index in 0..nanocodex_durability::LIST_LIMIT + 50 {
        let key = if index % 2 == 0 {
            format!("{root_id}-{index}:subagents")
        } else {
            format!("unrelated-{index}")
        };
        let owned = raw.acquire(&key, OwnerId::new()).await.unwrap();
        raw.replace(&key, &owned.owner, owned.state.revision, "{}", &[])
            .await
            .unwrap();
    }
    let listed = store.list().await.unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|summary| summary.record.session_id.as_str())
            .collect::<Vec<_>>(),
        [root_id.as_str()],
        "the session stays listed behind newer non-session states"
    );

    let loaded = store.load(&root_id).await.unwrap();
    assert!(
        loaded
            .transcript
            .contains(&TranscriptItem::Reasoning("weigh the sources".into())),
        "{:?}",
        loaded.transcript
    );
    assert!(loaded.transcript.iter().any(|item| matches!(
        item,
        TranscriptItem::Tool { call_id, name, .. } if call_id == "srv-1" && name == "web_search"
    )));
    assert!(
        !format!("{:?}", loaded.transcript).contains("opaque"),
        "opaque provider payloads stay out of the transcript"
    );

    let checkpoint = loaded
        .session_checkpoint()
        .unwrap()
        .expect("a settled Claude session has a portable checkpoint");
    assert_eq!(checkpoint.family(), HarnessFamily::Claude);
    assert_eq!(checkpoint.session_id(), root_id);
    assert_eq!(checkpoint.model(), HarnessModel::Claude(model));
    assert_eq!(checkpoint.thinking(), Thinking::High);
    let (restored, _events) = Nanocodex::builder(Claude::new(client, model.as_str()))
        .resume(checkpoint)
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(restored.session_id(), root_id);
    let result = restored
        .prompt("follow up")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "restored answer");
    restored.shutdown().await.unwrap();
    let sent = requests.lock().unwrap().last().unwrap().clone();
    assert!(sent["messages"].to_string().contains("searched answer"));
    assert_eq!(sent["output_config"]["effort"], "high");
    server.abort();
}

/// Like Codex, a durable Claude session that cannot persist children refuses
/// to fork instead of silently creating an unsaved session.
#[tokio::test]
async fn durable_claude_fork_without_catalog_record_is_unsupported() {
    use nanocodex_agent::{ForkRequest, NanocodexError};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, _requests, server) = server(|_, _| sse(text("answer"), "end_turn", 12)).await;
    let (root, _events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    root.prompt(PromptRequest::new("first").request_id("first"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let error = root
        .fork(ForkRequest::latest())
        .await
        .err()
        .expect("an unrecorded durable session cannot fork");
    assert!(
        matches!(
            error,
            NanocodexError::ExecutionPolicyBranchUnsupported { operation: "fork" }
        ),
        "{error}"
    );
    root.shutdown().await.unwrap();
    server.abort();
}


/// A durable Claude root's subagents are their own listed, readable and
/// resumable sessions with their own Codex-format rollouts, exactly like Codex.
#[tokio::test]
async fn durable_claude_subagents_are_their_own_resumable_sessions() {
    use nanocodex_agent::{ClaudeModel, HarnessModel, Origin, rollout::RolloutConfig};
    use nanocodex_durability::{SessionRecord, SessionStore, TranscriptItem};
    let prompts = |transcript: &[TranscriptItem]| {
        transcript
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::User(text) => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let home = tempfile::tempdir().unwrap();
    let (client, _requests, _server) =
        server(|index, _| sse(text(&format!("claude reply {index}")), "end_turn", 12)).await;
    let model = ClaudeModel::Sonnet55;
    let store = SessionStore::open(home.path()).unwrap();
    let rollout = RolloutConfig::new(home.path().join("codex"));
    let root_id = uuid::Uuid::now_v7().to_string();
    let (root, _events) = Nanocodex::builder(Claude::new(client.clone(), model.as_str()))
        .max_tokens(4096)
        .rollout(rollout.clone())
        .durability(
            store
                .session(SessionRecord::root(
                    root_id.clone(),
                    HarnessModel::Claude(model),
                    Some(home.path().to_path_buf()),
                ))
                .await
                .unwrap(),
        )
        .await
        .unwrap()
        .build()
        .unwrap();
    root.prompt(PromptRequest::new("root task").request_id("root-1"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let (child, _child_events) = root.spawn().await.unwrap();
    let (grandchild, _grandchild_events) = child.spawn().await.unwrap();
    let child_id = child.session_id().to_owned();
    let grandchild_id = grandchild.session_id().to_owned();
    assert_ne!(child_id, root_id);
    assert_ne!(grandchild_id, child_id);
    let tree = [(&child_id, &root_id), (&grandchild_id, &child_id)];
    for (agent, (id, parent)) in [&child, &grandchild].into_iter().zip(tree) {
        assert_eq!(agent.session().lineage.origin, Origin::Subagent);
        assert_eq!(
            agent.session().lineage.parent_session_id.as_deref(),
            Some(parent.as_str())
        );
        assert_eq!(
            agent.persistence().and_then(|p| p.durable_state_id),
            Some(id.clone()),
            "a durable Claude root's subagent persists to its own state"
        );
        agent
            .prompt(PromptRequest::new("subagent task").request_id("task-1"))
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
    }
    grandchild.shutdown().await.unwrap();
    child.shutdown().await.unwrap();
    root.shutdown().await.unwrap();

    let listed = store.list().await.unwrap();
    for (id, parent) in tree {
        let summary = listed
            .iter()
            .find(|summary| summary.record.session_id == *id)
            .unwrap_or_else(|| panic!("Claude subagent {id} is not listed"));
        assert_eq!(summary.record.lineage.origin, Origin::Subagent);
        assert_eq!(
            summary.record.lineage.parent_session_id.as_deref(),
            Some(parent.as_str())
        );
        assert_eq!(
            prompts(&store.load(id).await.unwrap().transcript),
            ["subagent task"]
        );
    }
    let mirrored = |id: &str| {
        rollout
            .list_sessions()
            .unwrap()
            .iter()
            .filter(|session| session.thread_id() == id)
            .count()
    };
    for id in [&root_id, &child_id, &grandchild_id] {
        assert_eq!(mirrored(id), 1, "{id} has exactly one Codex-format rollout");
    }

    let (resumed, _events) = Nanocodex::builder(Claude::new(client.clone(), model.as_str()))
        .max_tokens(4096)
        .rollout(rollout.clone())
        .durability(store.resume(&child_id).await.unwrap())
        .await
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(resumed.session_id(), child_id);
    resumed
        .prompt(PromptRequest::new("resumed subagent").request_id("task-2"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    resumed.shutdown().await.unwrap();
    assert_eq!(
        prompts(&store.load(&child_id).await.unwrap().transcript),
        ["subagent task", "resumed subagent"]
    );
    assert_eq!(mirrored(&child_id), 1, "resuming appends to the subagent's own rollout");
}


/// A durable Claude session resumed from the catalog keeps its recorded fast
/// mode and thinking level: the host does not configure them again, and the
/// resumed turn's outbound request still asks for fast speed and high effort.
#[tokio::test]
async fn durable_claude_resume_keeps_recorded_fast_mode_and_thinking() {
    use nanocodex_agent::{ClaudeModel, HarnessModel, Thinking};
    use nanocodex_durability::{SessionRecord, SessionStore};
    let home = tempfile::tempdir().unwrap();
    let (client, requests, server) =
        server(|index, _| sse(text(&format!("claude reply {index}")), "end_turn", 12)).await;
    let model = ClaudeModel::Opus55;
    let store = SessionStore::open(home.path()).unwrap();
    let session_id = uuid::Uuid::now_v7().to_string();
    let (root, _events) = Nanocodex::builder(Claude::new(client.clone(), model.as_str()))
        .max_tokens(4096)
        .thinking(Thinking::High)
        .unwrap()
        .fast_mode(true)
        .durability(
            store
                .session(SessionRecord::root(
                    session_id.clone(),
                    HarnessModel::Claude(model),
                    Some(home.path().to_path_buf()),
                ))
                .await
                .unwrap(),
        )
        .await
        .unwrap()
        .build()
        .unwrap();
    root.prompt(PromptRequest::new("fast question").request_id("turn-1"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    root.shutdown().await.unwrap();
    let first = requests.lock().unwrap().last().cloned().unwrap();
    assert_eq!(first["speed"], "fast", "{first}");
    assert_eq!(first["output_config"]["effort"], "high", "{first}");

    // Resume with a plain builder: no thinking or fast-mode configuration.
    let (resumed, _events) = Nanocodex::builder(Claude::new(client.clone(), model.as_str()))
        .max_tokens(4096)
        .durability(store.resume(&session_id).await.unwrap())
        .await
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(resumed.session_id(), session_id);
    resumed
        .prompt(PromptRequest::new("resumed question").request_id("turn-2"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    resumed.shutdown().await.unwrap();
    let log = requests.lock().unwrap().clone();
    assert_eq!(log.len(), 2, "one provider call per turn");
    let continued = &log[1];
    assert_eq!(
        continued["speed"], "fast",
        "the resumed turn keeps the recorded fast mode: {continued}"
    );
    assert_eq!(
        continued["output_config"]["effort"], "high",
        "the resumed turn keeps the recorded thinking level: {continued}"
    );
    let checkpoint = store
        .load(&session_id)
        .await
        .unwrap()
        .session_checkpoint()
        .unwrap()
        .expect("a settled Claude session has a portable checkpoint");
    assert_eq!(checkpoint.thinking(), Thinking::High);
    server.abort();
}

