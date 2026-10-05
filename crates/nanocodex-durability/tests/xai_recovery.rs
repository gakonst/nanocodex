//! Native Responses HTTP/SSE + reopened SQLite journeys. Faults occur at the
//! public host-store boundary after observable effects, before or after commit.
#![cfg(all(feature = "xai", feature = "sqlite"))]

use axum::{Json, Router, routing::post};
use nanocodex_agent::{Nanocodex, PromptRequest};
use nanocodex_durability::{
    DurableAgentExt, DurableSession, OwnedState, OwnerId, OwnerToken, SqliteStore, StateStore,
    StoreError, StoreFuture, StoreRecord,
};
use nanocodex_xai::{ToolDefinition, Xai, XaiClient};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

fn completion(output: Vec<Value>) -> String {
    let frame = json!({"type":"response.completed","response":{"id":"synthetic-response", "status":"completed", "output":output,
        "usage":{"input_tokens":8,"output_tokens":2,"total_tokens":10}}});
    format!("event: response.completed\ndata: {frame}\n\n")
}
fn answer() -> String {
    completion(vec![
        json!({"type":"message","id":"answer","role":"assistant","status":"completed",
        "content":[{"type":"output_text","text":"effect reconciled","annotations":[]} ]}),
    ])
}
fn effect() -> String {
    completion(vec![
        json!({"type":"reasoning","id":"opaque-reasoning","encrypted_content":"opaque-signed-ciphertext", "summary":[]}),
        json!({"type":"function_call","id":"item-effect","call_id":"effect-once","name":"effect", "arguments":"{\"key\":\"synthetic\"}","status":"completed"}),
    ])
}
async fn server(
    reply: impl Fn(usize, &Value) -> String + Send + Sync + 'static,
) -> (
    XaiClient,
    Arc<Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    server_http(move |index, body| (200, reply(index, body))).await
}
async fn server_http(
    reply: impl Fn(usize, &Value) -> (u16, String) + Send + Sync + 'static,
) -> (
    XaiClient,
    Arc<Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    let reply = Arc::new(reply);
    let app = Router::new().route(
        "/v1/responses",
        post(move |Json(body): Json<Value>| {
            let log = log.clone();
            let reply = reply.clone();
            async move {
                let index = {
                    let mut log = log.lock().unwrap();
                    log.push(body.clone());
                    log.len()
                };
                let (status, response) = reply(index, &body);
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    [("content-type", "text/event-stream")],
                    response,
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        XaiClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/responses"),
            "synthetic",
        ),
        requests,
        server,
    )
}
fn recipe(client: XaiClient, effects: Arc<AtomicUsize>, arm: Option<Arc<AtomicBool>>) -> Xai {
    Xai::new(client, "original-model").tool(
        ToolDefinition {
            name: "effect".into(),
            description: "Synthetic observed effect".into(),
            parameters: json!({"type":"object"}),
        },
        move |_| {
            effects.fetch_add(1, Ordering::SeqCst);
            if let Some(arm) = &arm {
                arm.store(true, Ordering::SeqCst);
            }
            async { Ok("committed synthetic receipt".into()) }
        },
    )
}
async fn reopen(path: &std::path::Path) -> DurableSession {
    DurableSession::open(SqliteStore::open(path).unwrap(), "xai-synthetic")
        .await
        .unwrap()
}
async fn result(
    agent: &Nanocodex,
    id: &str,
    text: &str,
) -> nanocodex_agent::Result<nanocodex_agent::TurnResult> {
    agent
        .prompt(PromptRequest::new(text).request_id(id))
        .await?
        .result()
        .await
}
fn evidence(name: &str, requests: &[Value], effects: usize, note: &str) {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/xai-durability");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&json!({
            "scenario":name,"observed_effects":effects,"outcome":note,"requests":requests
        }))
        .unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn terminal_receipts_native_history_and_stale_owner_survive_sqlite_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) =
        server(|index, _| if index == 1 { effect() } else { answer() }).await;
    let effects = Arc::new(AtomicUsize::new(0));
    let (old, old_events) = recipe(client.clone(), effects.clone(), None)
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let first = result(&old, "first", "perform synthetic effect")
        .await
        .unwrap();
    assert_eq!(first.final_message(), "effect reconciled");
    let (current, events) = recipe(client, effects.clone(), None)
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let replay = result(&current, "first", "perform synthetic effect")
        .await
        .unwrap();
    assert_eq!(first.usage(), replay.usage());
    assert_eq!(
        requests.lock().unwrap().len(),
        2,
        "terminal replay must not make HTTP requests"
    );
    assert!(result(&current, "first", "different input").await.is_err());
    let stale = result(&old, "stale", "must be fenced").await.unwrap_err();
    assert!(stale.execution_policy_disposition().is_some(), "{stale}");
    assert_eq!(
        requests.lock().unwrap().len(),
        2,
        "stale writer must fail before HTTP"
    );
    result(&current, "next", "explicit next request")
        .await
        .unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let log = requests.lock().unwrap().clone();
    assert_eq!(log.len(), 3);
    assert!(
        log[2]["input"]
            .to_string()
            .contains("opaque-signed-ciphertext")
    );
    assert!(
        log[2]["input"]
            .to_string()
            .contains("committed synthetic receipt")
    );
    evidence(
        "terminal-and-owner-fencing",
        &log,
        1,
        "terminal replay made zero HTTP calls; stale owner rejected before HTTP; native opaque output retained",
    );
    current.shutdown().await.unwrap();
    let _ = old.shutdown().await;
    drop((current, events, old, old_events));
    server.abort();
}

struct InterruptedStore {
    writes: Arc<AtomicUsize>,
    fail_at: Option<usize>,
    inner: SqliteStore,
    armed: Arc<AtomicBool>,
    after_commit: bool,
}
impl StateStore for InterruptedStore {
    fn read_record<'a>(
        &'a mut self,
        id: &'a str,
        key: &'a str,
    ) -> StoreFuture<'a, Result<Option<String>, StoreError>> {
        self.inner.read_record(id, key)
    }
    fn acquire<'a>(
        &'a mut self,
        id: &'a str,
        owner: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedState, StoreError>> {
        self.inner.acquire(id, owner)
    }
    fn replace<'a>(
        &'a mut self,
        id: &'a str,
        owner: &'a OwnerToken,
        revision: u64,
        payload: &'a str,
        records: &'a [StoreRecord],
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move {
            let ordinal = self.writes.fetch_add(1, Ordering::SeqCst);
            if self.fail_at == Some(ordinal) || self.armed.swap(false, Ordering::SeqCst) {
                if self.after_commit {
                    self.inner
                        .replace(id, owner, revision, payload, records)
                        .await?;
                }
                return Err(StoreError::Backend(
                    "synthetic process interruption at receipt write".into(),
                ));
            }
            self.inner
                .replace(id, owner, revision, payload, records)
                .await
        })
    }
}

#[tokio::test]
async fn tool_receipt_loss_replays_only_committed_receipts_and_fences_uncertainty() {
    for after_commit in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let (client, requests, server) =
            server(|index, _| if index == 1 { effect() } else { answer() }).await;
        let effects = Arc::new(AtomicUsize::new(0));
        let armed = Arc::new(AtomicBool::new(false));
        let state = DurableSession::open(
            InterruptedStore {
                writes: Arc::new(AtomicUsize::new(0)),
                fail_at: None,
                inner: SqliteStore::open(&path).unwrap(),
                armed: armed.clone(),
                after_commit,
            },
            "xai-synthetic",
        )
        .await
        .unwrap();
        let (agent, events) = recipe(client.clone(), effects.clone(), Some(armed))
            .durability(state)
            .await
            .unwrap()
            .build()
            .unwrap();
        let interrupted = result(&agent, "effect-request", "perform synthetic effect")
            .await
            .unwrap_err();
        assert!(
            interrupted.execution_policy_disposition().is_some(),
            "{interrupted}"
        );
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        let _ = agent.shutdown().await;
        drop((agent, events));
        // Different recipe must not alter the admitted model request. Recovered
        // receipts must work even though the original handler is absent.
        let (agent, events) = Xai::new(client, "replacement-model")
            .durability(reopen(&path).await)
            .await
            .unwrap()
            .build()
            .unwrap();
        let recovered = result(&agent, "effect-request", "perform synthetic effect").await;
        if after_commit {
            assert_eq!(recovered.unwrap().final_message(), "effect reconciled");
        } else {
            let error = recovered.unwrap_err().to_string();
            assert!(
                error.contains("unknown") || error.contains("uncertain"),
                "{error}"
            );
            assert_eq!(
                requests.lock().unwrap().len(),
                1,
                "uncertain effect must stop before another provider request"
            );
            assert!(
                result(&agent, "effect-request", "perform synthetic effect")
                    .await
                    .is_err()
            );
            result(
                &agent,
                "safe-next",
                "inspect the existing effect before taking further action",
            )
            .await
            .unwrap();
        }
        assert_eq!(
            effects.load(Ordering::SeqCst),
            1,
            "started effect must never be repeated"
        );
        let log = requests.lock().unwrap().clone();
        assert_eq!(log.len(), 2);
        let native = log[1]["input"].to_string();
        assert!(native.contains("opaque-signed-ciphertext"));
        if after_commit {
            assert!(native.contains("committed synthetic receipt"));
            assert_eq!(log[1]["model"], "original-model");
        } else {
            assert!(native.contains("unknown") || native.contains("uncertain"));
        }
        evidence(
            if after_commit {
                "tool-lost-ack"
            } else {
                "tool-uncommitted-receipt"
            },
            &log,
            1,
            if after_commit {
                "reopened receipt restored without handler; no duplicate effect"
            } else {
                "started tool fenced; old request failed; explicit new prompt retained uncertainty"
            },
        );
        agent.shutdown().await.unwrap();
        drop((agent, events));
        server.abort();
    }
}

#[tokio::test]
async fn started_provider_request_without_receipt_is_never_automatically_reissued() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let armed = Arc::new(AtomicBool::new(false));
    let arm = armed.clone();
    let (client, requests, server) = server(move |index, _| {
        if index == 1 {
            arm.store(true, Ordering::SeqCst);
        }
        answer()
    })
    .await;
    let state = DurableSession::open(
        InterruptedStore {
            writes: Arc::new(AtomicUsize::new(0)),
            fail_at: None,
            inner: SqliteStore::open(&path).unwrap(),
            armed,
            after_commit: false,
        },
        "xai-synthetic",
    )
    .await
    .unwrap();
    let (agent, events) = Xai::new(client.clone(), "test")
        .web_search()
        .durability(state)
        .await
        .unwrap()
        .build()
        .unwrap();
    assert!(
        result(&agent, "remote", "perform hosted search once")
            .await
            .unwrap_err()
            .execution_policy_disposition()
            .is_some()
    );
    let _ = agent.shutdown().await;
    drop((agent, events));
    let (agent, events) = Xai::new(client, "test")
        .web_search()
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    assert!(
        result(&agent, "remote", "perform hosted search once")
            .await
            .is_err()
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "unknown remote effects prohibit automatic HTTP replay"
    );
    result(
        &agent,
        "safe-next",
        "continue after checking the uncertain remote outcome",
    )
    .await
    .unwrap();
    let log = requests.lock().unwrap().clone();
    assert_eq!(log.len(), 2);
    assert!(
        log[1]["input"].to_string().contains("unknown")
            || log[1]["input"].to_string().contains("uncertain")
    );
    evidence(
        "provider-uncertainty",
        &log,
        1,
        "started Responses request was not repeated; new explicit prompt preserved uncertainty",
    );
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
}

#[tokio::test]
async fn portable_memory_store_reopens_and_replays_the_same_public_request() {
    let (client, requests, server) = server(|_, _| answer()).await;
    let store = nanocodex_durability::MemoryStore::new().unwrap();
    for _ in 0..2 {
        let session = DurableSession::open(store.clone(), "portable-xai")
            .await
            .unwrap();
        let (agent, events) = Xai::new(client.clone(), "test")
            .durability(session)
            .await
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            result(&agent, "same-id", "same input")
                .await
                .unwrap()
                .final_message(),
            "effect reconciled"
        );
        agent.shutdown().await.unwrap();
        drop((agent, events));
    }
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 1);
    evidence(
        "portable-memory-store",
        &log,
        0,
        "reopened portable store replayed exact terminal receipt with no extra HTTP",
    );
    server.abort();
}

// Discover the terminal commit through an actual successful public journey,
// then interrupt that last write on either side of the atomic store boundary.
// A terminal model response must remain a receipt until the request settles.
async fn terminal_commit_journey(fail_at: Option<usize>, after_commit: bool) -> usize {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let (client, requests, server) =
        server(|index, _| if index == 1 { effect() } else { answer() }).await;
    let writes = Arc::new(AtomicUsize::new(0));
    let effects = Arc::new(AtomicUsize::new(0));
    let state = DurableSession::open(
        InterruptedStore {
            inner: SqliteStore::open(&path).unwrap(),
            armed: Arc::new(AtomicBool::new(false)),
            writes: writes.clone(),
            fail_at,
            after_commit,
        },
        "xai-synthetic",
    )
    .await
    .unwrap();
    let (agent, events) = recipe(client.clone(), effects.clone(), None)
        .durability(state)
        .await
        .unwrap()
        .build()
        .unwrap();
    let first = result(&agent, "terminal", "effect and answer").await;
    let count = writes.load(Ordering::SeqCst);
    if fail_at.is_none() {
        first.unwrap();
    } else {
        assert!(first.unwrap_err().execution_policy_disposition().is_some());
    }
    let _ = agent.shutdown().await;
    drop((agent, events));
    let (agent, events) = Xai::new(client, "replacement")
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let recovered = result(&agent, "terminal", "effect and answer")
        .await
        .unwrap();
    assert_eq!(recovered.final_message(), "effect reconciled");
    assert_eq!(recovered.usage().unwrap().total_tokens(), 20);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let log = requests.lock().unwrap().clone();
    assert_eq!(
        log.len(),
        2,
        "terminal commit loss must not generate another response"
    );
    evidence(
        if fail_at.is_none() {
            "terminal-baseline"
        } else if after_commit {
            "terminal-lost-ack"
        } else {
            "terminal-precommit"
        },
        &log,
        1,
        "terminal response and usage restored exactly without any new provider request",
    );
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
    count
}
#[tokio::test]
async fn terminal_commit_interruption_never_generates_an_additional_response() {
    let count = terminal_commit_journey(None, false).await;
    for after_commit in [false, true] {
        terminal_commit_journey(Some(count - 1), after_commit).await;
    }
}

#[tokio::test]
async fn rejection_and_compaction_receipts_recover_exactly_without_repeating_requests() {
    for scenario in ["http-rejection", "auto-summary", "failed-summary"] {
        for after_commit in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("state.sqlite");
            let armed = Arc::new(AtomicBool::new(false));
            let arm = armed.clone();
            let (client, requests, server) = server_http(move |index, body| {
                let summary = body["input"][0]["content"].as_str().is_some_and(|s|s.starts_with("Summarize"));
                if index == 2 {
                    arm.store(true, Ordering::SeqCst);
                    if scenario == "http-rejection" || scenario == "failed-summary" {
                        assert_eq!(summary, scenario == "failed-summary");
                        return (503, "known provider rejection".into());
                    }
                    assert!(summary);
                    return (200, completion(vec![json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"Retained objective: preserve violet and continue."}]})]));
                }
                (200, answer())
            }).await;
            let state = DurableSession::open(
                InterruptedStore {
                    inner: SqliteStore::open(&path).unwrap(),
                    armed,
                    writes: Arc::new(AtomicUsize::new(0)),
                    fail_at: None,
                    after_commit,
                },
                "xai-synthetic",
            )
            .await
            .unwrap();
            let window = if scenario == "http-rejection" {
                500_000
            } else {
                1000
            };
            let (agent, events) = Xai::new(client.clone(), "grok-4.6")
                .context_window_tokens(window)
                .compaction_keep_tail(0)
                .max_retries(2)
                .durability(state)
                .await
                .unwrap()
                .build()
                .unwrap();
            result(&agent, "history", &"violet historical detail ".repeat(200))
                .await
                .unwrap();
            let failure = result(&agent, "interrupted", "continue the current task")
                .await
                .unwrap_err();
            assert!(
                failure.execution_policy_disposition().is_some(),
                "{failure}"
            );
            assert_eq!(requests.lock().unwrap().len(), 2);
            let _ = agent.shutdown().await;
            drop((agent, events));
            let (agent, events) = Xai::new(client, "replacement-model")
                .max_retries(0)
                .context_window_tokens(50_000)
                .durability(reopen(&path).await)
                .await
                .unwrap()
                .build()
                .unwrap();
            let recovered = result(&agent, "interrupted", "continue the current task").await;
            if after_commit {
                assert_eq!(recovered.unwrap().final_message(), "effect reconciled");
                let log = requests.lock().unwrap();
                assert_eq!(
                    log.len(),
                    3,
                    "{scenario} must replay receipt without extra HTTP"
                );
                assert_eq!(log[2]["model"], "grok-4.6", "admitted model retained");
                let input = log[2]["input"].to_string();
                assert!(input.contains("continue the current task"));
                if scenario == "auto-summary" {
                    assert!(input.contains("Retained objective"));
                    assert!(!input.contains("violet historical detail"));
                } else {
                    assert!(input.contains("violet historical detail"));
                }
            } else {
                let error = recovered.unwrap_err().to_string();
                assert!(
                    error.contains("unknown") || error.contains("uncertain"),
                    "{error}"
                );
                assert_eq!(
                    requests.lock().unwrap().len(),
                    2,
                    "uncommitted provider effect must not replay"
                );
            }
            let log = requests.lock().unwrap().clone();
            evidence(
                &format!(
                    "{scenario}-{}",
                    if after_commit {
                        "lost-ack"
                    } else {
                        "uncommitted"
                    }
                ),
                &log,
                0,
                "reopened SQLite consumes exact committed receipt; missing receipt fences request without resampling",
            );
            agent.shutdown().await.unwrap();
            drop((agent, events));
            server.abort();
            println!(
                "SQLite+HTTP journey: {scenario}, after_commit={after_commit}, physical requests={}",
                log.len()
            );
        }
    }
}

#[tokio::test]
async fn durable_empty_prompt_limit_compacts_once_and_transient_rejections_are_bounded() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("context.sqlite");
    let (client, requests, server)=server_http(|index,body| {
        if index==2 { return (200,format!("data: {}\n\n",json!({"type":"response.incomplete","response":{"status":"incomplete","output":[],"incomplete_details":{"reason":"max_prompt_tokens"}}}))); }
        if index==3 {
            assert!(body["input"][0]["content"].as_str().unwrap().starts_with("Summarize"));
            return (200,completion(vec![json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"Retained violet task."}]})]));
        }
        (200,answer())
    }).await;
    let (agent, _) = Xai::new(client, "grok-4.6")
        .compaction_keep_tail(0)
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    result(&agent, "old", &"history violet ".repeat(200))
        .await
        .unwrap();
    let completed = result(&agent, "new", "finish current task").await.unwrap();
    assert_eq!(completed.final_message(), "effect reconciled");
    assert_eq!(requests.lock().unwrap().len(), 4);
    result(&agent, "new", "finish current task").await.unwrap();
    assert_eq!(
        requests.lock().unwrap().len(),
        4,
        "terminal receipt causes no new HTTP"
    );
    evidence(
        "durable-prompt-limit",
        &requests.lock().unwrap(),
        0,
        "empty max_prompt_tokens terminal -> one summary -> completion; terminal replay causes zero HTTP",
    );
    agent.shutdown().await.unwrap();
    server.abort();

    let path = directory.path().join("retry.sqlite");
    let (client, requests, server) = server_http(|_, _| (503, "known rejection".into())).await;
    let (agent, _) = Xai::new(client, "grok-4.6")
        .max_retries(2)
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    assert!(
        result(&agent, "bounded", "attempt bounded request")
            .await
            .is_err()
    );
    assert_eq!(requests.lock().unwrap().len(), 3);
    assert!(
        result(&agent, "bounded", "attempt bounded request")
            .await
            .is_err()
    );
    assert_eq!(requests.lock().unwrap().len(), 3);
    evidence(
        "durable-bounded-rejections",
        &requests.lock().unwrap(),
        0,
        "two retries yields three physical requests; exact failed-request replay causes zero HTTP",
    );
    agent.shutdown().await.unwrap();
    server.abort();
    println!(
        "SQLite+HTTP journey: max_prompt_tokens compacts once; transient HTTP retries remain bounded under durable ownership"
    );
}

#[tokio::test]
async fn durable_fork_identity_is_distinct_from_caller_request_id() {
    let directory = tempfile::tempdir().unwrap();
    let (client, requests, server) = server(|_, _| answer()).await;
    let first_path = directory.path().join("a.sqlite");
    let second_path = directory.path().join("b.sqlite");
    let (a, _) = Xai::new(client.clone(), "grok-4.6")
        .durability(reopen(&first_path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let (b, _) = Xai::new(client.clone(), "grok-4.6")
        .durability(reopen(&second_path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let a_result = result(&a, "same-caller-id", "conversation violet")
        .await
        .unwrap();
    let b_result = result(&b, "same-caller-id", "conversation amber")
        .await
        .unwrap();
    assert_eq!(a_result.request_id(), b_result.request_id());
    assert!(
        a.fork_from(&b_result).await.is_err(),
        "foreign result must not match caller ID"
    );
    let (fork, _) = a.fork_from(&a_result).await.unwrap();
    assert!(
        serde_json::to_value(fork.context().await.unwrap().history())
            .unwrap()
            .to_string()
            .contains("conversation violet")
    );
    fork.shutdown().await.unwrap();
    a.shutdown().await.unwrap();
    b.shutdown().await.unwrap();
    let (a, _) = Xai::new(client, "grok-4.6")
        .durability(reopen(&first_path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    result(&a, "later", "new current context").await.unwrap();
    let replay = result(&a, "same-caller-id", "conversation violet")
        .await
        .unwrap();
    let (fork, _) = a.fork_from(&replay).await.unwrap();
    let historical = serde_json::to_value(fork.context().await.unwrap().history())
        .unwrap()
        .to_string();
    assert!(historical.contains("conversation violet"));
    assert!(!historical.contains("new current context"));
    assert!(
        serde_json::to_value(a.context().await.unwrap().history())
            .unwrap()
            .to_string()
            .contains("new current context")
    );
    assert_eq!(requests.lock().unwrap().len(), 3);
    evidence(
        "durable-fork-provenance",
        &requests.lock().unwrap(),
        0,
        "foreign same-ID result rejected; replayed historical checkpoint forks without rewinding newer live context",
    );
    fork.shutdown().await.unwrap();
    a.shutdown().await.unwrap();
    server.abort();
    println!(
        "SQLite+HTTP journey: identical caller IDs from distinct stores cannot cross-fork; replayed historical checkpoint remains available"
    );
}

#[tokio::test]
async fn recovered_tool_repetition_budget_covers_prior_rounds_and_replayed_receipts() {
    for partial_round in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("repeat.sqlite");
        let armed = Arc::new(AtomicBool::new(false));
        let arm = armed.clone();
        let (client,requests,server)=server(move |index,_| {
            let call = |id:&str|json!({"type":"function_call","call_id":id,"name":"effect","arguments":"{\"key\":\"synthetic\"}"});
            if index==1 {
                let mut calls=vec![call("first")]; if partial_round {calls.push(call("second"));}
                return completion(calls);
            }
            if index==2 && !partial_round {arm.store(true,Ordering::SeqCst);return completion(vec![call("second")]);}
            answer()
        }).await;
        let state = DurableSession::open(
            InterruptedStore {
                inner: SqliteStore::open(&path).unwrap(),
                armed: armed.clone(),
                after_commit: true,
                fail_at: None,
                writes: Arc::new(AtomicUsize::new(0)),
            },
            "xai-synthetic",
        )
        .await
        .unwrap();
        let effects = Arc::new(AtomicUsize::new(0));
        let (agent, _) = recipe(
            client.clone(),
            effects.clone(),
            partial_round.then_some(armed),
        )
        .repetition_limit(1)
        .durability(state)
        .await
        .unwrap()
        .build()
        .unwrap();
        assert!(
            result(&agent, "repeat", "perform one effect only")
                .await
                .unwrap_err()
                .execution_policy_disposition()
                .is_some()
        );
        let _ = agent.shutdown().await;
        drop(agent);
        let (agent, _) = recipe(client, effects.clone(), None)
            .repetition_limit(99)
            .durability(reopen(&path).await)
            .await
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            result(&agent, "repeat", "perform one effect only")
                .await
                .unwrap()
                .final_message(),
            "effect reconciled"
        );
        assert_eq!(
            effects.load(Ordering::SeqCst),
            1,
            "reopened budget must retain original limit and receipts"
        );
        let log = requests.lock().unwrap().clone();
        assert!(
            log.last().unwrap()["input"]
                .to_string()
                .contains("repetition limit")
        );
        evidence(
            if partial_round {
                "repetition-partial-round"
            } else {
                "repetition-prior-round"
            },
            &log,
            1,
            "reopened request with changed recipe retains admitted repetition limit and counts; executes one observed effect",
        );
        agent.shutdown().await.unwrap();
        server.abort();
        println!(
            "SQLite+HTTP journey: repetition_limit=1 survived partial_round={partial_round} with exactly one host effect"
        );
    }
}

#[tokio::test]
async fn streamed_hosted_effect_prevents_context_recovery_even_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("streamed-effect.sqlite");
    let (client, requests, server) = server(|index, _| {
        if index == 2 {
            let effect = json!({"type":"response.output_item.done","output_index":0,
                "item":{"type":"web_search_call","id":"hosted-effect","status":"completed"}});
            let terminal = json!({"type":"response.incomplete","response":{"status":"incomplete",
                "output":[],"incomplete_details":{"reason":"max_prompt_tokens"}}});
            return format!("data: {effect}\n\ndata: {terminal}\n\n");
        }
        answer()
    })
    .await;
    let (agent, _) = Xai::new(client.clone(), "grok-4.6")
        .web_search()
        .compaction_keep_tail(0)
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    result(&agent, "history", &"retained violet history ".repeat(200))
        .await
        .unwrap();
    let failure = result(&agent, "interrupted", "continue the task")
        .await
        .unwrap_err();
    assert!(failure.to_string().contains("incomplete"), "{failure}");
    assert_eq!(
        requests.lock().unwrap().len(),
        2,
        "observed hosted effect forbids recovery sampling"
    );
    agent.shutdown().await.unwrap();
    drop(agent);
    let (agent, _) = Xai::new(client, "grok-4.6")
        .durability(reopen(&path).await)
        .await
        .unwrap()
        .build()
        .unwrap();
    let replayed = result(&agent, "interrupted", "continue the task")
        .await
        .unwrap_err();
    assert!(replayed.to_string().contains("incomplete"));
    assert_eq!(
        requests.lock().unwrap().len(),
        2,
        "reopen replays the failure without provider calls"
    );
    result(&agent, "followup", "explicitly continue")
        .await
        .unwrap();
    let log = requests.lock().unwrap().clone();
    assert_eq!(log.len(), 3);
    assert!(
        log[2]["input"]
            .to_string()
            .contains("retained violet history")
    );
    evidence(
        "streamed-effect-no-retry",
        &log,
        0,
        "streamed hosted effect followed by empty prompt-limit terminal fails without compaction; SQLite reopen replays failure; only explicit followup sends another request",
    );
    agent.shutdown().await.unwrap();
    server.abort();
    println!(
        "SQLite+SSE journey: observed hosted effect cannot trigger context recovery; failed receipt replay sends no HTTP; explicit followup succeeds (3 requests)"
    );
}
