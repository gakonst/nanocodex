use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, Response, StatusCode},
    response::IntoResponse,
    routing::{get, patch, post},
};
use futures_util::{FutureExt, stream};
use nanocodex_agent::{
    AgentEvents, Model, Nanocodex, NanocodexError, PromptRequest, ReasoningMode, Thinking,
    TurnControl, TurnResult,
};
use nanocodex_managed::{
    AgentSettings, Managed, ManagedApiKey, ManagedClient, ManagedError, ManagedEventData,
    PromptInput,
};
use nanocodex_oai_api::events::AgentEventKind;
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

#[cfg(feature = "tools")]
use nanocodex_tools::{
    Tools,
    attachment::{AttachmentMachine, AttachmentMetadata},
};

const AGENT_ID: &str = "agent-public-lifecycle";
const SESSION_ID: &str = "019fc927-b280-79a7-8445-1b9996ad2fb0";
const ACTIVE_REQUEST_ID: &str = "caller-request-active";
const RETAINED_REQUEST_ID: &str = "caller-request-retained";
const CANCELLED_REQUEST_ID: &str = "caller-request-cancelled";
const FOREIGN_REQUEST_ID: &str = "caller-request-foreign";
const ROOT_SOURCE_REQUEST_ID: &str = "server-private-session";
const CHILD_SOURCE_REQUEST_ID: &str = "server-private-subagent";
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct Fixture {
    inner: Arc<FixtureInner>,
}

struct FixtureInner {
    authorization: String,
    state_reads: Mutex<Vec<String>>,
    event_cursors: Mutex<Vec<String>>,
    event_streams: Mutex<Vec<mpsc::UnboundedSender<Bytes>>>,
    retained_events: Mutex<Vec<Bytes>>,
    submissions: Mutex<Vec<Submission>>,
    actions: Mutex<Vec<Action>>,
    create_bodies: Mutex<Vec<Value>>,
    settings: Mutex<Value>,
    operations: Mutex<Vec<&'static str>>,
    catalogs: Mutex<Vec<Value>>,
    changed: Notify,
    steer_entered: Notify,
    steer_release: Mutex<Option<Arc<Notify>>>,
}

#[derive(Debug)]
struct Submission {
    idempotency_key: String,
    body: Value,
}

#[derive(Debug)]
struct Action {
    kind: &'static str,
    agent_id: String,
    turn_id: String,
    body: Option<Value>,
}

impl Fixture {
    fn new(api_key: &str) -> Self {
        Self {
            inner: Arc::new(FixtureInner {
                authorization: format!("Bearer {api_key}"),
                state_reads: Mutex::new(Vec::new()),
                event_cursors: Mutex::new(Vec::new()),
                event_streams: Mutex::new(Vec::new()),
                retained_events: Mutex::new(Vec::new()),
                submissions: Mutex::new(Vec::new()),
                actions: Mutex::new(Vec::new()),
                create_bodies: Mutex::new(Vec::new()),
                settings: Mutex::new(default_settings()),
                operations: Mutex::new(Vec::new()),
                catalogs: Mutex::new(Vec::new()),
                changed: Notify::new(),
                steer_entered: Notify::new(),
                steer_release: Mutex::new(None),
            }),
        }
    }

    async fn wait_for_event_cursor(&self, expected: &str) {
        loop {
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if lock(&self.inner.event_cursors)
                .iter()
                .any(|cursor| cursor == expected)
            {
                return;
            }
            changed.await;
        }
    }

    async fn send_event(&self, event: Bytes) {
        lock(&self.inner.retained_events).push(event.clone());
        loop {
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let senders = lock(&self.inner.event_streams)
                .iter()
                .filter(|sender| !sender.is_closed())
                .cloned()
                .collect::<Vec<_>>();
            if senders
                .into_iter()
                .any(|sender| sender.send(event.clone()).is_ok())
            {
                return;
            }
            changed.await;
        }
    }

    #[cfg(feature = "tools")]
    async fn wait_for_catalog(&self) {
        loop {
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !lock(&self.inner.catalogs).is_empty() {
                return;
            }
            changed.await;
        }
    }
}

#[tokio::test]
async fn live_open_survives_delayed_ready_large_replay_and_lost_admission_ack() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let api_key = format!("ncx_live_{}_{}", "e".repeat(12), "f".repeat(43));
        let fixture = Fixture::new(&api_key);
        let release = Arc::new(Notify::new());
        *lock(&fixture.inner.steer_release) = Some(release.clone());
        let app = Router::new()
            .route("/v1/agents/{agent_id}", get(agent_state))
            .route("/v1/agents/{agent_id}/ws", get(reconnecting_socket))
            .with_state(fixture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ManagedClient::new(
            format!("http://{address}"),
            ManagedApiKey::parse(api_key).unwrap(),
        )
        .unwrap();
        let (agent, mut events): (Nanocodex, AgentEvents) =
            Nanocodex::builder(Managed::open_live(client, AGENT_ID))
                .build()
                .await
                .expect("retained state should open before the socket is ready");
        let prompting =
            agent.prompt(PromptRequest::new("live prompt").request_id(ACTIVE_REQUEST_ID));
        tokio::pin!(prompting);
        assert!((&mut prompting).now_or_never().is_none());
        release.notify_one();
        let turn = prompting
            .await
            .expect("submission must consume a replay larger than the socket buffer");
        assert_result(
            &turn.result().await.unwrap(),
            ACTIVE_REQUEST_ID,
            "reconnected answer",
        );
        for sequence in 1..=401 {
            let event = events.recv().await.unwrap();
            assert_eq!(event.seq, sequence);
        }
        assert_eq!(*lock(&fixture.inner.event_cursors), ["40", "240", "440"]);
        {
            let submissions = lock(&fixture.inner.submissions);
            assert_eq!(submissions.len(), 2);
            assert_eq!(submissions[0].body, submissions[1].body);
        }
        agent.disconnect().await.unwrap();
        server.abort();
    })
    .await
    .expect("live reconnect and replay must remain bounded");
}

async fn reconnecting_socket(
    State(fixture): State<Fixture>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    authorize(&fixture, &headers);
    let cursor = query["cursor"].parse::<u64>().unwrap();
    lock(&fixture.inner.event_cursors).push(cursor.to_string());
    let release = lock(&fixture.inner.steer_release).take();
    upgrade.on_upgrade(move |mut socket| async move {
        if let Some(release) = release {
            release.notified().await;
        }
        let mut ready = agent_state_json(AGENT_ID, "440");
        ready["type"] = json!("ready");
        ready["session_id"] = json!(AGENT_ID);
        ready["restored"] = json!(true);
        socket
            .send(Message::Text(ready.to_string().into()))
            .await
            .unwrap();
        let end = if cursor == 40 { 240 } else { 440 };
        for next in cursor + 1..=end {
            let event = nested_event(
                next,
                ROOT_SOURCE_REQUEST_ID,
                None,
                "assistant.message",
                json!({"text": format!("event {next}")}),
            );
            socket
                .send(Message::Text(wire_event(event).into()))
                .await
                .unwrap();
        }
        if cursor == 40 {
            socket.send(Message::Close(None)).await.unwrap();
            return;
        }
        while let Some(Ok(Message::Text(frame))) = socket.recv().await {
            let command: Value = serde_json::from_str(&frame).unwrap();
            if command["type"] == "ping" {
                socket
                    .send(Message::Text(r#"{"type":"pong"}"#.into()))
                    .await
                    .unwrap();
                continue;
            }
            assert_eq!(command["type"], "prompt");
            assert_eq!(command["id"], ACTIVE_REQUEST_ID);
            lock(&fixture.inner.submissions).push(Submission {
                idempotency_key: ACTIVE_REQUEST_ID.to_owned(),
                body: command,
            });
            if cursor == 240 {
                // Lose only the admission response. The socket keeps answering
                // heartbeats, so a heartbeat timeout cannot rescue this wait.
                continue;
            }
            for event in [
                accepted_event(441, ACTIVE_REQUEST_ID, "live prompt"),
                nested_event(
                    442,
                    ROOT_SOURCE_REQUEST_ID,
                    None,
                    "run.completed",
                    json!({"status": "completed"}),
                ),
                completed_event(443, ACTIVE_REQUEST_ID, "reconnected answer"),
            ] {
                socket
                    .send(Message::Text(wire_event(event).into()))
                    .await
                    .unwrap();
            }
            // Keep the stream alive until the caller explicitly detaches.
            while socket.recv().await.is_some() {}
            return;
        }
    })
}

fn wire_event(event: Bytes) -> String {
    std::str::from_utf8(&event)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn detach_does_not_wait_for_an_unavailable_live_admission() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let api_key = format!("ncx_live_{}_{}", "g".repeat(12), "h".repeat(43));
        let fixture = Fixture::new(&api_key);
        let entered = Arc::new(Notify::new());
        let connected = entered.clone();
        let app = Router::new()
            .route("/v1/agents/{agent_id}", get(agent_state))
            .route(
                "/v1/agents/{agent_id}/ws",
                get(move |upgrade: WebSocketUpgrade| {
                    let connected = connected.clone();
                    async move {
                        upgrade.on_upgrade(move |mut socket| async move {
                            connected.notify_one();
                            // The connection exists but never sends its ready frame.
                            while socket.recv().await.is_some() {}
                        })
                    }
                }),
            )
            .with_state(fixture);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ManagedClient::new(
            format!("http://{address}"),
            ManagedApiKey::parse(api_key).unwrap(),
        )
        .unwrap();
        let (agent, _): (Nanocodex, AgentEvents) =
            Nanocodex::builder(Managed::open_live(client, AGENT_ID))
                .build()
                .await
                .unwrap();
        let prompting_agent = agent.clone();
        let prompting = tokio::spawn(async move {
            prompting_agent
                .prompt(PromptRequest::new("still owned remotely").request_id(ACTIVE_REQUEST_ID))
                .await
        });
        entered.notified().await;
        tokio::time::timeout(Duration::from_secs(1), agent.disconnect())
            .await
            .expect("detach must not wait for transport recovery")
            .unwrap();
        assert!(prompting.await.unwrap().is_err());
        server.abort();
    })
    .await
    .expect("detachment should remain bounded");
}

#[cfg(feature = "tools")]
#[tokio::test]
async fn public_managed_lifecycle_threads_attachment_metadata() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let api_key = format!("ncx_live_{}_{}", "c".repeat(12), "d".repeat(43));
        let fixture = Fixture::new(&api_key);
        let app = Router::new()
            .route("/v1/agents/{agent_id}", get(agent_state))
            .route("/v1/agents/{agent_id}/events", get(events))
            .route("/v1/agents/{agent_id}/tool-host", get(tool_host))
            .with_state(fixture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ManagedClient::new(
            format!("http://{address}"),
            ManagedApiKey::parse(api_key).unwrap(),
        )
        .unwrap();
        let machine = AttachmentMachine::new(
            "machine-public-1",
            "Public lifecycle host",
            "/workspace/public",
            ["native", "filesystem"],
        )
        .unwrap();
        let tools = Tools::builder().without_defaults().build().unwrap();
        let (agent, _): (Nanocodex, AgentEvents) =
            Nanocodex::builder(Managed::open(client, AGENT_ID))
                .tools(tools)
                .attachment_metadata(AttachmentMetadata::machine(machine))
                .build()
                .await
                .unwrap();
        fixture.wait_for_catalog().await;
        assert_eq!(
            lock(&fixture.inner.catalogs)[0],
            json!({
                "type": "catalog",
                "tools": [],
                "attachment_id": "machine-public-1",
                "machines": [{
                    "id": "machine-public-1",
                    "name": "Public lifecycle host",
                    "workspace": "/workspace/public",
                    "capabilities": ["native", "filesystem"]
                }]
            })
        );
        agent.disconnect().await.unwrap();
        server.abort();
    })
    .await
    .expect("metadata lifecycle should remain bounded");
}

#[tokio::test]
async fn public_managed_lifecycle_preserves_durable_identity_control_and_replay() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let api_key = format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43));
        let fixture = Fixture::new(&api_key);
        let app = Router::new()
            .route("/v1/agents", post(create_agent))
            .route("/v1/agents/{agent_id}", get(agent_state))
            .route("/v1/agents/{agent_id}/settings", patch(update_settings))
            .route("/v1/agents/{agent_id}/events", get(events))
            .route("/v1/agents/{agent_id}/turns", post(submit_turn))
            .route(
                "/v1/agents/{agent_id}/turns/{turn_id}/steer",
                post(steer_turn),
            )
            .route(
                "/v1/agents/{agent_id}/turns/{turn_id}/cancel",
                post(cancel_turn),
            )
            .with_state(fixture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener should bind");
        let address = listener
            .local_addr()
            .expect("loopback listener should have an address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("managed fixture should serve");
        });
        let client = ManagedClient::new(
            format!("http://{address}"),
            ManagedApiKey::parse(api_key).expect("fixture API key should validate"),
        )
        .expect("loopback managed client should build");

        let (observed_sender, mut observed_events) = mpsc::unbounded_channel();
        let (agent, mut events): (Nanocodex, AgentEvents) = Nanocodex::builder(
            Managed::create(client.clone()).with_settings(AgentSettings {
                model: Model::Terra,
                thinking: Thinking::Medium,
                reasoning_mode: ReasoningMode::Pro,
                fast_mode: true,
            }),
        )
        .event_observer(observed_sender)
        .build()
        .await
        .expect("public managed create should build");
        assert_eq!(agent.session_id().to_string(), SESSION_ID);
        assert_eq!(events.request_id(), SESSION_ID);
        fixture.wait_for_event_cursor("40").await;
        assert_eq!(lock(&fixture.inner.event_cursors).len(), 1);

        let settings = client
            .set_model(AGENT_ID, Model::Luna)
            .await
            .expect("model should remain mutable before first admission");
        assert_eq!(settings.model, Model::Luna);
        agent
            .set_thinking(Thinking::Xhigh)
            .await
            .expect("thinking should update through the driver");
        agent
            .set_fast_mode(false)
            .await
            .expect("fast mode should update through the driver");

        let cancelled = agent
            .prompt(
                PromptRequest::new("cancel before managed work")
                    .request_id(CANCELLED_REQUEST_ID)
                    .cancel_on_admission(),
            )
            .await
            .expect("cancelled managed prompt should be admitted");
        assert_eq!(cancelled.request_id(), Some(CANCELLED_REQUEST_ID));
        assert!(matches!(
            cancelled.result().await,
            Err(NanocodexError::TurnCancelled)
        ));

        let mut turn = agent
            .prompt(PromptRequest::new("live prompt").request_id(ACTIVE_REQUEST_ID))
            .await
            .expect("live prompt should be accepted");
        assert_eq!(turn.request_id(), Some(ACTIVE_REQUEST_ID));
        let control: TurnControl = turn.control();
        let steer_release = Arc::new(Notify::new());
        *lock(&fixture.inner.steer_release) = Some(steer_release.clone());
        let steering_control = control.clone();
        let steering =
            tokio::spawn(async move { steering_control.steer("follow-up steering").await });
        fixture.inner.steer_entered.notified().await;
        fixture
            .send_event(accepted_event(41, ACTIVE_REQUEST_ID, "live prompt"))
            .await;
        let first_observed = observed_events
            .recv()
            .await
            .expect("durable events must continue while steering waits");
        assert_eq!(first_observed.cursor.as_str(), "41");
        control
            .cancel()
            .await
            .expect("public turn control should cancel");
        steer_release.notify_one();
        steering
            .await
            .unwrap()
            .expect("public turn control should steer");
        let shutdown_agent = agent.clone();
        let shutdown = shutdown_agent.shutdown();
        tokio::pin!(shutdown);
        assert!(
            (&mut shutdown).now_or_never().is_none(),
            "shutdown must wait for the durable turn terminal"
        );

        fixture
            .send_event(completed_event_with_usage(
                42,
                FOREIGN_REQUEST_ID,
                "foreign answer",
                json!({"future_usage_schema": true}),
            ))
            .await;
        fixture
            .send_event(nested_event(
                43,
                ROOT_SOURCE_REQUEST_ID,
                None,
                "assistant.message",
                json!({"text": "live"}),
            ))
            .await;
        fixture
            .send_event(nested_event(
                44,
                CHILD_SOURCE_REQUEST_ID,
                Some(7),
                "assistant.message",
                json!({"text": "child"}),
            ))
            .await;
        fixture
            .send_event(nested_event(
                45,
                CHILD_SOURCE_REQUEST_ID,
                Some(7),
                "run.completed",
                json!({"status": "completed"}),
            ))
            .await;
        fixture
            .send_event(nested_event(
                46,
                ROOT_SOURCE_REQUEST_ID,
                None,
                "assistant.message",
                json!({"text": "after child"}),
            ))
            .await;
        assert!(
            (&mut turn).now_or_never().is_none(),
            "a nested subagent terminal must not stop the parent turn"
        );
        fixture
            .send_event(nested_event(
                47,
                ROOT_SOURCE_REQUEST_ID,
                None,
                "run.completed",
                json!({"status": "completed"}),
            ))
            .await;

        assert!(
            (&mut turn).now_or_never().is_none(),
            "the result must not complete from the nested run terminal alone"
        );
        assert!(
            (&mut shutdown).now_or_never().is_none(),
            "shutdown must not complete from the nested run terminal alone"
        );

        fixture
            .send_event(completed_event(48, ACTIVE_REQUEST_ID, "live answer"))
            .await;
        let mut published = Vec::new();
        for _ in 0..5 {
            published.push(
                events
                    .recv()
                    .await
                    .expect("rewritten parent event stream should remain open"),
            );
        }
        assert_eq!(
            published.iter().map(|event| event.kind).collect::<Vec<_>>(),
            [
                AgentEventKind::AssistantMessage,
                AgentEventKind::AssistantMessage,
                AgentEventKind::RunCompleted,
                AgentEventKind::AssistantMessage,
                AgentEventKind::RunCompleted,
            ]
        );
        assert_eq!(
            published.iter().map(|event| event.seq).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        assert!(
            published
                .iter()
                .all(|event| event.request_id.as_ref() == SESSION_ID)
        );
        let result: TurnResult = turn
            .await
            .expect("live terminal should complete the result");
        assert_result(&result, ACTIVE_REQUEST_ID, "live answer");

        let mut observed = vec![first_observed];
        for _ in 0..7 {
            observed.push(
                observed_events
                    .recv()
                    .await
                    .expect("ordered managed observer should remain open"),
            );
        }
        assert_eq!(
            observed
                .iter()
                .map(|event| event.cursor.as_str())
                .collect::<Vec<_>>(),
            ["41", "42", "43", "44", "45", "46", "47", "48"]
        );
        assert!(matches!(
            &observed[0].data,
            ManagedEventData::TurnAccepted {
                input: PromptInput::Text(input),
                ..
            } if input == "live prompt"
        ));
        assert!(matches!(
            observed[1].data,
            ManagedEventData::TurnCompleted { .. }
        ));
        assert!(matches!(observed[2].data, ManagedEventData::Event { .. }));
        assert!(matches!(observed[3].data, ManagedEventData::Event { .. }));
        assert!(matches!(
            observed[7].data,
            ManagedEventData::TurnCompleted { .. }
        ));
        assert_eq!(
            lock(&fixture.inner.event_cursors).len(),
            1,
            "the observer must tap the lifecycle stream instead of opening another subscription"
        );

        shutdown
            .await
            .expect("completed managed agent should shut down");

        let (reopened, reopened_events): (Nanocodex, AgentEvents) =
            Nanocodex::builder(Managed::open(client.clone(), AGENT_ID))
                .build()
                .await
                .expect("public managed open should build");
        assert_eq!(reopened.session_id().to_string(), SESSION_ID);
        assert_eq!(reopened_events.request_id(), SESSION_ID);
        fixture.wait_for_event_cursor("44").await;

        let retained = reopened
            .prompt(PromptRequest::new("retained prompt").request_id(RETAINED_REQUEST_ID))
            .await
            .expect("idempotent retained prompt should be accepted");
        let retained_result: TurnResult = retained
            .await
            .expect("retained terminal should not wait for silent historical SSE");
        assert_result(&retained_result, RETAINED_REQUEST_ID, "retained answer");
        reopened
            .shutdown()
            .await
            .expect("reopened managed agent should shut down");

        let state = client
            .state(AGENT_ID)
            .await
            .expect("public managed state snapshot should load");
        let state_reads_before_open = lock(&fixture.inner.state_reads).len();
        let mut invalid_state = state.clone();
        invalid_state.latest_event_cursor = "latest".to_owned();
        let invalid_open: nanocodex_agent::Result<(Nanocodex, AgentEvents)> = Nanocodex::builder(
            Managed::open_from_state(client.clone(), AGENT_ID, invalid_state),
        )
        .build()
        .await;
        let error = match invalid_open {
            Ok(_) => panic!("state-fenced open must reject the latest sentinel"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("agent state latest event cursor is invalid")
        );
        let mut invalid_settings_state = state.clone();
        invalid_settings_state.settings.model = Model::Astra;
        invalid_settings_state.settings.thinking = Thinking::None;
        let invalid_open: nanocodex_agent::Result<(Nanocodex, AgentEvents)> = Nanocodex::builder(
            Managed::open_from_state(client.clone(), AGENT_ID, invalid_settings_state),
        )
        .build()
        .await;
        let error = match invalid_open {
            Ok(_) => panic!("state-fenced open must reject incompatible settings"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("incompatible model and reasoning settings")
        );
        let (from_state, _): (Nanocodex, AgentEvents) =
            Nanocodex::builder(Managed::open_from_state(client.clone(), AGENT_ID, state))
                .build()
                .await
                .expect("public managed state-fenced open should build");
        fixture.wait_for_event_cursor("44").await;
        assert_eq!(
            lock(&fixture.inner.state_reads).len(),
            state_reads_before_open,
            "opening from a validated state must not repeat the state request"
        );
        from_state
            .disconnect()
            .await
            .expect("state-fenced agent should disconnect");

        assert_eq!(
            lock(&fixture.inner.state_reads).as_slice(),
            [AGENT_ID, AGENT_ID, AGENT_ID]
        );
        {
            let event_cursors = lock(&fixture.inner.event_cursors);
            assert_eq!(event_cursors.first().map(String::as_str), Some("40"));
            assert!(event_cursors.iter().any(|cursor| cursor == "44"));
            assert!(
                event_cursors
                    .iter()
                    .all(|cursor| cursor == "40" || cursor == "44")
            );
        }

        {
            let submissions = lock(&fixture.inner.submissions);
            assert_eq!(submissions.len(), 3);
            assert_eq!(submissions[0].idempotency_key, CANCELLED_REQUEST_ID);
            assert_eq!(
                submissions[0].body,
                json!({
                    "id": CANCELLED_REQUEST_ID,
                    "input": "cancel before managed work"
                })
            );
            assert_eq!(submissions[1].idempotency_key, ACTIVE_REQUEST_ID);
            assert_eq!(
                submissions[1].body,
                json!({"id": ACTIVE_REQUEST_ID, "input": "live prompt"})
            );
            assert_eq!(submissions[2].idempotency_key, RETAINED_REQUEST_ID);
            assert_eq!(
                submissions[2].body,
                json!({"id": RETAINED_REQUEST_ID, "input": "retained prompt"})
            );
        }

        assert_eq!(
            lock(&fixture.inner.operations).as_slice(),
            [
                "create", "settings", "settings", "settings", "submit", "submit", "submit"
            ]
        );
        assert_eq!(
            lock(&fixture.inner.create_bodies).as_slice(),
            [json!({
                "settings": {
                    "model": "gpt-5.6-terra",
                    "thinking": "medium",
                    "reasoning_mode": "pro",
                    "fast_mode": true
                }
            })]
        );
        assert_eq!(
            lock(&fixture.inner.settings).clone(),
            json!({
                "model": "gpt-5.6-luna",
                "thinking": "xhigh",
                "reasoning_mode": "pro",
                "fast_mode": false
            })
        );

        {
            let actions = lock(&fixture.inner.actions);
            assert_eq!(actions.len(), 4);
            assert_eq!(actions[0].kind, "cancel");
            assert_eq!(actions[0].agent_id, AGENT_ID);
            assert_eq!(actions[0].turn_id, CANCELLED_REQUEST_ID);
            assert_eq!(actions[0].body, None);
            assert_eq!(actions[1].kind, "steer");
            assert_eq!(actions[1].agent_id, AGENT_ID);
            assert_eq!(actions[1].turn_id, ACTIVE_REQUEST_ID);
            assert_eq!(
                actions[1].body,
                Some(json!({"input": "follow-up steering"}))
            );
            assert_eq!(actions[2].kind, "cancel");
            assert_eq!(actions[2].agent_id, AGENT_ID);
            assert_eq!(actions[2].turn_id, ACTIVE_REQUEST_ID);
            assert_eq!(actions[2].body, None);
            assert_eq!(actions[3].kind, "cancel");
            assert_eq!(actions[3].agent_id, AGENT_ID);
            assert_eq!(actions[3].turn_id, ACTIVE_REQUEST_ID);
            assert_eq!(actions[3].body, None);
        }

        let latest_error = client
            .state("agent-latest-cursor")
            .await
            .expect_err("state client must reject the non-exact latest sentinel");
        assert!(matches!(
            latest_error,
            ManagedError::InvalidResponse("agent state latest event cursor is invalid")
        ));

        server.abort();
    })
    .await
    .expect("public managed lifecycle test should remain bounded");
}

async fn create_agent(
    State(fixture): State<Fixture>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    authorize(&fixture, &headers);
    let body: Value = serde_json::from_slice(&body).expect("create settings should be JSON");
    assert_eq!(body.as_object().map(serde_json::Map::len), Some(1));
    let settings = body
        .get("settings")
        .cloned()
        .expect("create should carry complete settings");
    *lock(&fixture.inner.settings) = settings;
    lock(&fixture.inner.create_bodies).push(body);
    lock(&fixture.inner.operations).push("create");
    json_response(
        StatusCode::CREATED,
        json!({
            "agent_id": AGENT_ID,
            "session_id": SESSION_ID,
            "events_url": format!("http://unused/v1/agents/{AGENT_ID}/events"),
            "websocket_url": format!("ws://unused/v1/agents/{AGENT_ID}/ws"),
        }),
    )
}

async fn agent_state(
    State(fixture): State<Fixture>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
) -> Response<Body> {
    authorize(&fixture, &headers);
    if agent_id == "agent-latest-cursor" {
        return json_response(
            StatusCode::OK,
            agent_state_json("agent-latest-cursor", "latest"),
        );
    }
    let latest_event_cursor = {
        let mut reads = lock(&fixture.inner.state_reads);
        reads.push(agent_id);
        if reads.len() == 1 { "40" } else { "44" }
    };
    let mut state = agent_state_json(AGENT_ID, latest_event_cursor);
    state["settings"] = lock(&fixture.inner.settings).clone();
    json_response(StatusCode::OK, state)
}

async fn update_settings(
    State(fixture): State<Fixture>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response<Body> {
    authorize(&fixture, &headers);
    assert_eq!(agent_id, AGENT_ID);
    assert!(body.as_object().is_some_and(|body| !body.is_empty()));
    assert!(body.as_object().is_some_and(|body| body.keys().all(|key| {
        matches!(
            key.as_str(),
            "model" | "thinking" | "reasoning_mode" | "fast_mode"
        )
    })));
    let mut settings = lock(&fixture.inner.settings);
    for (key, value) in body
        .as_object()
        .expect("settings request was checked as an object")
    {
        settings[key] = value.clone();
    }
    lock(&fixture.inner.operations).push("settings");
    json_response(StatusCode::OK, json!({"settings": settings.clone()}))
}

#[cfg(feature = "tools")]
async fn tool_host(
    State(fixture): State<Fixture>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    authorize(&fixture, &headers);
    assert_eq!(agent_id, AGENT_ID);
    upgrade.on_upgrade(move |mut socket| async move {
        let Some(Ok(Message::Text(catalog))) = socket.recv().await else {
            return;
        };
        lock(&fixture.inner.catalogs).push(serde_json::from_str(&catalog).unwrap());
        fixture.inner.changed.notify_waiters();
        socket
            .send(Message::Text(json!({"type": "ready"}).to_string().into()))
            .await
            .unwrap();
        while let Some(Ok(Message::Text(frame))) = socket.recv().await {
            if serde_json::from_str::<Value>(&frame).unwrap()["type"] == "drain" {
                socket
                    .send(Message::Text(
                        json!({"type": "draining"}).to_string().into(),
                    ))
                    .await
                    .unwrap();
            }
        }
    })
}

fn agent_state_json(agent_id: &str, latest_event_cursor: &str) -> Value {
    json!({
        "agent_id": agent_id,
        "session_id": SESSION_ID,
        "has_snapshot": true,
        "completed_turns": 0,
        "last_active": 1,
        "active_turns": [],
        "active_turn_details": [],
        "agent_loaded": true,
        "connected_clients": 0,
        "capabilities": {
            "durable_turns": true,
            "resumable_events": true,
            "live_steer": true,
            "live_cancel": true,
            "workspace": "cloud",
            "execution_environments": true,
            "execution_namespace": "cwd-root-v1",
            "native_cross_mounts": false
        },
        "settings": {
            "model": "gpt-5.6-sol",
            "thinking": "high",
            "reasoning_mode": "standard",
            "fast_mode": false
        },
        "latest_event_cursor": latest_event_cursor,
        "stream_error": null
    })
}

async fn events(
    State(fixture): State<Fixture>,
    Path(agent_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response<Body> {
    authorize(&fixture, &headers);
    assert_eq!(agent_id, AGENT_ID);
    let cursor = query
        .get("cursor")
        .expect("event request should carry a cursor")
        .clone();
    let (sender, receiver) = mpsc::unbounded_channel();
    for event in lock(&fixture.inner.retained_events).iter().cloned() {
        drop(sender.send(event));
    }
    lock(&fixture.inner.event_cursors).push(cursor);
    lock(&fixture.inner.event_streams).push(sender);
    fixture.inner.changed.notify_waiters();
    let body = Body::from_stream(stream::unfold(receiver, |mut receiver| async move {
        receiver
            .recv()
            .await
            .map(|bytes| (Ok::<_, Infallible>(bytes), receiver))
    }));
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(body)
        .expect("SSE response should build")
}

async fn submit_turn(
    State(fixture): State<Fixture>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response<Body> {
    authorize(&fixture, &headers);
    assert_eq!(agent_id, AGENT_ID);
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .expect("submission should carry an idempotency key")
        .to_owned();
    assert_eq!(
        body.get("id").and_then(Value::as_str),
        Some(idempotency_key.as_str())
    );
    lock(&fixture.inner.submissions).push(Submission {
        idempotency_key: idempotency_key.clone(),
        body,
    });
    lock(&fixture.inner.operations).push("submit");
    match idempotency_key.as_str() {
        ACTIVE_REQUEST_ID => json_response(
            StatusCode::ACCEPTED,
            turn_view(
                ACTIVE_REQUEST_ID,
                "accepted",
                "live prompt",
                "41",
                None,
                None,
            ),
        ),
        CANCELLED_REQUEST_ID => {
            let terminal = json!({
                "type": "turn_cancelled",
                "id": CANCELLED_REQUEST_ID
            });
            json_response(
                StatusCode::OK,
                turn_view(
                    CANCELLED_REQUEST_ID,
                    "cancelled",
                    "cancel before managed work",
                    "40",
                    Some("40"),
                    Some(terminal),
                ),
            )
        }
        RETAINED_REQUEST_ID => {
            let terminal = json!({
                "type": "turn_completed",
                "id": RETAINED_REQUEST_ID,
                "final_message": "retained answer",
                "usage": exact_usage(),
                "citations": [],
                "usage_error": null
            });
            json_response(
                StatusCode::OK,
                turn_view(
                    RETAINED_REQUEST_ID,
                    "completed",
                    "retained prompt",
                    "43",
                    Some("43"),
                    Some(terminal),
                ),
            )
        }
        other => panic!("unexpected idempotency key {other}"),
    }
}

async fn steer_turn(
    State(fixture): State<Fixture>,
    Path((agent_id, turn_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    authorize(&fixture, &headers);
    lock(&fixture.inner.actions).push(Action {
        kind: "steer",
        agent_id,
        turn_id: turn_id.clone(),
        body: Some(body),
    });
    let release = lock(&fixture.inner.steer_release).take();
    if let Some(release) = release {
        fixture.inner.steer_entered.notify_one();
        release.notified().await;
    }
    Json(json!({"turn_id": turn_id, "state": "cancelling"}))
}

async fn cancel_turn(
    State(fixture): State<Fixture>,
    Path((agent_id, turn_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    authorize(&fixture, &headers);
    lock(&fixture.inner.actions).push(Action {
        kind: "cancel",
        agent_id,
        turn_id: turn_id.clone(),
        body: None,
    });
    Json(json!({"turn_id": turn_id, "state": "cancelling"}))
}

fn authorize(fixture: &Fixture, headers: &HeaderMap) {
    assert_eq!(
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some(fixture.inner.authorization.as_str())
    );
}

fn turn_view(
    turn_id: &str,
    state: &str,
    input: &str,
    accepted_cursor: &str,
    terminal_cursor: Option<&str>,
    terminal: Option<Value>,
) -> Value {
    json!({
        "turn_id": turn_id,
        "state": state,
        "input": input,
        "accepted_cursor": accepted_cursor,
        "terminal_cursor": terminal_cursor,
        "created_at": 1,
        "accepted_at": 1,
        "updated_at": 1,
        "attempt_count": 1,
        "retry_at": null,
        "error": null,
        "terminal": terminal
    })
}

fn nested_event(
    cursor: u64,
    request_id: &str,
    agent_id: Option<u64>,
    kind: &str,
    payload: Value,
) -> Bytes {
    let mut envelope = json!({
        "cursor": cursor.to_string(),
        "created_at": cursor,
        "turn_id": ACTIVE_REQUEST_ID,
        "type": "event",
        "event": {
            "protocol_version": 1,
            "request_id": request_id,
            "seq": 1,
            "type": kind,
            "payload": payload
        }
    });
    if let Some(agent_id) = agent_id {
        envelope["agent_id"] = agent_id.into();
    }
    Bytes::from(format!("id: {cursor}\nevent: event\ndata: {envelope}\n\n"))
}

fn accepted_event(cursor: u64, turn_id: &str, input: &str) -> Bytes {
    let envelope = json!({
        "cursor": cursor.to_string(),
        "created_at": cursor,
        "turn_id": turn_id,
        "type": "turn_accepted",
        "id": turn_id,
        "input": input,
        "replayed": false
    });
    Bytes::from(format!(
        "id: {cursor}\nevent: turn_accepted\ndata: {envelope}\n\n"
    ))
}

fn completed_event(cursor: u64, turn_id: &str, final_message: &str) -> Bytes {
    completed_event_with_usage(cursor, turn_id, final_message, exact_usage())
}

fn completed_event_with_usage(
    cursor: u64,
    turn_id: &str,
    final_message: &str,
    usage: Value,
) -> Bytes {
    let envelope = json!({
        "cursor": cursor.to_string(),
        "created_at": cursor,
        "turn_id": turn_id,
        "type": "turn_completed",
        "id": turn_id,
        "final_message": final_message,
        "usage": usage,
        "citations": [],
        "usage_error": null
    });
    Bytes::from(format!(
        "id: {cursor}\nevent: turn_completed\ndata: {envelope}\n\n"
    ))
}

fn exact_usage() -> Value {
    json!({
        "input_tokens": 101,
        "cached_input_tokens": 17,
        "cache_write_input_tokens": 9,
        "output_tokens": 23,
        "reasoning_output_tokens": 7,
        "total_tokens": 124,
        "estimated_cost": null,
        "cost_status": "usage_not_reported"
    })
}

fn default_settings() -> Value {
    json!({
        "model": "gpt-5.6-sol",
        "thinking": "high",
        "reasoning_mode": "standard",
        "fast_mode": false
    })
}

fn assert_result(result: &TurnResult, request_id: &str, final_message: &str) {
    assert_eq!(result.request_id(), Some(request_id));
    assert_eq!(result.final_message(), final_message);
    let usage = result.usage().expect("exact managed usage should survive");
    assert_eq!(usage.input_tokens(), 101);
    assert_eq!(usage.cached_input_tokens(), 17);
    assert_eq!(usage.cache_write_input_tokens(), 9);
    assert_eq!(usage.output_tokens(), 23);
    assert_eq!(usage.reasoning_output_tokens(), 7);
    assert_eq!(usage.total_tokens(), 124);
    assert_eq!(usage.cost_status().as_str(), "usage_not_reported");
    assert!(usage.estimated_cost().is_none());
}

fn json_response(status: StatusCode, body: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("JSON response should build")
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
