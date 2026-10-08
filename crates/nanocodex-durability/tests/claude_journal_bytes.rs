//! Long synthetic Claude tool loop through a real SQLite store, measuring the
//! durable journal's write volume and commit time per model round. Each round
//! must persist only new transcript content, not the whole growing request and
//! cursor (which made total journal bytes quadratic in conversation length).
#![cfg(all(feature = "claude", feature = "sqlite"))]

use axum::{Json, Router, routing::post};
use nanocodex_agent::{Nanocodex, PromptRequest};
use nanocodex_claude::{Claude, ClaudeClient, ToolDefinition};
use nanocodex_durability::{
    DurableAgentExt, DurableSession, OwnedState, OwnerId, OwnerToken, SqliteStore, StateStore,
    StoreError, StoreFuture, StoreRecord,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const ROUNDS: usize = 60;
const TOOL_OUTPUT_BYTES: usize = 10_000;

#[derive(Default)]
struct Metrics {
    commits: usize,
    offered_bytes: usize,
    commit_time: Duration,
    largest_commit: usize,
}

struct Measured {
    inner: SqliteStore,
    metrics: Arc<Mutex<Metrics>>,
}

impl StateStore for Measured {
    fn read_record<'a>(
        &'a mut self,
        state_id: &'a str,
        key: &'a str,
    ) -> StoreFuture<'a, Result<Option<String>, StoreError>> {
        self.inner.read_record(state_id, key)
    }
    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedState, StoreError>> {
        self.inner.acquire(state_id, owner)
    }
    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a OwnerToken,
        revision: u64,
        payload: &'a str,
        records: &'a [StoreRecord],
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move {
            let bytes = payload.len()
                + records
                    .iter()
                    .map(|record| record.key.len() + record.value.len())
                    .sum::<usize>();
            if std::env::var_os("NANOCODEX_JOURNAL_BYTES_TRACE").is_some() {
                let detail: Vec<_> = records
                    .iter()
                    .map(|record| {
                        format!(
                            "{}:{}",
                            &record.key[..record.key.len().min(6)],
                            record.value.len()
                        )
                    })
                    .collect();
                eprintln!(
                    "commit rev={revision} head={} records={detail:?}",
                    payload.len()
                );
            }
            let started = Instant::now();
            let result = self
                .inner
                .replace(state_id, owner, revision, payload, records)
                .await;
            let mut metrics = self.metrics.lock().unwrap();
            metrics.commits += 1;
            metrics.offered_bytes += bytes;
            metrics.commit_time += started.elapsed();
            metrics.largest_commit = metrics.largest_commit.max(bytes);
            result
        })
    }
}

fn sse(blocks: Vec<Value>, stop: &str) -> String {
    let mut frames = vec![
        json!({"type":"message_start","message":{"id":"synthetic","role":"assistant","model":"test","content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
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

/// Deterministic non-periodic prose, like real tool guidance.
fn prose(seed: u64, bytes: usize) -> String {
    const WORDS: [&str; 16] = [
        "the",
        "command",
        "workspace",
        "returns",
        "output",
        "when",
        "file",
        "should",
        "path",
        "argument",
        "never",
        "result",
        "process",
        "before",
        "using",
        "current",
    ];
    let mut state = seed.wrapping_add(1);
    let mut text = String::new();
    while text.len() < bytes {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        text.push_str(WORDS[(state >> 60) as usize]);
        text.push(if state >> 56 & 15 == 0 { '.' } else { ' ' });
    }
    text
}

fn tool(index: usize) -> ToolDefinition {
    ToolDefinition {
        name: format!("effect_{index}"),
        description: prose(index as u64, 4_000),
        input_schema: json!({"type":"object","properties":{"command":{"type":"string"}}}),
        strict: None,
        defer_loading: false,
    }
}

fn record_table(path: &std::path::Path) -> (u64, u64) {
    let db = rusqlite::Connection::open(path).unwrap();
    db.query_row(
        "SELECT COUNT(*), COALESCE(SUM(length(key) + length(value)), 0) FROM nanocodex_durable_records",
        [],
        |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn long_tool_loop_journal_grows_with_new_content_not_transcript_squared() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<usize>::new()));
    let log = requests.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let log = log.clone();
            async move {
                let round = {
                    let mut log = log.lock().unwrap();
                    log.push(body.to_string().len());
                    log.len()
                };
                let body = if round <= ROUNDS {
                    sse(
                        vec![json!({"type":"tool_use","id":format!("call-{round}"),"name":"effect_0","input":{"command":format!("step {round}")}})],
                        "tool_use",
                    )
                } else {
                    sse(vec![json!({"type":"text","text":"done"})], "end_turn")
                };
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let metrics = Arc::new(Mutex::new(Metrics::default()));
    let store = Measured {
        inner: SqliteStore::open(&path).unwrap(),
        metrics: metrics.clone(),
    };
    let mut builder = Nanocodex::builder(Claude::new(client, "test")).max_tokens(4096);
    for index in 0..10 {
        builder = builder.tool(tool(index), |input: Value| async move {
            let command = input["command"].as_str().unwrap_or_default().to_owned();
            // Realistic, non-periodic output: numbered lines with varied fields.
            let mut output = String::new();
            let mut line = 0_u64;
            while output.len() < TOOL_OUTPUT_BYTES {
                line += 1;
                let mix = line.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ command.len() as u64;
                output.push_str(&format!(
                    "{command} src/module_{}.rs:{line}: fn item_{mix:x}() -> Result<{}> {{ /* {} */ }}\n",
                    mix % 97,
                    ["u32", "String", "Vec<u8>", "Value"][(mix % 4) as usize],
                    "x".repeat((mix % 23) as usize),
                ));
            }
            Ok(output)
        });
    }
    let (agent, events) = builder
        .durability(
            DurableSession::open(store, "claude-journal-bytes")
                .await
                .unwrap(),
        )
        .await
        .unwrap()
        .build()
        .unwrap();
    let started = Instant::now();
    let result = agent
        .prompt(PromptRequest::new("run the long synthetic loop").request_id("long-loop"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(result.final_message(), "done");
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();

    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), ROUNDS + 1);
    let final_request = *requests.last().unwrap();
    let (rows, stored) = record_table(&path);
    let metrics = metrics.lock().unwrap();
    let summary = json!({
        "rounds": ROUNDS + 1,
        "final_request_bytes": final_request,
        "sum_request_bytes": requests.iter().sum::<usize>(),
        "record_rows": rows,
        "record_bytes_stored": stored,
        "stored_bytes_per_round": stored / (ROUNDS as u64 + 1),
        "commits": metrics.commits,
        "offered_bytes": metrics.offered_bytes,
        "offered_bytes_per_round": metrics.offered_bytes / (ROUNDS + 1),
        "largest_commit_bytes": metrics.largest_commit,
        "commit_time_ms": metrics.commit_time.as_secs_f64() * 1000.0,
        "commit_time_ms_per_round": metrics.commit_time.as_secs_f64() * 1000.0 / (ROUNDS as f64 + 1.0),
        "turn_wall_ms": elapsed.as_secs_f64() * 1000.0,
    });
    eprintln!("journal bytes: {summary}");
    if let Some(output) = std::env::var_os("NANOCODEX_JOURNAL_BYTES_OUTPUT") {
        std::fs::write(output, serde_json::to_string_pretty(&summary).unwrap()).unwrap();
    }
    if std::env::var_os("NANOCODEX_JOURNAL_BYTES_MEASURE_ONLY").is_none() {
        // Linear bound: stored and offered bytes stay within a small multiple of
        // the final transcript, rather than the sum of every round's request.
        assert!(
            (stored as usize) < 8 * final_request,
            "stored {stored} bytes for a {final_request}-byte final request"
        );
        assert!(
            metrics.offered_bytes < 10 * final_request,
            "offered {} bytes for a {final_request}-byte final request",
            metrics.offered_bytes
        );
    }
}
