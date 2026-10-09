//! Transient-failure retry journeys through the public agent and a loopback
//! Messages endpoint.
use axum::{Router, http::StatusCode, response::IntoResponse, routing::post};
use futures_util::{StreamExt, stream};
use nanocodex_agent::{
    Nanocodex, NanocodexError,
    events::{AgentEventKind, AgentEvents, TimedAgentEvent, monotonic_now_ns},
};
use nanocodex_claude::{Claude, ClaudeClient, ServerToolDefinition, ToolDefinition};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

enum Reply {
    Http(u16),
    RateLimited(&'static str),
    Stream(String),
    Disconnected,
}

struct Fixture {
    client: ClaudeClient,
    requests: Arc<Mutex<Vec<(String, u64)>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new(reply: impl Fn(usize) -> Reply + Send + Sync + 'static) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        let reply = Arc::new(reply);
        let app = Router::new().route(
            "/v1/messages",
            post(move |body: String| {
                let log = log.clone();
                let reply = reply.clone();
                async move {
                    let index = {
                        let mut log = log.lock().unwrap();
                        log.push((body, monotonic_now_ns()));
                        log.len()
                    };
                    let stream = [("content-type", "text/event-stream")];
                    match reply(index) {
                        Reply::Http(status) => {
                            (StatusCode::from_u16(status).unwrap(), "synthetic failure")
                                .into_response()
                        }
                        Reply::RateLimited(delay) => (
                            StatusCode::TOO_MANY_REQUESTS,
                            [("retry-after", delay)],
                            "limited",
                        )
                            .into_response(),
                        Reply::Stream(body) => (stream, body).into_response(),
                        Reply::Disconnected => (
                            stream,
                            axum::body::Body::from_stream(stream::iter([Err::<String, _>(
                                std::io::Error::other("connection interrupted"),
                            )])),
                        )
                            .into_response(),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            client: ClaudeClient::new(reqwest::Client::new(), endpoint, "synthetic"),
            requests,
            server,
        }
    }

    fn bodies(&self) -> Vec<String> {
        let requests = self.requests.lock().unwrap();
        requests.iter().map(|(body, _)| body.clone()).collect()
    }

    /// Checks a retry announcement against the request it scheduled. Emission
    /// and loopback receipt share a monotonic clock, so queued event delivery
    /// cannot inflate the measured wait.
    fn assert_retry_wait(&self, event: &TimedAgentEvent, next_request: usize) -> Value {
        let payload: Value = serde_json::from_str(event.event.payload.get()).unwrap();
        let attempt = payload["attempt"].as_u64().unwrap();
        assert_eq!(payload["next_attempt"], attempt + 1);
        let delay = Duration::from_nanos(payload["delay_ns"].as_u64().unwrap());
        let scale = 2_u32.pow(u32::try_from(attempt - 1).unwrap());
        assert!(delay >= Duration::from_millis(900) * scale, "{payload}");
        if payload["server_requested_delay"] == false {
            assert!(delay <= Duration::from_millis(1_100) * scale, "{payload}");
        }
        let received_ns = self.requests.lock().unwrap()[next_request].1;
        let waited = Duration::from_nanos(received_ns - event.timing.emitted_ns);
        assert!(
            waited >= delay,
            "retried after {waited:?}, before {delay:?}"
        );
        payload
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn frames(events: Vec<Value>) -> String {
    events
        .into_iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

fn start() -> Value {
    json!({"type":"message_start","message":{"id":"synthetic","role":"assistant","model":"test","content":[],"usage":{"input_tokens":10,"output_tokens":0}}})
}

fn stream_error(kind: &str) -> String {
    frames(vec![
        json!({"type":"error","error":{"type":kind,"message":"synthetic failure"}}),
    ])
}

fn text_delta(text: &str) -> Vec<Value> {
    vec![
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
    ]
}

fn completed(tool: bool) -> String {
    let mut events = vec![start()];
    if tool {
        events.push(json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"effect-once","name":"effect","input":{}}}));
    } else {
        events.extend(text_delta("completed"));
    }
    events.push(json!({"type":"content_block_stop","index":0}));
    events.push(json!({"type":"message_delta","delta":{"stop_reason":if tool { "tool_use" } else { "end_turn" }},"usage":{"output_tokens":5}}));
    events.push(json!({"type":"message_stop"}));
    frames(events)
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

/// Collects retry announcements until the run settles.
async fn retries(events: &mut AgentEvents) -> Vec<TimedAgentEvent> {
    let mut retries = Vec::new();
    loop {
        let timed = events.recv_timed().await.unwrap();
        match timed.event.kind {
            AgentEventKind::ModelAttemptRetrying => retries.push(timed),
            AgentEventKind::RunCompleted | AgentEventKind::RunFailed => return retries,
            _ => {}
        }
    }
}

#[tokio::test]
async fn transient_failures_resend_the_current_request_after_backoff() {
    let fixture = Fixture::new(|index| match index {
        1 => Reply::Http(503),
        2 => Reply::Stream(completed(true)),
        3 => Reply::Stream(
            frames(vec![
                start(),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"discarded","name":"effect","input":{}}}),
                json!({"type":"content_block_stop","index":0}),
            ]) + &stream_error("overloaded_error"),
        ),
        4 => Reply::Disconnected,
        _ => Reply::Stream(completed(false)),
    })
    .await;
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let (agent, mut events) = Nanocodex::builder(Claude::new(fixture.client.clone(), "test"))
        .max_tokens(128_000)
        .tool(effect_tool(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("committed once".into()) }
        })
        .build()
        .unwrap();
    let result = agent
        .prompt("complete the effect")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "completed");
    assert_eq!(result.usage().unwrap().total_tokens(), 30);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let log = fixture.bodies();
    assert_eq!(log.len(), 5);
    assert_eq!(log[0], log[1]);
    assert!(log[2..].iter().all(|body| *body == log[2]));
    assert!(log[2].contains("committed once") && !log[2].contains("discarded"));

    let mut completed_attempts = Vec::new();
    let mut retries = Vec::new();
    loop {
        let timed = events.recv_timed().await.unwrap();
        let payload: Value = serde_json::from_str(timed.event.payload.get()).unwrap();
        match timed.event.kind {
            AgentEventKind::ModelCallCompleted => {
                completed_attempts.push(payload["attempt"].clone())
            }
            AgentEventKind::ModelAttemptRetrying => {
                let next_request = [1, 3, 4][retries.len()];
                let payload = fixture.assert_retry_wait(&timed, next_request);
                retries.push((
                    payload["model_call_index"].clone(),
                    payload["attempt"].clone(),
                ));
                assert_eq!(payload["max_attempts"], 5);
            }
            AgentEventKind::RunCompleted => break,
            _ => {}
        }
    }
    assert_eq!(
        retries,
        [
            (json!(0), json!(1)),
            (json!(1), json!(1)),
            (json!(1), json!(2))
        ]
    );
    assert_eq!(completed_attempts, [json!(2), json!(3)]);
    agent.shutdown().await.unwrap();
}

#[tokio::test]
async fn persistent_transient_failure_ends_after_five_attempts() {
    let fixture = Fixture::new(|_| Reply::Stream(stream_error("overloaded_error"))).await;
    let (agent, mut events) = Nanocodex::builder(Claude::new(fixture.client.clone(), "test"))
        .max_tokens(128_000)
        .build()
        .unwrap();
    let error = agent
        .prompt("exhaust retries")
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("overloaded_error"), "{error}");
    assert_eq!(fixture.bodies().len(), 5);
    let retries = retries(&mut events).await;
    assert_eq!(retries.len(), 4);
    for (attempt, retry) in retries.iter().enumerate() {
        assert_eq!(
            fixture.assert_retry_wait(retry, attempt + 1)["attempt"],
            attempt + 1
        );
    }
    agent.shutdown().await.unwrap();
}

#[tokio::test]
async fn compaction_ends_after_three_attempts() {
    let fixture = Fixture::new(|index| {
        Reply::Stream(if index == 1 {
            completed(false)
        } else {
            stream_error("overloaded_error")
        })
    })
    .await;
    let (agent, _events) = Nanocodex::builder(Claude::new(fixture.client.clone(), "test"))
        .max_tokens(128_000)
        .build()
        .unwrap();
    agent
        .prompt("remember this")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert!(agent.compact().await.is_err());
    let log = fixture.bodies();
    assert_eq!(log.len(), 4);
    assert!(log[1..].iter().all(|body| *body == log[1]));
    agent.shutdown().await.unwrap();
}

#[tokio::test]
async fn unsafe_or_permanent_failures_are_not_retried() {
    let published = frames([vec![start()], text_delta("published once")].concat())
        + &stream_error("overloaded_error");
    let progress = frames(vec![
        start(),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Checking the retrieved record."}}),
    ]) + &stream_error("overloaded_error");
    let scenarios = [
        ("permanent HTTP rejection", Reply::Http(400), false),
        (
            "permanent stream error",
            Reply::Stream(stream_error("invalid_request_error")),
            false,
        ),
        ("published text", Reply::Stream(published), false),
        ("published progress", Reply::Stream(progress), false),
        (
            "possible server effect",
            Reply::Stream(stream_error("overloaded_error")),
            true,
        ),
        ("long Retry-After", Reply::RateLimited("61"), false),
        (
            "distant Retry-After date",
            Reply::RateLimited("Fri, 01 Jan 2100 00:00:00 GMT"),
            false,
        ),
    ];
    for (scenario, reply, server_tool) in scenarios {
        let reply = Mutex::new(Some(reply));
        let fixture = Fixture::new(move |_| reply.lock().unwrap().take().unwrap()).await;
        let mut builder =
            Nanocodex::builder(Claude::new(fixture.client.clone(), "test")).max_tokens(128_000);
        if server_tool {
            builder = builder.server_tool(ServerToolDefinition::code_execution_current());
        }
        let (agent, mut events) = builder.build().unwrap();
        let result = agent.prompt("fail safely").await.unwrap().result().await;
        assert!(result.is_err(), "{scenario}");
        assert_eq!(fixture.bodies().len(), 1, "{scenario}");
        assert!(retries(&mut events).await.is_empty(), "{scenario}");
        agent.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn retry_after_sets_a_cancellable_minimum_delay() {
    // An explicit rejection proves the request had no server-tool effects.
    let fixture = Fixture::new(|index| match index {
        1 => Reply::RateLimited("2"),
        _ => Reply::Stream(completed(false)),
    })
    .await;
    let (agent, mut events) = Nanocodex::builder(Claude::new(fixture.client.clone(), "test"))
        .max_tokens(128_000)
        .server_tool(ServerToolDefinition::code_execution_current())
        .build()
        .unwrap();
    let result = agent.prompt("respect delay").await.unwrap().result().await;
    assert_eq!(result.unwrap().final_message(), "completed");
    let log = fixture.bodies();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0], log[1]);
    let retries = retries(&mut events).await;
    assert_eq!(retries.len(), 1);
    let payload = fixture.assert_retry_wait(&retries[0], 1);
    assert_eq!(payload["server_requested_delay"], true);
    assert!(payload["delay_ns"].as_u64().unwrap() >= 2_000_000_000);
    agent.shutdown().await.unwrap();

    let fixture = Fixture::new(|_| Reply::RateLimited("60")).await;
    let (agent, mut events) = Nanocodex::builder(Claude::new(fixture.client.clone(), "test"))
        .max_tokens(128_000)
        .build()
        .unwrap();
    let turn = agent.prompt("cancel during backoff").await.unwrap();
    let retry = loop {
        let event = events.next().await.unwrap();
        if event.kind == AgentEventKind::ModelAttemptRetrying {
            break event;
        }
    };
    let payload: Value = serde_json::from_str(retry.payload.get()).unwrap();
    assert_eq!(payload["delay_ns"], 60_000_000_000_u64);
    turn.cancel().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), turn.result())
        .await
        .unwrap();
    assert!(matches!(result, Err(NanocodexError::TurnCancelled)));
    assert_eq!(fixture.bodies().len(), 1);
    agent.shutdown().await.unwrap();
}
