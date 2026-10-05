//! Claude recovery through loopback Messages HTTP and a reopened SQLite store.
//! Failure cases defined before implementation: terminal receipt replay after
//! restart; completed tool receipts after a failed provider continuation; live
//! cancellation after an effect starts (unknown outcome, never dispatched again);
//! signed/opaque compaction suffixes and container/discovery state across reopen.
//! Pending effects require both persisted and current safe replay permission.
#![cfg(all(feature = "claude", feature = "sqlite"))]

use axum::{Json, Router, response::IntoResponse, routing::post};
use nanocodex_agent::{Nanocodex, PromptRequest};
use nanocodex_claude::{Claude, ClaudeClient, ToolDefinition};
use nanocodex_durability::{DurableAgentExt, DurableSession, SqliteStore};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

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
async fn completed_effect_and_signed_compaction_suffix_survive_reopen() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) = server(|index, _| match index {
        1 => sse(signed_round(), "tool_use", 70_000),
        2 => sse(text("Preserve the original task."), "end_turn", 10),
        3 => "data: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"synthetic followup failure\"}}\n\n".into(),
        _ => sse(text("recovered"), "end_turn", 10),
    }).await;
    let effects = Arc::new(AtomicUsize::new(0));
    let receipt = vec![
        json!({"type":"text","text":"effect committed"}),
        json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"cG5n"}}),
    ];
    for recovery in [false, true] {
        let counter = effects.clone();
        let returned = receipt.clone();
        let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
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
            .prompt(
                PromptRequest::new(if recovery {
                    "reconcile existing receipt"
                } else {
                    "perform effect once"
                })
                .request_id(if recovery { "recovery" } else { "first" }),
            )
            .await
            .unwrap()
            .result()
            .await;
        if recovery {
            assert_eq!(result.unwrap().final_message(), "recovered");
        } else {
            assert!(result.is_err());
        }
        agent.shutdown().await.unwrap();
        drop((agent, events));
    }
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 4);
    assert_eq!(
        effects.load(Ordering::SeqCst),
        1,
        "completed effects must not repeat after reopen"
    );
    assert_eq!(log[2]["messages"][1]["content"], json!(signed_round()));
    assert_eq!(
        log[2]["messages"][2]["content"][0]["content"],
        json!(receipt)
    );
    assert_eq!(
        &log[3]["messages"].as_array().unwrap()[..3],
        log[2]["messages"].as_array().unwrap()
    );
    assert_eq!(log[3]["container"], "stable-container");
    assert_eq!(log[0]["tools"], log[3]["tools"]);
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
    assert_eq!(log[2]["messages"][1]["content"], json!(signed_round()));
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
            .client_tool_search()
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
}

async fn transaction_recovery(
    fail_at: Option<usize>,
    after_commit: bool,
    journey: CompactionJourney,
) -> usize {
    let context_exhaustion = !matches!(journey, CompactionJourney::Automatic);
    let repeated_exhaustion = matches!(journey, CompactionJourney::ExhaustionAfterRecovery);
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
        let summarized_receipt = request["messages"]
            .to_string()
            .contains("Retain the synthetic task and committed receipt.");
        if context_exhaustion
            && has_receipt
            && !request["messages"].to_string().contains("signed-exhaustion")
        {
            sse(
                vec![
                    json!({"type":"thinking","thinking":"partial reasoning","signature":"signed-exhaustion"}),
                    json!({"type":"text","text":"partial answer"}),
                ],
                "model_context_window_exceeded",
                10,
            )
        } else if has_receipt || summarized_receipt {
            sse(
                text("completed exactly once"),
                if repeated_exhaustion { "model_context_window_exceeded" } else { "end_turn" },
                10,
            )
        } else {
            sse(signed_round(), "tool_use", if context_exhaustion { 10 } else { 70_000 })
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
        .auto_compact_window_tokens(100_000)
        .tasks(board.clone())
        // This fixture mutates only the restored, receipt-coupled task board.
        .tool_replay_safety("effect", nanocodex_agent::ReplaySafety::Safe)
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
        assert!(
            first
                .unwrap_err()
                .to_string()
                .contains("context window exhausted after recovery")
        );
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
        .tasks(board.clone())
        // This fixture mutates only the restored, receipt-coupled task board.
        .tool_replay_safety("effect", nanocodex_agent::ReplaySafety::Safe);
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
    } else {
        // Keep current declaration authority for fresh inference, without restoring
        // the old handler: the committed receipt must bypass this sentinel.
        builder = builder.tool(tool(), |_| async {
            panic!("committed task receipt must bypass replacement handler")
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
            error
                .to_string()
                .contains("context window exhausted after recovery"),
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
        assert!(continuation.contains("Retain the synthetic task and committed receipt."));
        if after_commit || fail_at.is_none() {
            assert_eq!(log.len(), 4, "committed model responses must not repeat");
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
    println!(
        "Claude automatic compaction: {count} write boundaries, precommit and lost-ACK crash at each"
    );
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
        println!(
            "Claude context recovery repeated_exhaustion={}: {count} write boundaries, precommit and lost-ACK crash at each",
            matches!(journey, CompactionJourney::ExhaustionAfterRecovery)
        );
        for after_commit in [false, true] {
            for ordinal in 0..count {
                transaction_recovery(Some(ordinal), after_commit, journey).await;
            }
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
        // Fresh inference still requires current catalog authority. The original
        // handler is gone; replay must bypass this replacement sentinel.
        .tool(tool(), |_| async {
            panic!("committed task receipt must bypass replacement handler")
        })
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
        .tasks(board.clone())
        .tool(tool(), |_| async {
            panic!("committed task receipt must bypass replacement handler")
        })
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
    assert_eq!(log[2]["messages"][1]["content"], json!(signed_round()));
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
        assert_eq!(log[2]["messages"][1]["content"][1]["id"], "paused-mutation");
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
            assert!(evidence.contains("opaque-paused-signature"));
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
// while an admitted server effect with no committed receipt remains outcome unknown.
#[tokio::test]
async fn paused_server_cursor_replays_across_store_failure_without_terminalizing() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    for (after_commit, retain_authority) in
        [(false, true), (true, true), (false, false), (true, false)]
    {
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

        for _ in 0..2 {
            let builder = Nanocodex::builder(Claude::new(client.clone(), "different-model"));
            let builder = if retain_authority {
                builder
                    .server_tool(nanocodex_claude::ServerToolDefinition::code_execution_current())
            } else {
                builder
            };
            let (agent, events) = builder
                .durability(reopen(&path).await)
                .await
                .unwrap()
                .build()
                .unwrap();
            let result = agent.prompt(request()).await.unwrap().result().await;
            if after_commit {
                assert_eq!(
                    result.unwrap().final_message(),
                    "recovered prepared server turn"
                );
            } else {
                let error = result.unwrap_err();
                assert!(error.to_string().contains("outcome is unknown"), "{error}");
            }
            assert_eq!(
                requests.lock().unwrap().len(),
                2,
                "unknown server effects and completed receipts must not redispatch HTTP, including revoked authority"
            );
            assert_eq!(
                effects.load(Ordering::SeqCst),
                1,
                "the provider mutation must execute exactly once"
            );
            let _ = agent.shutdown().await;
            drop((agent, events));
        }
        println!(
            "Claude HTTP recovery after_commit={after_commit} retain_authority={retain_authority}: requests=2 effects=1"
        );
        let log = requests.lock().unwrap();
        assert!(
            log.iter()
                .all(|request| request["model"] == "original-model")
        );
        server.abort();
    }
}

// A settled paused model receipt may replay after revocation, but its next
// unsent continuation must be checked against the current host catalog.
#[tokio::test]
async fn revoked_server_catalog_blocks_fresh_http_after_completed_pause_replay() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("revoked-server.sqlite");
    let armed = Arc::new(AtomicBool::new(false));
    let arm = armed.clone();
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let (client, requests, server) = server(move |index, _| {
        if index == 1 {
            arm.store(true, Ordering::SeqCst);
            sse(vec![json!({"type":"server_tool_use","id":"revoked-pause","name":"bash_code_execution","input":{"command":"synthetic effect"}})], "pause_turn", 10)
        } else {
            counter.fetch_add(1, Ordering::SeqCst);
            sse(text("unauthorized continuation executed"), "end_turn", 10)
        }
    }).await;
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
    let request =
        || PromptRequest::new("execute only while authorized").request_id("revoked-server");
    let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "original-model"))
        .server_tool(nanocodex_claude::ServerToolDefinition::code_execution_current())
        .durability(state)
        .await
        .unwrap()
        .build()
        .unwrap();
    assert!(
        agent
            .prompt(request())
            .await
            .unwrap()
            .result()
            .await
            .unwrap_err()
            .execution_policy_disposition()
            .is_some()
    );
    let _ = agent.shutdown().await;
    drop((agent, events));
    let (agent, events) = Nanocodex::builder(Claude::new(client, "new-model"))
        .durability(reopen(&path).await)
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
    assert!(
        error
            .to_string()
            .contains("revoked by current host authorization"),
        "{error}"
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "replayed pause cannot grant fresh HTTP authority"
    );
    assert_eq!(
        effects.load(Ordering::SeqCst),
        0,
        "revoked continuation cannot execute"
    );
    println!(
        "Claude revoked catalog after settled pause replay: requests=1 continuation effects=0"
    );
    let _ = agent.shutdown().await;
    drop((agent, events));
    server.abort();
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

#[tokio::test]
async fn interrupted_unsafe_tool_returns_unknown_over_messages_without_redispatch() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) = server(|_, request| {
        let receipt = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["content"].as_array().unwrap())
            .find(|block| block["type"] == "tool_result");
        if let Some(receipt) = receipt {
            assert_eq!(receipt["is_error"], true);
            assert!(receipt["content"].to_string().contains("outcome unknown"));
            sse(text("reconciled outcome unknown"), "end_turn", 10)
        } else {
            sse(
                vec![json!({"type":"tool_use","id":"one-effect","name":"effect","input":{}})],
                "tool_use",
                10,
            )
        }
    })
    .await;
    let count = Arc::new(AtomicUsize::new(0));
    let armed = Arc::new(AtomicBool::new(false));
    let store = FaultStore {
        inner: SqliteStore::open(&path).unwrap(),
        writes: Arc::new(AtomicUsize::new(0)),
        fail_at: None,
        after_commit: false,
        fail_when_armed: Some(armed.clone()),
    };
    let state = DurableSession::open(store, "claude-synthetic")
        .await
        .unwrap();
    let handler_count = count.clone();
    let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .tool(tool(), move |_| {
            handler_count.fetch_add(1, Ordering::SeqCst);
            armed.store(true, Ordering::SeqCst);
            async { Ok("external action occurred".into()) }
        })
        .durability(state)
        .await
        .unwrap()
        .build()
        .unwrap();
    let request = || PromptRequest::new("perform one effect").request_id("unsafe-recovery");
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
    let handler_count = count.clone();
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .tool(tool(), move |_| {
            handler_count.fetch_add(1, Ordering::SeqCst);
            async { Ok("must not run".into()) }
        })
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
        "reconciled outcome unknown"
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(requests.lock().unwrap().len(), 2);
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
}

#[tokio::test]
async fn historical_document_fork_restores_native_claude_checkpoint_after_receipt_pruning() {
    use nanocodex_durability::{DocumentForkPolicy as Policy, DocumentWrite};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("historical-documents.sqlite");
    let (client, requests, server) =
        server(|_, _| sse(text("document answer"), "end_turn", 12)).await;
    let open = |id| {
        DurableSession::open_with_terminal_receipt_limit(SqliteStore::open(&path).unwrap(), id, 1)
    };
    let writes = |version, value| {
        [Policy::Initial, Policy::Current, Policy::AsOf]
            .into_iter()
            .zip(["initial", "current", "asOf"])
            .map(|(fork, key)| DocumentWrite {
                key: key.into(),
                expected_version: version,
                value: json!(value),
                fork,
            })
            .collect()
    };
    let source = open("claude-parent").await.unwrap();
    let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .durability(source.clone())
        .await
        .unwrap()
        .build()
        .unwrap();
    source
        .compare_exchange_documents(writes(0, 1))
        .await
        .unwrap();
    for index in 0..5 {
        let id = format!("claude-historical-{index}");
        agent
            .prompt(PromptRequest::new(format!("CLAUDE_BOUNDARY_{index}")).request_id(id))
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        if index == 0 {
            source
                .compare_exchange_documents(writes(1, 2))
                .await
                .unwrap();
        }
    }
    agent.shutdown().await.unwrap();
    drop((agent, events, source));
    let source = open("claude-parent").await.unwrap();
    assert!(
        source
            .state()
            .await
            .unwrap()
            .operation("claude-historical-0")
            .is_none()
    );
    let (checkpoint, seed) = source.document_fork("claude-historical-0").await.unwrap();
    assert_eq!(
        ["initial", "current", "asOf"].map(|key| seed.documents[key].value.clone()),
        [json!(1), json!(2), json!(1)]
    );
    let child = open("claude-child").await.unwrap();
    child
        .initialize_document_fork(seed, &checkpoint)
        .await
        .unwrap();
    drop(child);
    let child = open("claude-child").await.unwrap();
    assert_eq!(
        child.document("asOf").await.unwrap().unwrap().value,
        json!(1)
    );
    child
        .compare_exchange_documents(vec![DocumentWrite {
            key: "asOf".into(),
            expected_version: 1,
            value: json!(42),
            fork: Policy::AsOf,
        }])
        .await
        .unwrap();
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .durability(child.clone())
        .await
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt(PromptRequest::new("CLAUDE_CHILD_ONLY").request_id("child-next"))
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "document answer"
    );
    {
        let log = requests.lock().unwrap();
        let last = serde_json::to_string(log.last().unwrap()).unwrap();
        assert!(last.contains("CLAUDE_BOUNDARY_0") && last.contains("CLAUDE_CHILD_ONLY"));
        assert!(!last.contains("CLAUDE_BOUNDARY_4"));
    }
    assert_eq!(
        source.document("asOf").await.unwrap().unwrap().value,
        json!(2)
    );
    assert_eq!(
        child.document("asOf").await.unwrap().unwrap().value,
        json!(42)
    );
    agent.shutdown().await.unwrap();
    drop((agent, events));
    println!(
        "Native Claude HTTP + cold SQLite: five completions retention1, historical first checkpoint and policies [1,2,1], cold branch history excludes later turns; child value42 parent2"
    );
    server.abort();
}
