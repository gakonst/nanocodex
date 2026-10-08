//! Output exhaustion through real streaming HTTP, including client effect fences.
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
        json!({"type":"message_start","message":{"id":"response","role":"assistant","model":"test","content":[],"usage":{"input_tokens":157334,"output_tokens":0}}}),
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
    fixture_gated(responses, None).await
}
/// Optionally holds the zero-based request `.0` until `.2` is notified, after
/// signalling `.1`, so a test can act while that provider call is in flight.
type Gate = (usize, Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>);
async fn fixture_gated(
    responses: Vec<String>,
    gate: Option<Gate>,
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
            let gate = gate.clone();
            async move {
                let index = {
                    let mut log = log.lock().unwrap();
                    log.push(body.clone());
                    log.len() - 1
                };
                if let Some((held, started, release)) = gate
                    && held == index
                {
                    started.notify_one();
                    release.notified().await;
                }
                if std::env::var_os("NANOCLAUDE_OUTPUT_TRACE").is_some() {
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
fn cut_tool(closed: bool, stop: &str) -> String {
    let mut s = response(
        vec![
            json!({"type":"thinking","thinking":"signed partial reasoning","signature":"opaque-signature"}),
            json!({"type":"text","text":"partial answer"}),
        ],
        stop,
    );
    let tail = s.find("data: {\"delta\"").unwrap();
    let mut tool = format!(
        "data: {}\n\ndata: {}\n\n",
        json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"incomplete","name":"effect","input":{}}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"value\":"}})
    );
    if closed {
        tool += &format!(
            "data: {}\n\n",
            json!({"type":"content_block_stop","index":2})
        );
    }
    s.insert_str(tail, &tool);
    s
}

#[tokio::test]
async fn partial_tool_never_executes_and_signed_content_continues() {
    for (closed, valid_input) in [(false, false), (true, false), (false, true)] {
        let complete =
            json!({"type":"tool_use","id":"complete","name":"effect","input":{"value":1}});
        let (client, log, server) = fixture(vec![
            if valid_input {
                cut_tool(closed, "max_tokens").replace(
                    &json!({"type":"input_json_delta","partial_json":"{\"value\":"}).to_string(),
                    &json!({"type":"input_json_delta","partial_json":"{\"value\":1}"}).to_string(),
                )
            } else {
                cut_tool(closed, "max_tokens")
            },
            response(vec![complete.clone()], "max_tokens"),
            response(vec![json!({"type":"text","text":"done"})], "end_turn"),
        ])
        .await;
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
            .max_tokens(128_000)
            .tool(
                ToolDefinition {
                    name: "effect".into(),
                    description: "Synthetic effect".into(),
                    input_schema: json!({"type":"object"}),
                    strict: None,
                    defer_loading: false,
                },
                move |input| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async move {
                        assert_eq!(input, json!({"value":1}));
                        Ok("receipt committed".to_string())
                    }
                },
            )
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
            "done"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let r = log.lock().unwrap();
        assert_eq!(r.len(), 3);
        let history = r[1]["messages"].to_string();
        assert!(history.contains("opaque-signature"));
        assert!(history.contains("partial answer"));
        assert!(history.contains("was not executed"));
        assert!(!history.contains("\"id\":\"incomplete\""));
        let history = r[2]["messages"].to_string();
        assert!(history.contains("receipt committed"));
        assert!(
            r[2]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "assistant" && m["content"] == json!([complete]))
        );
        server.abort();
    }
}
#[tokio::test]
async fn repeated_exhaustion_is_bounded_and_last_partial_is_retained() {
    let responses = (0..4)
        .map(|i| {
            response(
                vec![json!({"type":"text","text":format!("partial-{i}")})],
                "max_tokens",
            )
        })
        .chain([response(
            vec![json!({"type":"text","text":"resumed"})],
            "end_turn",
        )])
        .collect();
    let (client, log, server) = fixture(responses).await;
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
        .unwrap_err()
        .to_string();
    assert!(error.contains("after 3 continuations"), "{error}");
    assert_eq!(log.lock().unwrap().len(), 4);
    agent
        .prompt("resume retained task")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let r = log.lock().unwrap();
    assert_eq!(r.len(), 5);
    for i in 0..4 {
        assert!(
            r[4]["messages"]
                .to_string()
                .contains(&format!("partial-{i}"))
        );
    }
    server.abort();
}
#[tokio::test]
async fn malformed_terminal_without_token_cutoff_is_still_rejected() {
    let (client, log, server) = fixture(vec![cut_tool(false, "end_turn")]).await;
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
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("message_stop before content_block_stop"),
        "{error}"
    );
    assert_eq!(log.lock().unwrap().len(), 1);
    server.abort();
}

#[tokio::test]
async fn automatic_compaction_omits_old_thinking_but_retains_cutoff_and_continuation() {
    let (client, log, server) = fixture(vec![
        cut_tool(false, "max_tokens"),
        response(
            vec![json!({"type":"text","text":"Retain the original task."})],
            "end_turn",
        ),
        response(
            vec![json!({"type":"text","text":"done after compaction"})],
            "end_turn",
        ),
    ])
    .await;
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .auto_compact_window_tokens(100_000)
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
        "done after compaction"
    );
    let requests = log.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1]["tool_choice"]["type"], "none");
    let continuation = requests[2]["messages"].as_array().unwrap();
    // The summary replaces the prefix to which the old thinking was bound.
    assert!(
        !requests[2]["messages"]
            .to_string()
            .contains("opaque-signature")
    );
    assert!(
        !requests[2]["messages"]
            .to_string()
            .contains("signed partial reasoning")
    );
    assert!(
        continuation.iter().any(
            |m| m["role"] == "assistant" && m["content"].to_string().contains("partial answer")
        )
    );
    assert!(
        requests[2]["messages"]
            .to_string()
            .contains("was not executed")
    );
    assert!(
        continuation
            .iter()
            .flat_map(|m| m["content"].as_array().unwrap())
            .all(|block| block["type"] != "thinking"
                && block["type"] != "redacted_thinking"
                && block["type"] != "tool_use")
    );
    assert_eq!(continuation.last().unwrap()["role"], "user");
    assert!(
        continuation.last().unwrap()["content"]
            .to_string()
            .contains("Continue the current task")
    );
    assert!(
        requests[2]["messages"]
            .to_string()
            .contains("Retain the original task.")
    );
    server.abort();
}

// ---- Consecutive output-cutoff budget -------------------------------------
fn cutoff(i: usize) -> String {
    response(
        vec![
            json!({"type":"thinking","thinking":format!("reasoning-{i}"),"signature":format!("sig-{i}")}),
            json!({"type":"text","text":format!("partial-{i}")}),
        ],
        "max_tokens",
    )
}
fn tool_round(id: &str) -> String {
    response(
        vec![json!({"type":"tool_use","id":id,"name":"effect","input":{"value":1}})],
        "tool_use",
    )
}
fn finished(text: &str) -> String {
    response(vec![json!({"type":"text","text":text})], "end_turn")
}
fn effect_tool() -> ToolDefinition {
    ToolDefinition {
        name: "effect".into(),
        description: "Synthetic effect".into(),
        input_schema: json!({"type":"object"}),
        strict: None,
        defer_loading: false,
    }
}
const CONTINUE_NOTICE: &str = "The output token limit was reached";

/// Exact shape of the observed failure: a cutoff, eight tool rounds, a cutoff,
/// a tool round, then two more cutoffs. Four cutoffs in the turn, but never
/// more than two in a row, so the turn must finish.
#[tokio::test]
async fn interleaved_cutoffs_and_tool_rounds_succeed_beyond_three_total() {
    let mut responses = vec![cutoff(24)];
    responses.extend((25..=32).map(|n| tool_round(&format!("call-{n}"))));
    responses.extend([
        cutoff(33),
        tool_round("call-34"),
        cutoff(35),
        cutoff(36),
        finished("done"),
    ]);
    let (client, log, server) = fixture(responses).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .tool(effect_tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("receipt committed".to_string()) }
        })
        .build()
        .unwrap();
    let result = agent
        .prompt("finish task")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "done");
    assert_eq!(calls.load(Ordering::SeqCst), 9, "each tool round ran once");
    let r = log.lock().unwrap();
    assert_eq!(r.len(), 14);
    let last = r[13]["messages"].to_string();
    for i in [24, 33, 35, 36] {
        // Partial content and its signature are retained verbatim.
        assert!(last.contains(&format!("partial-{i}")), "{i}");
        assert!(last.contains(&format!("sig-{i}")), "{i}");
    }
    assert_eq!(last.matches(CONTINUE_NOTICE).count(), 4);
    server.abort();
}

/// A successful tool round resets the budget, but four cutoffs in a row after
/// it still fail, keep the last partial, and never rerun the committed effect.
#[tokio::test]
async fn four_consecutive_cutoffs_after_a_tool_round_still_fail() {
    let (client, log, server) = fixture(vec![
        cutoff(0),
        cutoff(1),
        tool_round("call-a"),
        cutoff(2),
        cutoff(3),
        cutoff(4),
        cutoff(5),
        finished("resumed"),
    ])
    .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .tool(effect_tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("receipt committed".to_string()) }
        })
        .build()
        .unwrap();
    let error = agent
        .prompt("finish task")
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("after 3 continuations"), "{error}");
    assert_eq!(log.lock().unwrap().len(), 7);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    agent
        .prompt("resume retained task")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "committed receipt not rerun"
    );
    let r = log.lock().unwrap();
    assert_eq!(r.len(), 8);
    let history = r[7]["messages"].to_string();
    assert!(history.contains("receipt committed"));
    for i in 0..6 {
        assert!(history.contains(&format!("partial-{i}")), "{i}");
        assert!(history.contains(&format!("sig-{i}")), "{i}");
    }
    server.abort();
}

/// A cutoff that still carries a complete tool call executes it, but remains a
/// cutoff: it never resets the budget.
#[tokio::test]
async fn cutoff_with_complete_tool_call_does_not_reset_budget() {
    let (client, log, server) = fixture(vec![
        cutoff(0),
        cutoff(1),
        response(
            vec![json!({"type":"tool_use","id":"cut-call","name":"effect","input":{"value":1}})],
            "max_tokens",
        ),
        cutoff(3),
        finished("unreachable"),
    ])
    .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .tool(effect_tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("receipt committed".to_string()) }
        })
        .build()
        .unwrap();
    let error = agent
        .prompt("finish task")
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("after 3 continuations"), "{error}");
    assert_eq!(log.lock().unwrap().len(), 4);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
}

struct StopOnce(std::sync::atomic::AtomicBool);
impl nanocodex_claude::ClaudeToolHooks for StopOnce {
    fn handles_lifecycle(&self, event: &nanocodex_claude::ClaudeLifecycleEvent) -> bool {
        matches!(event, nanocodex_claude::ClaudeLifecycleEvent::Stop { .. })
    }
    fn before<'a>(
        &'a self,
        _: &'a str,
        _: &'a Value,
        _: &'a nanocodex_claude::ClaudeToolInvocation,
    ) -> nanocodex_claude::ClaudeHookFuture<'a, Result<nanocodex_claude::ClaudeToolDecision, String>>
    {
        Box::pin(async { Ok(nanocodex_claude::ClaudeToolDecision::Allow) })
    }
    fn lifecycle<'a>(
        &'a self,
        _: &'a nanocodex_claude::ClaudeLifecycleInvocation,
    ) -> nanocodex_claude::ClaudeHookFuture<
        'a,
        Result<nanocodex_claude::ClaudeLifecycleOutcome, String>,
    > {
        Box::pin(async move {
            let block = !self.0.swap(true, Ordering::SeqCst);
            Ok(nanocodex_claude::ClaudeLifecycleOutcome {
                decision: if block {
                    nanocodex_claude::ClaudeLifecycleDecision::Block("keep going".into())
                } else {
                    nanocodex_claude::ClaudeLifecycleDecision::Continue
                },
                ..Default::default()
            })
        })
    }
}

/// A normally finished response that a Stop hook continues is progress too.
#[tokio::test]
async fn stop_hook_continuation_after_normal_finish_resets_budget() {
    let (client, log, server) = fixture(vec![
        cutoff(0),
        cutoff(1),
        finished("first finish"),
        cutoff(2),
        cutoff(3),
        cutoff(4),
        finished("done"),
    ])
    .await;
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .tool_hooks(Arc::new(StopOnce(Default::default())))
        .build()
        .unwrap();
    let result = agent
        .prompt("finish task")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "done");
    assert_eq!(log.lock().unwrap().len(), 7);
    assert!(
        log.lock().unwrap()[3]["messages"]
            .to_string()
            .contains("Host Stop hook requests continuation: keep going")
    );
    server.abort();
}

/// Accepted steering after a normal end_turn starts a fresh response budget.
#[tokio::test]
async fn steering_after_normal_finish_resets_budget() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (client, log, server) = fixture_gated(
        vec![
            cutoff(0),
            cutoff(1),
            finished("first finish"),
            cutoff(2),
            cutoff(3),
            cutoff(4),
            finished("done"),
        ],
        Some((2, started.clone(), release.clone())),
    )
    .await;
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .build()
        .unwrap();
    let turn = agent.prompt("finish task").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    turn.steer("also mention steering").await.unwrap();
    release.notify_one();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), turn.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.final_message(), "done");
    let r = log.lock().unwrap();
    assert_eq!(r.len(), 7);
    assert!(
        r[3]["messages"]
            .to_string()
            .contains("also mention steering")
    );
    server.abort();
}

// ---- Durable reopen -------------------------------------------------------
use nanocodex_agent::{ExecutionPolicyDisposition, NanocodexError};
use nanocodex_claude::execution::{Admission, ClaudeExecutionPolicy, PolicyFuture, Step};
use std::collections::HashMap;

/// In-memory host store: persisted cursors and settled step receipts survive
/// the "crash" (an advance failing with a reopen disposition).
type CrashPredicate = Box<dyn Fn(&Value) -> bool + Send>;

struct Store {
    cursors: Mutex<Vec<Value>>,
    steps: Mutex<HashMap<String, Value>>,
    attempts: AtomicUsize,
    crash: Mutex<Option<CrashPredicate>>,
    terminal: Mutex<Vec<&'static str>>,
}
impl Store {
    fn new(crash: impl Fn(&Value) -> bool + Send + 'static) -> Arc<Self> {
        Arc::new(Self {
            cursors: Mutex::default(),
            steps: Mutex::default(),
            attempts: AtomicUsize::new(0),
            crash: Mutex::new(Some(Box::new(crash))),
            terminal: Mutex::default(),
        })
    }
    fn counts(&self) -> Vec<(u64, u64)> {
        self.cursors
            .lock()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["index"].as_u64().unwrap(),
                    c["output_continuations"].as_u64().unwrap_or(0),
                )
            })
            .collect()
    }
}
impl ClaudeExecutionPolicy for Store {
    fn state_id(&self) -> &str {
        "output-budget-store"
    }
    fn admit(&self, _: String, _: Value, _: bool) -> PolicyFuture<'_, (String, Admission)> {
        let resume = self.attempts.load(Ordering::SeqCst) > 0;
        Box::pin(async move {
            let admission = if resume {
                Admission::Resume
            } else {
                Admission::Execute
            };
            Ok(("budget-op".to_string(), admission))
        })
    }
    fn begin_attempt(&self, _: String) -> PolicyFuture<'_, ()> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn continuation(&self, _: String) -> PolicyFuture<'_, Option<Value>> {
        let last = self.cursors.lock().unwrap().last().cloned();
        Box::pin(async move { Ok(last) })
    }
    fn advance(&self, _: String, state: Value) -> PolicyFuture<'_, ()> {
        let mut crash = self.crash.lock().unwrap();
        let hit = crash.as_ref().is_some_and(|when| when(&state));
        if hit {
            *crash = None;
            return Box::pin(async {
                Err(NanocodexError::execution_policy_with_disposition(
                    "test-store",
                    ExecutionPolicyDisposition::Reopen,
                    std::io::Error::other("simulated crash before cursor persisted"),
                ))
            });
        }
        self.cursors.lock().unwrap().push(state);
        Box::pin(async { Ok(()) })
    }
    fn begin_step(&self, _: String, step: String, _: String, _: Value) -> PolicyFuture<'_, Step> {
        let settled = self.steps.lock().unwrap().get(&step).cloned();
        Box::pin(async move { Ok(settled.map_or(Step::Execute, Step::Replay)) })
    }
    fn complete_step(&self, _: String, step: String, output: Value) -> PolicyFuture<'_, ()> {
        self.steps.lock().unwrap().insert(step, output);
        Box::pin(async { Ok(()) })
    }
    fn complete(&self, _: String, _: Value, _: Value) -> PolicyFuture<'_, ()> {
        self.terminal.lock().unwrap().push("complete");
        Box::pin(async { Ok(()) })
    }
    fn fail(&self, _: String, _: Value, _: String) -> PolicyFuture<'_, ()> {
        self.terminal.lock().unwrap().push("fail");
        Box::pin(async { Ok(()) })
    }
    fn cancel(&self, _: String, _: Value) -> PolicyFuture<'_, ()> {
        self.terminal.lock().unwrap().push("cancel");
        Box::pin(async { Ok(()) })
    }
    fn release(&self, _: String) -> PolicyFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn shutdown(&self) -> PolicyFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn checkpoint(&self, _: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
fn durable_agent(client: ClaudeClient, store: &Arc<Store>, calls: &Arc<AtomicUsize>) -> Nanocodex {
    let counter = calls.clone();
    Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .tool(effect_tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("receipt committed".to_string()) }
        })
        .execution_policy(store.clone(), None)
        .unwrap()
        .build()
        .unwrap()
        .0
}

/// Crash after a tool round settled but before its reset cursor persisted:
/// reopen replays the model response and tool receipt (effect runs once),
/// re-derives the reset, and the next three cutoffs are within budget.
#[tokio::test]
async fn reopen_after_tool_round_replays_receipt_once_and_resets_budget() {
    let (client, log, server) = fixture(vec![
        cutoff(0),
        cutoff(1),
        tool_round("call-a"),
        cutoff(2),
        cutoff(3),
        cutoff(4),
        finished("done"),
    ])
    .await;
    // The tool round's cursor is the first with index 3 and a reset budget.
    let store = Store::new(|c| c["index"] == 3 && c["output_continuations"] == 0);
    let calls = Arc::new(AtomicUsize::new(0));
    let first = durable_agent(client.clone(), &store, &calls);
    let error = first
        .prompt("finish task")
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err();
    assert!(error.execution_policy_disposition().is_some(), "{error}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(log.lock().unwrap().len(), 3);
    assert_eq!(store.counts().last(), Some(&(2, 2)), "{:?}", store.counts());
    assert!(store.terminal.lock().unwrap().is_empty());
    drop(first);
    let reopened = durable_agent(client, &store, &calls);
    let result = reopened
        .prompt("finish task")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "done");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "settled receipt replayed");
    assert_eq!(log.lock().unwrap().len(), 7, "replay made no provider call");
    assert!(store.counts().contains(&(3, 0)), "{:?}", store.counts());
    assert_eq!(*store.terminal.lock().unwrap(), ["complete"]);
    server.abort();
}

/// Crash after the third cutoff but before its cursor persisted: the reopened
/// turn re-counts that cutoff from the persisted cursor and cannot refill the
/// budget, so the next cutoff fails after exactly four provider calls.
#[tokio::test]
async fn reopen_cannot_refill_consecutive_cutoff_budget() {
    let (client, log, server) = fixture(vec![
        cutoff(0),
        cutoff(1),
        cutoff(2),
        cutoff(3),
        finished("unreachable"),
    ])
    .await;
    let store = Store::new(|c| c["index"] == 3 && c["output_continuations"] == 3);
    let calls = Arc::new(AtomicUsize::new(0));
    let first = durable_agent(client.clone(), &store, &calls);
    let error = first
        .prompt("finish task")
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err();
    assert!(error.execution_policy_disposition().is_some(), "{error}");
    assert_eq!(log.lock().unwrap().len(), 3);
    assert_eq!(store.counts().last(), Some(&(2, 2)), "{:?}", store.counts());
    drop(first);
    let reopened = durable_agent(client, &store, &calls);
    let error = reopened
        .prompt("finish task")
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("after 3 continuations"), "{error}");
    let r = log.lock().unwrap();
    assert_eq!(r.len(), 4, "third cutoff replayed, fourth called once");
    let history = r[3]["messages"].to_string();
    for i in 0..3 {
        assert!(history.contains(&format!("sig-{i}")), "{i}");
        assert!(history.contains(&format!("partial-{i}")), "{i}");
    }
    assert_eq!(*store.terminal.lock().unwrap(), ["fail"]);
    server.abort();
}

/// Provider refusal is an unsuccessful terminal turn, never a continuation.
#[tokio::test]
async fn refusal_is_terminal_without_retry_or_client_effects() {
    for (case, blocks) in [
        ("empty", vec![]),
        (
            "text",
            vec![json!({"type":"text","text":"I cannot help with that request."})],
        ),
        (
            "tool",
            vec![
                json!({"type":"text","text":"I cannot help with that request."}),
                json!({"type":"tool_use","id":"refused-tool","name":"effect","input":{"value":1}}),
            ],
        ),
    ] {
        let (client, log, server) = fixture(vec![response(blocks, "refusal")]).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
            .max_tokens(128_000)
            .tool(
                ToolDefinition {
                    name: "effect".into(),
                    description: "Synthetic effect".into(),
                    input_schema: json!({"type":"object"}),
                    strict: None,
                    defer_loading: false,
                },
                move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async { Ok("must not execute".to_string()) }
                },
            )
            .build()
            .unwrap();
        let error = agent
            .prompt("synthetic refusal test")
            .await
            .unwrap()
            .result()
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("provider refused the request"),
            "{case}: {error}"
        );
        assert!(error.contains("stop_reason=refusal"), "{case}: {error}");
        assert!(!error.contains("unsupported"), "{case}: {error}");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "{case}");
        assert_eq!(log.lock().unwrap().len(), 1, "{case}");
        eprintln!("refusal case={case}: error={error}; provider_requests=1; client_effects=0");
        server.abort();
    }
}
