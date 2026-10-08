//! Context recovery journeys through the public backend and loopback Messages API.
use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
use futures_util::StreamExt;
use nanocodex_agent::{Nanocodex, events::AgentEventKind};
use nanocodex_claude::{Claude, ClaudeClient, ToolDefinition};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

fn sse(blocks: Vec<Value>, stop: &str, input: u64) -> String {
    let mut output = String::new();
    let mut emit = |value: Value| output.push_str(&format!("data: {value}\n\n"));
    emit(
        json!({"type":"message_start","message":{"id":"synthetic","role":"assistant","model":"test","content":[],"usage":{"input_tokens":input,"output_tokens":0}}}),
    );
    for (index, block) in blocks.into_iter().enumerate() {
        emit(json!({"type":"content_block_start","index":index,"content_block":block}));
        emit(json!({"type":"content_block_stop","index":index}));
    }
    emit(json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":5}}));
    emit(json!({"type":"message_stop"}));
    output
}

// The loopback provider enforces the Messages turn boundary: ordinary user
// text cannot terminate a directly called server tool before its result exists.
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
    respond: impl Fn(usize, &Value) -> (Vec<Value>, &'static str, u64) + Send + Sync + 'static,
    fail_at: Option<usize>,
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
                if std::env::var_os("NANOCLAUDE_CONTEXT_TRACE").is_some() {
                    eprintln!(
                        "{}",
                        json!({
                            "scenario": std::thread::current().name(),
                            "request_index": index,
                            "synthetic_failure": Some(index) == fail_at,
                            "request": body,
                        })
                    );
                }
                if invalid_server_boundary(&body) {
                    return (
                        StatusCode::BAD_REQUEST,
                        "unresolved server tool before user text",
                    )
                        .into_response();
                }
                if Some(index) == fail_at {
                    return (StatusCode::BAD_REQUEST, "synthetic failure").into_response();
                }
                let (blocks, stop, input) = respond(index, &body);
                (
                    [("content-type", "text/event-stream")],
                    sse(blocks, stop, input),
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    (client, requests, task)
}

fn text(value: &str) -> Vec<Value> {
    vec![json!({"type":"text","text":value})]
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
fn pending_round() -> Vec<Value> {
    vec![
        json!({"type":"thinking","thinking":"signed reasoning","signature":"opaque-signature","binding":"opaque-binding"}),
        json!({"type":"tool_use","id":"effect-a","name":"effect","input":{"key":"a"},"caller":{"type":"direct"}}),
        json!({"type":"tool_use","id":"effect-b","name":"effect","input":{"key":"b"}}),
    ]
}

#[tokio::test]
async fn retained_tool_suffix_survives_compaction_failed_followup_and_recovery() {
    let (client, requests, task) = server(
        |index, _| match index {
            1 => (pending_round(), "tool_use", 66_900),
            2 => (
                text("Earlier task: perform both synthetic effects."),
                "end_turn",
                20,
            ),
            _ => (text("recovered"), "end_turn", 100),
        },
        Some(3),
    )
    .await;
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let receipt = json!({"type":"text","text":"receipt".repeat(500)});
    let returned = vec![
        receipt,
        json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGMQjD0JAAG6ATiGpB8nAAAAAElFTkSuQmCC"}}),
    ];
    let results = returned.clone();
    let (agent, _) = Nanocodex::builder(Claude::latest(client))
        .auto_compact_window_tokens(100_000)
        .tool_blocks(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            let results = results.clone();
            async move { Ok(results) }
        })
        .build()
        .unwrap();
    assert!(
        agent
            .prompt("perform both effects once")
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    agent
        .prompt("continue without repeating effects")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 4);
    let summary = &log[1]["messages"];
    assert!(
        !summary.to_string().contains("effect-a"),
        "pending round must be excluded from summary input"
    );
    // There is no older completed round in this first-turn case. Summarize
    // the original user task alone and retain the entire first tool exchange.
    assert_eq!(summary.as_array().unwrap().len(), 2);
    assert_eq!(
        summary[0]["content"][0]["text"],
        "perform both effects once"
    );
    let continuation = log[2]["messages"].as_array().unwrap();
    assert_eq!(continuation.len(), 3);
    assert_eq!(continuation[1]["content"], json!(&pending_round()[1..]));
    assert_eq!(continuation[2]["content"][0]["tool_use_id"], "effect-a");
    assert_eq!(continuation[2]["content"][1]["tool_use_id"], "effect-b");
    assert_eq!(continuation[2]["content"][0]["content"], json!(returned));
    assert_eq!(
        &log[3]["messages"].as_array().unwrap()[..3],
        continuation.as_slice()
    );
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    task.abort();
}

#[tokio::test]
async fn repeated_manual_compaction_includes_prior_summary_and_failed_summary_is_atomic() {
    let (client, requests, task) = server(
        |index, _| match index {
            1 => (text("first answer"), "end_turn", 10),
            2 => (text("first summary, preserve constraint A"), "end_turn", 10),
            3 => (text(" "), "end_turn", 10),
            4 => (
                text("second summary preserves constraint A"),
                "end_turn",
                10,
            ),
            _ => (text("continued"), "end_turn", 10),
        },
        None,
    )
    .await;
    let (agent, _) = Nanocodex::builder(Claude::latest(client)).build().unwrap();
    agent
        .prompt("constraint A")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    agent.compact().await.unwrap();
    assert!(agent.compact().await.is_err());
    agent.compact().await.unwrap();
    agent
        .prompt("Correction: constraint B replaces constraint A; do not publish.")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 5);
    assert_eq!(
        log[2]["messages"], log[3]["messages"],
        "failed summary must leave prior summary unchanged"
    );
    assert!(
        log[3]["messages"]
            .to_string()
            .contains("first summary, preserve constraint A")
    );
    assert!(
        log[4]["messages"]
            .to_string()
            .contains("second summary preserves constraint A")
    );
    // A later user correction must remain a separate, latest user message;
    // this checks transport ordering, not the summarizer's semantic fidelity.
    let messages = log[4]["messages"].as_array().unwrap();
    let correction = messages.last().unwrap();
    assert_eq!(correction["role"], "user");
    assert_eq!(
        correction["content"][0]["text"],
        "Correction: constraint B replaces constraint A; do not publish."
    );
    assert!(
        !messages[0].to_string().contains("constraint B"),
        "new user steering must not be folded into generated history"
    );
    task.abort();
}

#[tokio::test]
async fn advancing_rounds_allow_new_compaction_with_bounded_rapid_refill() {
    let (client, requests, task) = server(|index, _| match index {
        1 => (pending_round(), "tool_use", 70_000),
        2 | 4 | 8 => (text("task summary"), "end_turn", 70_000),
        3 | 5 | 6 | 7 => (vec![json!({"type":"tool_use","id":format!("effect-{index}"),"name":"effect","input":{}})], "tool_use", 70_000),
        _ => (text("done"), "end_turn", 70_000),
    }, None).await;
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let (agent, mut events) = Nanocodex::builder(Claude::latest(client))
        .auto_compact_window_tokens(100_000)
        .tool(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("receipt".into()) }
        })
        .build()
        .unwrap();
    let result = agent
        .prompt("perform effects")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    // Hosts restate prompt-carried context after a summary replaces history.
    let mut compactions = Vec::new();
    loop {
        let event = events.next().await.unwrap();
        if event.kind == AgentEventKind::ModelCompactionCompleted {
            let payload: Value = serde_json::from_str(event.payload.get()).unwrap();
            compactions.push(payload["after_model_call_index"].as_u64().unwrap());
        }
        if event.kind == AgentEventKind::RunCompleted {
            break;
        }
    }
    assert_eq!(
        compactions,
        [1, 2, 5],
        "one event per summary (requests 2, 4 and 8)"
    );
    assert_eq!(result.final_message(), "done");
    assert_eq!(result.usage().unwrap().input_tokens(), 630_000);
    let log = requests.lock().unwrap();
    assert_eq!(
        log.len(),
        9,
        "new rounds permit compaction; two rapid summaries require three advancing rounds before another"
    );
    for (summary, continuation, id) in [(3, 4, "effect-3"), (7, 8, "effect-7")] {
        assert!(!log[summary]["messages"].to_string().contains(id));
        assert_eq!(
            log[continuation]["messages"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["content"][0]["tool_use_id"],
            id
        );
    }
    assert_eq!(effects.load(Ordering::SeqCst), 6);
    task.abort();
}

#[tokio::test]
async fn server_pause_suffix_survives_summary_and_failed_continuation() {
    let paused = json!({"type":"server_tool_use","id":"srv-pending","name":"web_fetch","input":{"url":"https://example.org"},"opaque":"preserve"});
    let source = paused.clone();
    let (client, requests, task) = server(
        move |index, _| match index {
            1 => (vec![source.clone()], "pause_turn", 70_000),
            2 => (
                text("Fetch the requested page and report its result."),
                "end_turn",
                10,
            ),
            _ => (text("reconciled uncertain fetch"), "end_turn", 10),
        },
        Some(3),
    )
    .await;
    let (agent, _) = Nanocodex::builder(Claude::latest(client))
        .auto_compact_window_tokens(100_000)
        .server_tool(nanocodex_claude::ServerToolDefinition::web_fetch_basic(1))
        .build()
        .unwrap();
    assert!(
        agent
            .prompt("fetch page")
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    let result = agent
        .prompt("continue fetch")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "reconciled uncertain fetch");
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 4);
    assert!(!log[1]["messages"].to_string().contains("srv-pending"));
    assert_eq!(log[2]["messages"].as_array().unwrap().len(), 2);
    assert_eq!(log[2]["messages"][1]["content"], json!([paused]));
    let messages = log[3]["messages"].as_array().unwrap();
    assert!(messages.iter().all(|message| message["role"] == "user"));
    assert!(log[3]["messages"].to_string().contains("outcome unknown"));
    assert!(log[3]["messages"].to_string().contains("srv-pending"));
    assert_eq!(
        messages.last().unwrap()["content"][0]["text"],
        "continue fetch"
    );
    assert_eq!(
        log[3]["messages"]
            .to_string()
            .matches("continue fetch")
            .count(),
        1
    );
    assert_eq!(log[0]["tools"], log[2]["tools"]);
    assert_eq!(
        log[1]["tools"], log[0]["tools"],
        "summary keeps stable server catalog"
    );
    assert_eq!(
        log[1]["tool_choice"],
        json!({"type":"none"}),
        "summary must prohibit server effects at API boundary"
    );
    assert!(log[0].get("tool_choice").is_none());
    assert!(log[2].get("tool_choice").is_none());
    task.abort();
}

#[tokio::test]
async fn rejected_tool_summary_keeps_completed_effects_for_manual_recovery() {
    let (client, requests, task) = server(
        |index, _| match index {
            1 => (pending_round(), "tool_use", 70_000),
            2 => (
                vec![json!({"type":"tool_use","id":"summary-call","name":"effect","input":{}})],
                "tool_use",
                10,
            ),
            3 => (text("Original task summary after retry"), "end_turn", 10),
            _ => (text("recovered"), "end_turn", 10),
        },
        None,
    )
    .await;
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let (agent, _) = Nanocodex::builder(Claude::latest(client))
        .auto_compact_window_tokens(100_000)
        .tool(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("committed".into()) }
        })
        .build()
        .unwrap();
    assert!(
        agent
            .prompt("effects once")
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    agent.compact().await.unwrap();
    agent
        .prompt("recover")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 4);
    assert_eq!(log[1]["messages"], log[2]["messages"]);
    assert_eq!(
        log[3]["messages"][1]["content"],
        json!(&pending_round()[1..])
    );
    assert_eq!(log[3]["messages"][2]["content"][1]["content"], "committed");
    assert_eq!(
        effects.load(Ordering::SeqCst),
        2,
        "summarization must never execute tools"
    );
    task.abort();
}

fn discovery(id: &str) -> Vec<Value> {
    vec![
        json!({"type":"tool_use","id":id,"name":"ToolSearch","input":{"query":"select:effect","max_results":1}}),
    ]
}

#[tokio::test]
async fn only_successful_compaction_resets_dropped_discoveries_until_rediscovery() {
    let (client, requests, task) = server(|index, _| match index {
        1 => (discovery("find-original"), "tool_use", 10),
        3 => (text(" "), "end_turn", 10),
        4 | 7 | 9 => (vec![json!({"type":"tool_use","id":format!("effect-{index}"),"name":"effect","input":{}})], "tool_use", 10),
        6 => (text("Earlier discovery and effect completed."), "end_turn", 10),
        8 => (discovery("find-again"), "tool_use", 10),
        _ => (text("done"), "end_turn", 10),
    }, None).await;
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let mut deferred = tool();
    deferred.defer_loading = true;
    let (agent, _) = Nanocodex::builder(Claude::latest(client))
        .client_tool_search()
        .tool(deferred, move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("receipt".into()) }
        })
        .build()
        .unwrap();
    agent
        .prompt("discover effect")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert!(agent.compact().await.is_err());
    agent
        .prompt("use preserved discovery")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(
        effects.load(Ordering::SeqCst),
        1,
        "failed summary must keep discovery active"
    );
    agent.compact().await.unwrap();
    let error = agent
        .prompt("try old discovery directly")
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("before discovery"));
    assert_eq!(
        effects.load(Ordering::SeqCst),
        1,
        "dropped reference cannot authorize execution"
    );
    agent
        .prompt("rediscover then use effect")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 10);
    assert!(
        log[3]["messages"]
            .to_string()
            .contains("\"tool_reference\"")
    );
    assert!(
        !log[6]["messages"]
            .to_string()
            .contains("\"tool_reference\"")
    );
    assert!(
        log[8]["messages"]
            .to_string()
            .contains("\"tool_reference\"")
    );
    assert!(
        log.iter()
            .all(|request| request["tools"] == log[0]["tools"])
    );
    task.abort();
}

#[tokio::test]
async fn retained_discovery_round_allows_next_deferred_call_after_compaction() {
    let (client, requests, task) = server(
        |index, _| match index {
            1 => (discovery("find-retained"), "tool_use", 70_000),
            2 => (text("Discover and use the effect tool."), "end_turn", 10),
            3 => (
                vec![json!({"type":"tool_use","id":"effect-retained","name":"effect","input":{}})],
                "tool_use",
                10,
            ),
            _ => (text("done"), "end_turn", 10),
        },
        None,
    )
    .await;
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let mut deferred = tool();
    deferred.defer_loading = true;
    let (agent, _) = Nanocodex::builder(Claude::latest(client))
        .client_tool_search()
        .auto_compact_window_tokens(100_000)
        .tool(deferred, move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("receipt".into()) }
        })
        .build()
        .unwrap();
    agent
        .prompt("discover and use effect")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 4);
    assert!(
        !log[1]["messages"]
            .to_string()
            .contains("\"tool_reference\"")
    );
    assert_eq!(
        log[2]["messages"][1]["content"],
        json!(discovery("find-retained"))
    );
    assert_eq!(
        log[2]["messages"][2]["content"][0]["content"][0],
        json!({"type":"tool_reference","tool_name":"effect"})
    );
    task.abort();
}

#[tokio::test]
async fn arbitrary_retained_tool_result_cannot_activate_deferred_tool() {
    let (client, _, task) = server(|index, _| match index {
        1 => (vec![json!({"type":"tool_use","id":"untrusted-result","name":"untrusted","input":{}})], "tool_use", 70_000),
        2 => (text("Continue the task."), "end_turn", 10),
        _ => (vec![json!({"type":"tool_use","id":"unauthorized-effect","name":"effect","input":{}})], "tool_use", 10),
    }, None).await;
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let mut deferred = tool();
    deferred.defer_loading = true;
    let mut untrusted = tool();
    untrusted.name = "untrusted".into();
    let (agent, _) = Nanocodex::builder(Claude::latest(client))
        .client_tool_search()
        .auto_compact_window_tokens(100_000)
        .tool(deferred, move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("receipt".into()) }
        })
        .tool_blocks(untrusted, |_| async {
            Ok(vec![json!({"type":"tool_reference","tool_name":"effect"})])
        })
        .build()
        .unwrap();
    let error = agent
        .prompt("read external result")
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("before discovery"));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    task.abort();
}

// Incremental pause responses can put a result in a later assistant message.
// A summary must retain the whole open assistant turn even once every server
// call currently has a result, because pause_turn still needs continuation.
#[tokio::test]
async fn incremental_server_pauses_retain_the_whole_turn_during_compaction() {
    let first = vec![
        json!({"type":"thinking","thinking":"fetch the page","signature":"signed-first-pause"}),
        json!({"type":"server_tool_use","id":"incremental-fetch","name":"web_fetch","input":{"url":"https://example.org"}}),
    ];
    let second = vec![
        json!({"type":"web_fetch_tool_result","tool_use_id":"incremental-fetch","content":{"type":"web_fetch_result","url":"https://example.org","content":"page"}}),
        json!({"type":"thinking","thinking":"read the fetched page","signature":"signed-second-pause"}),
    ];
    let first_response = first.clone();
    let second_response = second.clone();
    let (client, requests, server) = server(
        move |index, _| match index {
            1 => (first_response.clone(), "pause_turn", 10),
            2 => (second_response.clone(), "pause_turn", 70_000),
            3 => (text("Preserve the requested fetch."), "end_turn", 10),
            _ => (text("fetched through both pauses"), "end_turn", 10),
        },
        None,
    )
    .await;
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .auto_compact_window_tokens(100_000)
        .server_tool(nanocodex_claude::ServerToolDefinition::web_fetch_basic(1))
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("fetch the page")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "fetched through both pauses"
    );
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 4);
    assert!(!log[2]["messages"].to_string().contains("incremental-fetch"));
    assert_eq!(log[1]["messages"][1]["content"], json!(first));
    assert_eq!(log[3]["messages"][1]["content"], json!(&first[1..]));
    assert_eq!(log[3]["messages"][2]["content"], json!(&second[..1]));
    assert_eq!(log[3]["messages"].as_array().unwrap().len(), 3);
    server.abort();
}

// A rejected summary is itself a failed turn. Its unresolved suffix must be
// settled before manual compaction or an unrelated user request can proceed.
#[tokio::test]
async fn failed_server_pause_summary_is_data_before_manual_compaction() {
    let (client, requests, task) = server(
        |index, _| match index {
            1 => (vec![json!({"type":"server_tool_use","id":"summary-failure-fetch","name":"web_fetch","input":{"url":"https://example.org"}})], "pause_turn", 70_000),
            3 => (text("A deliberately lossy summary"), "end_turn", 10),
            _ => (text("reconciled after summary failure"), "end_turn", 10),
        }, Some(2),
    ).await;
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .auto_compact_window_tokens(100_000)
        .server_tool(nanocodex_claude::ServerToolDefinition::web_fetch_basic(1))
        .build()
        .unwrap();
    assert!(
        agent
            .prompt("fetch once")
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    agent.compact().await.unwrap();
    assert_eq!(
        agent
            .prompt("reconcile the fetch")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "reconciled after summary failure"
    );
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 4);
    for request in &log[2..] {
        assert!(
            request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["role"] == "user")
        );
        assert!(
            request["messages"]
                .to_string()
                .contains("summary-failure-fetch")
        );
        assert!(request["messages"].to_string().contains("outcome unknown"));
    }
    task.abort();
}

// Even without new server blocks, a malformed continuation is evidence of an
// uncertain prior server turn. No client callback may run and no call may replay.
#[tokio::test]
async fn invalid_client_continuation_preserves_prior_server_uncertainty() {
    let (client, requests, task) = server(
        |index, _| match index {
            1 => (vec![json!({"type":"server_tool_use","id":"invalid-prior-fetch","name":"web_fetch","input":{"url":"https://example.org"}})], "pause_turn", 10),
            2 => (vec![json!({"type":"tool_use","id":"invalid-client-response","name":"effect","input":{}})], "end_turn", 10),
            _ => (text("reconciled invalid continuation"), "end_turn", 10),
        }, None,
    ).await;
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .server_tool(nanocodex_claude::ServerToolDefinition::web_fetch_basic(1))
        .tool(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("must not dispatch".into()) }
        })
        .build()
        .unwrap();
    assert!(
        agent
            .prompt("fetch once")
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    assert_eq!(
        agent
            .prompt("reconcile invalid continuation")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "reconciled invalid continuation"
    );
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 3);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    let messages = &log[2]["messages"];
    assert!(
        messages
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] == "user")
    );
    assert!(messages.to_string().contains("outcome unknown"));
    assert!(messages.to_string().contains("invalid-prior-fetch"));
    assert!(messages.to_string().contains("invalid-client-response"));
    task.abort();
}

#[tokio::test]
async fn end_turn_without_prior_server_result_fails_and_recovers_as_data() {
    let (client, requests, task) = server(
        |index, _| match index {
            1 => (vec![json!({"type":"mcp_tool_use","id":"missing-mcp-result","name":"fetch","server_name":"synthetic","input":{}})], "pause_turn", 10),
            2 => (text(&"no server result received ".repeat(900)), "end_turn", 10),
            3 => (text("A lossy summary of the failed turn"), "end_turn", 10),
            _ => (text("reconciled missing result"), "end_turn", 10),
        }, None,
    ).await;
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .auto_compact_window_tokens(4_000)
        .server_tool(nanocodex_claude::ServerToolDefinition::web_fetch_basic(1))
        .build()
        .unwrap();
    let failure = agent
        .prompt("fetch once")
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("without a complete server-tool result")
    );
    agent
        .prompt("reconcile missing result")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = requests.lock().unwrap();
    assert_eq!(
        log.len(),
        4,
        "converted evidence must count toward the next compaction threshold"
    );
    assert_eq!(log[2]["tool_choice"]["type"], "none");
    assert!(log[3].get("tool_choice").is_none());
    assert!(
        log[3]["messages"]
            .to_string()
            .contains("missing-mcp-result")
    );
    assert!(
        log[3]["messages"]
            .to_string()
            .contains("reconcile missing result")
    );
    assert!(
        log[2]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] == "user")
    );
    assert!(
        log[2]["messages"]
            .to_string()
            .contains("missing-mcp-result")
    );
    assert!(
        log[2]["messages"]
            .to_string()
            .contains("no server result received")
    );
    task.abort();
}

#[tokio::test]
async fn context_exhaustion_retains_output_and_completed_effects() {
    let exhausted = vec![
        json!({"type":"thinking","thinking":"partial reasoning","signature":"signed-exhaustion"}),
        json!({"type":"server_tool_use","id":"completed-fetch","name":"web_fetch","input":{"url":"https://example.org"}}),
        json!({"type":"web_fetch_tool_result","tool_use_id":"completed-fetch","content":{"type":"web_fetch_result","url":"https://example.org","content":"page"}}),
        json!({"type":"text","text":"partial answer ".repeat(3000)}),
    ];
    let source = exhausted.clone();
    let (client, requests, task) = server(
        move |index, body| {
            // Synthetic byte capacity models a provider that accepts input but
            // stops generation when input plus output fills its context window.
            const CAPACITY: usize = 145_000;
            let input = body["messages"].to_string().len();
            assert!(input < CAPACITY, "recovery must reduce the request input");
            match index {
                1 => (text("background received"), "end_turn", 10),
                2 => (pending_round(), "tool_use", 10),
                3 => {
                    assert!(input + json!(source).to_string().len() > CAPACITY);
                    (source.clone(), "model_context_window_exceeded", 10)
                }
                4 => {
                    assert_eq!(
                        body["max_tokens"], 128_000,
                        "recovery preserves the caller's output budget"
                    );
                    (text("Perform the requested task."), "end_turn", 10)
                }
                _ => (text("completed after recovery"), "end_turn", 10),
            }
        },
        None,
    )
    .await;
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .max_tokens(128_000)
        .adaptive_thinking()
        .server_tool(nanocodex_claude::ServerToolDefinition::web_fetch_basic(1))
        .tool(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("committed receipt".into()) }
        })
        .build()
        .unwrap();
    agent
        .prompt("background ".repeat(10_000))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let result = agent
        .prompt("perform effects once")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "completed after recovery");
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 5);
    assert_eq!(log[3]["tool_choice"], json!({"type":"none"}));
    assert_eq!(log[3]["thinking"], json!({"type":"disabled"}));
    assert_eq!(log[3]["max_tokens"], 128_000);
    assert!(!log[3]["messages"].to_string().contains("completed-fetch"));
    assert_eq!(
        log[4]["messages"][1]["content"],
        json!(&pending_round()[1..])
    );
    assert_eq!(
        log[4]["messages"][2]["content"][0]["content"],
        "committed receipt"
    );
    assert_eq!(log[4]["messages"][3]["content"], json!(&exhausted[1..]));
    assert_eq!(log[4]["messages"][4]["role"], "user");
    assert_eq!(log[4]["max_tokens"], 128_000);
    assert_eq!(log[4]["thinking"], log[0]["thinking"]);
    assert_eq!(log[4]["tools"], log[0]["tools"]);
    task.abort();
}

#[tokio::test]
async fn context_exhaustion_retries_once_and_retains_partial_text_on_failure() {
    let (client, requests, task) = server(
        |index, _| match index {
            1 | 3 => (text("partial answer"), "model_context_window_exceeded", 10),
            2 => (text("Task summary"), "end_turn", 10),
            _ => (text("manually continued"), "end_turn", 10),
        },
        None,
    )
    .await;
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .build()
        .unwrap();
    let error = agent
        .prompt("finish task")
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("context window exhausted after recovery"),
        "{error}"
    );
    assert_eq!(requests.lock().unwrap().len(), 3);
    agent
        .prompt("continue manually")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 4);
    assert!(log[3]["messages"].to_string().contains("partial answer"));
    task.abort();
}

#[tokio::test]
async fn context_exhaustion_summary_failure_preserves_received_output() {
    let (client, requests, task) = server(
        |index, _| match index {
            1 => (text("partial answer"), "model_context_window_exceeded", 10),
            2 => (
                text("incomplete summary"),
                "model_context_window_exceeded",
                10,
            ),
            _ => (text("manual recovery"), "end_turn", 10),
        },
        None,
    )
    .await;
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .build()
        .unwrap();
    let error = agent
        .prompt("finish task")
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("compaction summary did not end normally"),
        "{error}"
    );
    agent
        .prompt("continue manually")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 3);
    assert!(log[2]["messages"].to_string().contains("partial answer"));
    assert!(
        !log[2]["messages"]
            .to_string()
            .contains("incomplete summary")
    );
    task.abort();
}

#[tokio::test]
async fn summary_omits_invalidated_thinking_and_replays_new_reasoning() {
    let fresh = vec![
        json!({"type":"thinking","thinking":"fresh reasoning","signature":"fresh-signature"}),
        json!({"type":"redacted_thinking","data":"fresh-redacted"}),
        json!({"type":"text","text":"completed"}),
    ];
    let answer = fresh.clone();
    // Request 3 fails before any response commits. The recovery summary and its
    // pending thinking-only boundary therefore remain uncommitted, which makes
    // manual compaction pack the retained history with the summary applied.
    let (client, requests, task) = server(
        move |index, _| match index {
            1 => (
                vec![
                    json!({"type":"thinking","thinking":"","signature":"stale-signature"}),
                    json!({"type":"redacted_thinking","data":"stale-redacted"}),
                ],
                "model_context_window_exceeded",
                10,
            ),
            2 | 4 => (text("Preserve the task"), "end_turn", 10),
            5 => (answer.clone(), "end_turn", 10),
            _ => (text("reviewed"), "end_turn", 10),
        },
        Some(3),
    )
    .await;
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .adaptive_thinking()
        .keep_thinking()
        .build()
        .unwrap();
    assert!(
        agent
            .prompt("finish task")
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    // The thinking-only response leaves no assistant content once the prior
    // summary is packed, so manual compaction summarizes user context alone.
    agent.compact().await.unwrap();
    for (prompt, expected) in [("continue", "completed"), ("review", "reviewed")] {
        let result = agent.prompt(prompt).await.unwrap().result().await.unwrap();
        assert_eq!(result.final_message(), expected);
    }
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 6);
    for request in &log[2..5] {
        assert!(!request["messages"].to_string().contains("stale-"));
    }
    // Manual compaction found no assistant to retain, so its input is only
    // the prior summary, the recovery continuation and the summarization instruction.
    let summary_input = log[3]["messages"].as_array().unwrap();
    assert!(
        summary_input
            .iter()
            .all(|message| message["role"] == "user")
    );
    assert!(summary_input[0].to_string().contains("Preserve the task"));
    assert!(
        summary_input
            .iter()
            .any(|message| message.to_string().contains("context window was exhausted"))
    );
    assert_eq!(log[5]["messages"][2]["content"], json!(fresh));
    task.abort();
}

#[tokio::test]
async fn output_cutoff_after_summary_replays_only_post_summary_reasoning() {
    let fresh = vec![
        json!({"type":"thinking","thinking":"fresh reasoning","signature":"fresh-signature"}),
        json!({"type":"text","text":"completed"}),
    ];
    let (client, requests, task) = server(
        move |index, _| match index {
            1 => (
                vec![json!({"type":"thinking","thinking":"","signature":"stale-signature"})],
                "model_context_window_exceeded",
                10,
            ),
            2 => (text("Preserve the task"), "end_turn", 10),
            3 => (
                vec![
                    json!({"type":"thinking","thinking":"","signature":"cutoff-signature"}),
                    json!({"type":"text","text":"cut partial"}),
                ],
                "max_tokens",
                10,
            ),
            _ => (fresh.clone(), "end_turn", 10),
        },
        None,
    )
    .await;
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .adaptive_thinking()
        .keep_thinking()
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("finish task")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "completed"
    );
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 4);
    // The pre-summary signature is never replayed. Thinking returned after the
    // summary is bound to the packed prefix, which committing the response
    // stores, so the following continuation replays it exactly.
    assert!(!log[2]["messages"].to_string().contains("stale-"));
    assert!(!log[3]["messages"].to_string().contains("stale-"));
    assert!(!log[2]["messages"].to_string().contains("cutoff-signature"));
    let continuation = log[3]["messages"].as_array().unwrap();
    assert!(continuation[0].to_string().contains("Preserve the task"));
    assert!(
        continuation
            .iter()
            .any(|message| message["role"] == "assistant"
                && message["content"][0]["signature"] == "cutoff-signature"
                && message["content"][1]["text"] == "cut partial")
    );
    let last = continuation.last().unwrap();
    assert_eq!(last["role"], "user");
    assert!(last["content"].to_string().contains("output token limit"));
    task.abort();
}

#[tokio::test]
async fn context_exhaustion_rejects_partial_client_calls_and_unresolved_server_effects() {
    for block in [
        json!({"type":"tool_use","id":"partial-client","name":"effect","input":{}}),
        json!({"type":"server_tool_use","id":"unresolved-server","name":"web_fetch","input":{"url":"https://example.org"}}),
    ] {
        let (client, requests, task) = server(
            move |index, _| match index {
                1 => (vec![block.clone()], "model_context_window_exceeded", 10),
                _ => (text("manual reconciliation"), "end_turn", 10),
            },
            None,
        )
        .await;
        let effects = Arc::new(AtomicUsize::new(0));
        let counter = effects.clone();
        let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
            .max_tokens(128_000)
            .server_tool(nanocodex_claude::ServerToolDefinition::web_fetch_basic(1))
            .tool(tool(), move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Ok("must not run".into()) }
            })
            .build()
            .unwrap();
        assert!(
            agent
                .prompt("perform effect")
                .await
                .unwrap()
                .result()
                .await
                .is_err()
        );
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert_eq!(requests.lock().unwrap().len(), 1);
        agent
            .prompt("reconcile manually")
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        task.abort();
    }
}

// Context recovery sends a real summarization request. The documented thinking
// matrix rejects `thinking: disabled` on Opus 5.5, Sonnet 5.5 and Fable 5.1
// (https://platform.claude.com/docs/en/about-claude/models/extended-thinking-models),
// so Opus 5.5/Fable 5.1 use adaptive thinking at low effort and Sonnet 5.5 its
// lowest setting, between_tools; older models keep the text-only disabled request. No signed pre-summary reasoning may be replayed.
#[tokio::test]
async fn context_recovery_summary_uses_thinking_mode_each_model_accepts() {
    const REJECTS_DISABLED: [&str; 3] =
        ["claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1"];
    const ACCEPTS_DISABLED: [&str; 3] =
        ["claude-opus-4-6", "claude-sonnet-4-6", "claude-haiku-4-5"];
    for model in REJECTS_DISABLED.into_iter().chain(ACCEPTS_DISABLED) {
        let exhausted = vec![
            json!({"type":"thinking","thinking":"","signature":"stale-signature"}),
            json!({"type":"text","text":"partial answer"}),
        ];
        let (client, requests, task) = server(
            move |index, _| match index {
                1 => (pending_round(), "tool_use", 10),
                2 => (exhausted.clone(), "model_context_window_exceeded", 10),
                3 => (text("Preserve the task"), "end_turn", 10),
                _ => (text("completed after recovery"), "end_turn", 10),
            },
            None,
        )
        .await;
        let mut builder = Nanocodex::builder(Claude::new(client, model));
        if model != "claude-haiku-4-5" {
            builder = builder.adaptive_thinking();
        }
        let effects = Arc::new(AtomicUsize::new(0));
        let counter = effects.clone();
        let (agent, _) = builder
            .tool(tool(), move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Ok("receipt".into()) }
            })
            .build()
            .unwrap();
        let result = agent
            .prompt("perform effects once")
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        assert_eq!(
            result.final_message(),
            "completed after recovery",
            "{model}"
        );
        // Both calls in the pending round ran once; recovery never repeats them.
        assert_eq!(effects.load(Ordering::SeqCst), 2, "{model}");
        let log = requests.lock().unwrap();
        assert_eq!(log.len(), 4, "{model}");
        let summary = &log[2];
        assert_eq!(summary["model"], model);
        assert_eq!(summary["tool_choice"], json!({"type":"none"}), "{model}");
        assert!(
            !summary["messages"].to_string().contains("stale-"),
            "{model}"
        );
        // Summary uses the same model maximum as the original request.
        assert_eq!(summary["max_tokens"], log[0]["max_tokens"], "{model}");
        assert_eq!(
            summary["max_tokens"],
            if model == "claude-haiku-4-5" {
                64_000
            } else {
                128_000
            }
        );
        match model {
            "claude-opus-5-5" | "claude-fable-5-1" => {
                assert_eq!(summary["thinking"], json!({"type":"adaptive"}), "{model}");
                assert_eq!(summary["output_config"], json!({"effort":"low"}), "{model}");
            }
            "claude-sonnet-5-5" => {
                assert_eq!(
                    summary["thinking"],
                    json!({"type":"between_tools"}),
                    "{model}"
                );
                assert_eq!(summary["output_config"], json!({"effort":"low"}), "{model}");
            }
            _ => {
                assert_eq!(summary["thinking"], json!({"type":"disabled"}), "{model}");
                assert!(summary.get("output_config").is_none(), "{model}");
            }
        }
        // The recovered task returns to the session's configured policy.
        assert_eq!(log[3]["thinking"], log[0]["thinking"], "{model}");
        assert_eq!(log[3]["output_config"], log[0]["output_config"], "{model}");
        assert!(
            !log[3]["messages"].to_string().contains("stale-"),
            "{model}"
        );
        task.abort();
    }
}

// If a thinking-capable model spends the configured summary budget on
// reasoning, the truncated summary is rejected atomically and the failure leaves
// the retained work available; a later manual compaction can still recover.
#[tokio::test]
async fn context_recovery_summary_truncated_by_max_tokens_fails_without_losing_state() {
    let (client, requests, task) = server(
        |index, _| match index {
            1 => (pending_round(), "tool_use", 10),
            2 => (
                vec![json!({"type":"text","text":"partial answer"})],
                "model_context_window_exceeded",
                10,
            ),
            3 => (
                vec![json!({"type":"thinking","thinking":"","signature":"summary-reasoning"})],
                "max_tokens",
                10,
            ),
            4 => (text("Preserve the task"), "end_turn", 10),
            _ => (text("recovered"), "end_turn", 10),
        },
        None,
    )
    .await;
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let (agent, _) = Nanocodex::builder(Claude::new(client, "claude-opus-5-5"))
        .adaptive_thinking()
        .tool(tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("receipt".into()) }
        })
        .build()
        .unwrap();
    let error = agent
        .prompt("perform effects once")
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("did not end normally"),
        "{error}"
    );
    {
        let log = requests.lock().unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(log[2]["max_tokens"], 128_000);
    }
    // pending_round() has two tool calls; each ran exactly once before the
    // summary was rejected.
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    agent.compact().await.unwrap();
    let result = agent
        .prompt("continue without repeating effects")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "recovered");
    let log = requests.lock().unwrap();
    assert!(!log[4]["messages"].to_string().contains("summary-reasoning"));
    // The rejected summary did not drop completed effects: their receipts are
    // still the input to the later manual summary, and no tool ran again.
    assert!(log[3]["messages"].to_string().contains("receipt"));
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    task.abort();
}
