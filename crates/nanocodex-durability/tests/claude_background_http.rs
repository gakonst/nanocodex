//! Real Messages HTTP/SSE plus SQLite: a held summary overlaps foreground calls,
//! hard limits wait, and committed summary/foreground receipts survive lost ACKs.
#![cfg(all(feature = "claude", feature = "sqlite"))]

use axum::{Json, Router, routing::post};
use nanocodex_agent::{Nanocodex, PromptRequest};
use nanocodex_claude::{Claude, ClaudeClient, ToolDefinition};
use nanocodex_durability::{DurableAgentExt, DurableSession, SqliteStore, StateStore, StepStatus};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, Semaphore};

const SUMMARY: &str = "Immutable original request summary";

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

fn round(index: usize) -> Vec<Value> {
    vec![
        json!({"type":"thinking","thinking":format!("round {index}"),"signature":format!("signed-{index}"),"binding":{"opaque":index}}),
        json!({"type":"redacted_thinking","data":format!("redacted-{index}"),"binding":"unchanged"}),
        json!({"type":"tool_use","id":format!("effect-{index}"),"name":"effect","input":{"round":index},"caller":{"type":"direct"}}),
    ]
}

fn receipt(index: usize) -> Vec<Value> {
    vec![
        json!({"type":"text","text":format!("committed effect {index}")}),
        json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"cG5n"}}),
    ]
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

struct Fixture {
    log: Mutex<Vec<Value>>,
    foreground: AtomicUsize,
    summaries: AtomicUsize,
    changed: Notify,
    summary_gate: Semaphore,
    foreground_gate: Semaphore,
    held_foreground: usize,
    tool_rounds: usize,
    hard_second: bool,
    arm_lost_ack: Arc<AtomicBool>,
}

impl Fixture {
    fn new(
        held_foreground: usize,
        tool_rounds: usize,
        hard_second: bool,
        arm_lost_ack: Arc<AtomicBool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            log: Mutex::new(Vec::new()),
            foreground: AtomicUsize::new(0),
            summaries: AtomicUsize::new(0),
            changed: Notify::new(),
            summary_gate: Semaphore::new(0),
            foreground_gate: Semaphore::new(0),
            held_foreground,
            tool_rounds,
            hard_second,
            arm_lost_ack,
        })
    }

    fn requests(&self) -> Vec<Value> {
        self.log.lock().unwrap().clone()
    }

    async fn wait_for(&self, foreground: usize, summaries: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let changed = self.changed.notified();
                if self.foreground.load(Ordering::SeqCst) >= foreground
                    && self.summaries.load(Ordering::SeqCst) >= summaries
                {
                    return;
                }
                changed.await;
            }
        })
        .await
        .expect("expected HTTP requests did not arrive");
    }
}

async fn server(fixture: Arc<Fixture>) -> (ClaudeClient, tokio::task::JoinHandle<()>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let fixture = fixture.clone();
            async move {
                fixture.log.lock().unwrap().push(body.clone());
                let summary = body["tool_choice"]["type"] == "none";
                let index = if summary {
                    fixture.summaries.fetch_add(1, Ordering::SeqCst) + 1
                } else {
                    fixture.foreground.fetch_add(1, Ordering::SeqCst) + 1
                };
                fixture.changed.notify_one();
                if std::env::var_os("NANOCLAUDE_DURABILITY_TRACE").is_some() {
                    eprintln!(
                        "{}",
                        json!({"summary":summary,"ordinal":index,"request":body})
                    );
                }
                let response = if summary {
                    fixture.summary_gate.acquire().await.unwrap().forget();
                    sse(vec![json!({"type":"text","text":SUMMARY})], "end_turn", 10)
                } else {
                    if index == fixture.held_foreground {
                        fixture.foreground_gate.acquire().await.unwrap().forget();
                        // Only the recovery journey arms this fault, after the test
                        // has observed the summary's authoritative completed receipt.
                        if fixture.tool_rounds == 1 && fixture.held_foreground == 2 {
                            fixture.arm_lost_ack.store(true, Ordering::SeqCst);
                        }
                    }
                    if index <= fixture.tool_rounds {
                        sse(
                            round(index),
                            "tool_use",
                            if fixture.hard_second && index == 2 {
                                70_000
                            } else {
                                55_000
                            },
                        )
                    } else {
                        sse(
                            vec![json!({"type":"text","text":"completed once"})],
                            "end_turn",
                            10,
                        )
                    }
                };
                ([("content-type", "text/event-stream")], response)
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
        task,
    )
}

async fn reopen(path: &std::path::Path) -> DurableSession {
    DurableSession::open(SqliteStore::open(path).unwrap(), "claude-background-http")
        .await
        .unwrap()
}

async fn wait_summary_receipt(session: &DurableSession) {
    // Inspect the public execution receipts, not provider-private cursor fields.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            for operation in session.state().await.unwrap().operations().values() {
                for step in operation.steps.values() {
                    if let StepStatus::Completed(output) = &step.status {
                        let value: Value = session.resolve(output).await.unwrap().decode().unwrap();
                        if value.to_string().contains(SUMMARY) {
                            return;
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("summary HTTP response never acquired a completed SQLite receipt");
}

fn foreground_requests(log: &[Value]) -> Vec<&Value> {
    log.iter()
        .filter(|body| body["tool_choice"]["type"] != "none")
        .collect()
}

fn assert_rounds(messages: &[Value], rounds: usize) {
    assert_eq!(
        messages.len(),
        rounds * 2 + 1,
        "summary plus complete lossless rounds"
    );
    assert!(messages[0]["content"].to_string().contains(SUMMARY));
    for index in 1..=rounds {
        assert_eq!(messages[index * 2 - 1]["role"], "assistant");
        assert_eq!(messages[index * 2 - 1]["content"], json!(round(index)));
        assert_eq!(messages[index * 2]["role"], "user");
        assert_eq!(messages[index * 2]["content"][0]["type"], "tool_result");
        assert_eq!(
            messages[index * 2]["content"][0]["tool_use_id"],
            format!("effect-{index}")
        );
        assert_eq!(
            messages[index * 2]["content"][0]["content"],
            json!(receipt(index))
        );
    }
}

#[tokio::test]
async fn held_summary_overlaps_foreground_and_preserves_exact_tail_after_sqlite_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let fixture = Fixture::new(3, 3, false, Arc::new(AtomicBool::new(false)));
    let (client, server) = server(fixture.clone()).await;
    let session = reopen(&path).await;
    let observer = session.clone();
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .auto_compact_window_tokens(100_000)
        .tool_blocks(tool(), move |input| {
            counter.fetch_add(1, Ordering::SeqCst);
            async move { Ok(receipt(input["round"].as_u64().unwrap() as usize)) }
        })
        .durability(session)
        .await
        .unwrap()
        .build()
        .unwrap();
    let turn = agent
        .prompt(PromptRequest::new("retain original constraint").request_id("overlap"))
        .await
        .unwrap();
    fixture.wait_for(3, 1).await;
    assert_eq!(
        effects.load(Ordering::SeqCst),
        2,
        "two completed effects while summary is held"
    );
    let held = fixture.requests();
    let foreground = foreground_requests(&held);
    let summary = held
        .iter()
        .find(|body| body["tool_choice"]["type"] == "none")
        .unwrap();
    assert_eq!(summary["messages"][0], foreground[0]["messages"][0]);
    assert_eq!(
        summary["messages"].as_array().unwrap().len(),
        2,
        "summary input is immutable prefix plus trigger; first signed round is retained"
    );
    assert!(!summary["messages"].to_string().contains("signed-2"));
    assert_eq!(foreground[2]["messages"][3]["content"], json!(round(2)));
    fixture.summary_gate.add_permits(1);
    wait_summary_receipt(&observer).await;
    fixture.foreground_gate.add_permits(1);
    let result = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.final_message(), "completed once");
    assert_eq!(effects.load(Ordering::SeqCst), 3);
    let log = fixture.requests();
    let foreground = foreground_requests(&log);
    assert_eq!(foreground.len(), 4);
    assert_eq!(fixture.summaries.load(Ordering::SeqCst), 1);
    assert_rounds(foreground[3]["messages"].as_array().unwrap(), 3);
    assert_eq!(foreground[3]["container"], "stable-container");
    assert_eq!(foreground[3]["tools"], foreground[0]["tools"]);
    agent.shutdown().await.unwrap();
    drop((agent, events, observer));

    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .auto_compact_window_tokens(100_000)
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    agent
        .prompt(PromptRequest::new("continue restored session").request_id("after-reopen"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = fixture.requests();
    let foreground = foreground_requests(&log);
    assert_eq!(foreground.len(), 5);
    let restored = foreground[4]["messages"].as_array().unwrap();
    assert_eq!(
        &restored[..7],
        foreground[3]["messages"].as_array().unwrap()
    );
    assert_eq!(
        effects.load(Ordering::SeqCst),
        3,
        "SQLite recovery must preserve completed effects"
    );
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
}

#[tokio::test]
async fn hard_limit_waits_for_held_summary_before_next_foreground_http_request() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(0, 2, true, Arc::new(AtomicBool::new(false)));
    let (client, server) = server(fixture.clone()).await;
    let effects = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(Notify::new());
    let counter = effects.clone();
    let notify = completed.clone();
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .auto_compact_window_tokens(100_000)
        .tool_blocks(tool(), move |input| {
            counter.fetch_add(1, Ordering::SeqCst);
            notify.notify_one();
            async move { Ok(receipt(input["round"].as_u64().unwrap() as usize)) }
        })
        .durability(reopen(&directory.path().join("state.sqlite")).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let turn = agent
        .prompt(PromptRequest::new("observe hard admission boundary").request_id("hard-limit"))
        .await
        .unwrap();
    fixture.wait_for(2, 1).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while effects.load(Ordering::SeqCst) < 2 {
            completed.notified().await;
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), fixture.wait_for(3, 1))
            .await
            .is_err(),
        "hard threshold must not admit a third provider request while summary is held"
    );
    fixture.summary_gate.add_permits(1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), turn.result())
            .await
            .unwrap()
            .unwrap()
            .final_message(),
        "completed once"
    );
    let log = fixture.requests();
    let foreground = foreground_requests(&log);
    assert_eq!(foreground.len(), 3);
    assert_rounds(foreground[2]["messages"].as_array().unwrap(), 2);
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
}

struct LostAckStore {
    inner: SqliteStore,
    armed: Arc<AtomicBool>,
}
impl StateStore for LostAckStore {
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
            let lost_ack = self.armed.swap(false, Ordering::SeqCst);
            let committed = self
                .inner
                .replace(id, owner, revision, payload, records)
                .await?;
            if lost_ack {
                return Err(nanocodex_durability::StoreError::Backend(
                    "synthetic lost foreground receipt acknowledgement".into(),
                ));
            }
            Ok(committed)
        })
    }
}

#[tokio::test]
async fn completed_summary_and_foreground_receipt_recover_without_http_redispatch() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let armed = Arc::new(AtomicBool::new(false));
    let fixture = Fixture::new(2, 1, false, armed.clone());
    let (client, server) = server(fixture.clone()).await;
    let session = DurableSession::open(
        LostAckStore {
            inner: SqliteStore::open(&path).unwrap(),
            armed,
        },
        "claude-background-http",
    )
    .await
    .unwrap();
    let observer = session.clone();
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let request =
        || PromptRequest::new("recover original foreground receipt").request_id("receipt-recovery");
    let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .auto_compact_window_tokens(100_000)
        .tool_blocks(tool(), move |input| {
            counter.fetch_add(1, Ordering::SeqCst);
            async move { Ok(receipt(input["round"].as_u64().unwrap() as usize)) }
        })
        .durability(session)
        .await
        .unwrap()
        .build()
        .unwrap();
    let turn = agent.prompt(request()).await.unwrap();
    fixture.wait_for(2, 1).await;
    fixture.summary_gate.add_permits(1);
    wait_summary_receipt(&observer).await;
    fixture.foreground_gate.add_permits(1);
    let error = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("lost foreground receipt acknowledgement"),
        "{error}"
    );
    let _ = agent.shutdown().await;
    drop((agent, events, observer));
    let calls = fixture.requests().len();
    assert_eq!(calls, 3, "two foreground responses plus completed summary");

    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .auto_compact_window_tokens(100_000)
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
    assert_eq!(result.final_message(), "completed once");
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.requests().len(),
        calls,
        "completed foreground and summary receipts must replay without any HTTP redispatch"
    );
    let replay = agent
        .prompt(request())
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(replay.final_message(), result.final_message());
    assert_eq!(
        replay.usage().unwrap().total_tokens(),
        result.usage().unwrap().total_tokens()
    );
    assert_eq!(
        fixture.requests().len(),
        calls,
        "terminal request receipt must also replay"
    );
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
}

// Observe the real SQLite rejection so the held foreground cannot race the
// stale summary receipt. This wrapper does not inject a store failure.
struct ObserveFencedSummaryStore {
    inner: SqliteStore,
    rejected_summary: Arc<Notify>,
}

impl StateStore for ObserveFencedSummaryStore {
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
            let summary = records.iter().any(|record| record.value.contains(SUMMARY));
            let result = self
                .inner
                .replace(id, owner, revision, payload, records)
                .await;
            if summary && matches!(&result, Err(nanocodex_durability::StoreError::Fenced)) {
                self.rejected_summary.notify_one();
            }
            result
        })
    }
}

fn sqlite_execution_head(path: &std::path::Path) -> (i64, String, Vec<(String, String)>) {
    // A new session's state() is a cached head. Read the authoritative database
    // directly to prove late work changed neither that head nor immutable records.
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let (revision, payload) = connection
        .query_row(
            "SELECT revision, payload FROM nanocodex_durable_states WHERE state_id = ?1",
            ["claude-background-http"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut statement = connection
        .prepare(
            "SELECT key, value FROM nanocodex_durable_records WHERE state_id = ?1 ORDER BY key",
        )
        .unwrap();
    let records = statement
        .query_map(["claude-background-http"], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    (revision, payload, records)
}

#[tokio::test]
async fn sqlite_owner_takeover_fences_held_summary_and_foreground_then_recovers() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let fixture = Fixture::new(3, 3, false, Arc::new(AtomicBool::new(false)));
    let (client, server) = server(fixture.clone()).await;
    let rejected_summary = Arc::new(Notify::new());
    let session = DurableSession::open(
        ObserveFencedSummaryStore {
            inner: SqliteStore::open(&path).unwrap(),
            rejected_summary: rejected_summary.clone(),
        },
        "claude-background-http",
    )
    .await
    .unwrap();
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let request = || {
        PromptRequest::new("retain original constraint through owner takeover")
            .request_id("owner-takeover")
    };
    let (older, older_events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .auto_compact_window_tokens(100_000)
        .tool_blocks(tool(), move |input| {
            counter.fetch_add(1, Ordering::SeqCst);
            async move { Ok(receipt(input["round"].as_u64().unwrap() as usize)) }
        })
        .durability(session)
        .await
        .unwrap()
        .build()
        .unwrap();
    let old_turn = older.prompt(request()).await.unwrap();
    fixture.wait_for(3, 1).await;
    assert_eq!(effects.load(Ordering::SeqCst), 2);

    // A separate connection and DurableSession atomically replace store authority;
    // cloning the live session would exercise only in-process ownership.
    let authoritative = reopen(&path).await;
    let head = sqlite_execution_head(&path);
    assert!(
        head.2.iter().all(|(_, value)| !value.contains(SUMMARY)),
        "held summary must not already have a durable receipt"
    );
    fixture.summary_gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(10), rejected_summary.notified())
        .await
        .expect("late summary must attempt and fail its real SQLite receipt write");
    assert_eq!(
        sqlite_execution_head(&path),
        head,
        "a fenced summary must not publish any head or immutable payload mutation"
    );
    assert_eq!(fixture.foreground.load(Ordering::SeqCst), 3);
    fixture.foreground_gate.add_permits(1);
    let error = tokio::time::timeout(Duration::from_secs(10), old_turn.result())
        .await
        .unwrap()
        .expect_err("the old turn must fail after independent SQLite owner takeover");
    assert_eq!(
        error.execution_policy_disposition(),
        Some(nanocodex_agent::ExecutionPolicyDisposition::Reopen),
        "{error}"
    );
    // The summary receipt already proved a real StoreError::Fenced above.
    // That fence can stop the owner driver before foreground completion, so
    // the public error may report the stopped driver with the same Reopen
    // disposition. Authoritative SQLite equality below proves no publication.
    assert_eq!(
        sqlite_execution_head(&path),
        head,
        "late foreground and stale failure handling must also leave authority unchanged"
    );
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    let _ = older.shutdown().await;
    drop((older, older_events));

    let counter = effects.clone();
    let (recovered, recovered_events) = Nanocodex::builder(Claude::new(client, "test"))
        .auto_compact_window_tokens(100_000)
        .tool_blocks(tool(), move |input| {
            counter.fetch_add(1, Ordering::SeqCst);
            async move { Ok(receipt(input["round"].as_u64().unwrap() as usize)) }
        })
        .durability(authoritative)
        .await
        .unwrap()
        .build()
        .unwrap();
    fixture.summary_gate.add_permits(1);
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        recovered.prompt(request()).await.unwrap().result(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.final_message(), "completed once");
    assert_eq!(
        effects.load(Ordering::SeqCst),
        2,
        "new owner must replay both completed effects and reject the old late tool response"
    );
    let log = fixture.requests();
    let foreground = foreground_requests(&log);
    assert_eq!(foreground.len(), 4);
    assert_eq!(
        foreground[3], foreground[2],
        "new owner must resend the exact frozen pending foreground HTTP request"
    );
    assert_eq!(fixture.summaries.load(Ordering::SeqCst), 2);
    let calls = log.len();
    let replay = recovered
        .prompt(request())
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(replay.final_message(), result.final_message());
    assert_eq!(replay.usage(), result.usage());
    assert_eq!(
        fixture.requests().len(),
        calls,
        "terminal receipt must replay without HTTP"
    );
    // Claude exposes the restored context at its next real Messages request.
    // The backend deliberately does not implement the OAI snapshot API.
    tokio::time::timeout(Duration::from_secs(10), async {
        recovered
            .prompt("continue the recovered task")
            .await
            .unwrap()
            .result()
            .await
    })
    .await
    .unwrap()
    .unwrap();
    let continued = fixture.requests();
    let history = foreground_requests(&continued).last().unwrap()["messages"].to_string();
    for retained in [
        SUMMARY,
        "signed-1",
        "signed-2",
        "committed effect 1",
        "committed effect 2",
    ] {
        assert!(history.contains(retained), "missing {retained}: {history}");
    }
    assert!(
        !history.contains("signed-3"),
        "stale foreground tool response leaked into recovery"
    );
    println!(
        "sqlite_owner_takeover=true stale_summary_write_fenced=true authoritative_head_and_records_unchanged=true frozen_foreground_replayed=true completed_effects_replayed=true"
    );
    recovered.shutdown().await.unwrap();
    drop((recovered, recovered_events));
    server.abort();
}
