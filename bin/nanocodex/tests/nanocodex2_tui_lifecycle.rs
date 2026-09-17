//! Exercise real terminal input against a controlled managed service.

use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    routing::{get, post, put},
};
use base64::Engine as _;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

const AGENT: &str = "019fc927-b280-79a7-8445-1b9996ad2fb0";
const REMOTE_TURN: &str = "019fc927-b281-79a7-8445-1b9996ad2fb0";
const VAULT_ID: &str = "abcdefghijklmnopqrstuv";
const VAULT_ORIGIN: &str = "https://vault-approval.example:8443";
const TIMEOUT: Duration = Duration::from_secs(10);

fn prompt_text(input: &Value) -> String {
    match input {
        Value::String(text) => text.clone(),
        Value::Array(content) => content
            .iter()
            .filter_map(|item| item["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => panic!("unexpected prompt: {input}"),
    }
}

struct Terminal {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    output: Arc<Mutex<Vec<u8>>>,
    screen: Arc<Mutex<vt100::Parser>>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
    _workspace: tempfile::TempDir,
}

impl Terminal {
    fn start(origin: &str, attach: bool) -> Self {
        let workspace = tempfile::tempdir().unwrap();
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 32,
                cols: 160,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_nanocodex2"));
        if attach {
            command.args(["attach", AGENT]);
        }
        command.cwd(workspace.path());
        // This PTY is not a tmux client, regardless of the developer's shell.
        // Inheriting TMUX used to skip a broken terminal capability probe.
        command.env_remove("TMUX");
        command.env_remove("TMUX_PANE");
        command.env_remove("TERM_PROGRAM");
        command.env("TERM", "xterm-256color");
        command.env("NANOCODEX_MANAGED_URL", origin);
        command.env(
            "NANOCODEX_API_KEY",
            format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)),
        );
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        let captured = output.clone();
        let screen = Arc::new(Mutex::new(vt100::Parser::new(32, 160, 0)));
        let parsed = screen.clone();
        std::thread::spawn(move || {
            let mut bytes = [0; 8192];
            while let Ok(count) = reader.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                captured.lock().unwrap().extend_from_slice(&bytes[..count]);
                parsed.lock().unwrap().process(&bytes[..count]);
            }
        });
        Self {
            child,
            writer,
            output,
            screen,
            _master: pair.master,
            _workspace: workspace,
        }
    }

    fn input(&mut self, input: &str) {
        self.writer.write_all(input.as_bytes()).unwrap();
        self.writer.flush().unwrap();
    }

    fn prompt(&mut self, input: &str, key: &str) {
        self.input(&format!("\x1b[200~{input}\x1b[201~{key}"));
    }

    fn resize(&self, cols: u16) {
        self.screen.lock().unwrap().set_size(32, cols);
        self._master
            .resize(PtySize {
                rows: 32,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
    }

    async fn wait_text(&self, text: &str) {
        self.wait_text_presence(text, true).await;
    }

    async fn wait_no_text(&self, text: &str) {
        self.wait_text_presence(text, false).await;
    }

    async fn wait_text_presence(&self, text: &str, present: bool) {
        tokio::time::timeout(TIMEOUT, async {
            loop {
                if self
                    .screen
                    .lock()
                    .unwrap()
                    .screen()
                    .contents()
                    .contains(text)
                    == present
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            let output = self.output.lock().unwrap().clone();
            panic!(
                "terminal text {text:?} should have presence={present}: {}\nRaw tail: {:?}",
                self.screen.lock().unwrap().screen().contents(),
                String::from_utf8_lossy(&output[output.len().saturating_sub(1000)..])
            );
        });
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Clone)]
struct Service {
    vault_writes: Arc<Mutex<Vec<Value>>>,
    listed_agent: Arc<Mutex<String>>,
    resume_gate: Arc<tokio::sync::Semaphore>,
    active: bool,
    state_available: Arc<AtomicBool>,
    settings: Arc<Mutex<Value>>,
    history_gate: Arc<tokio::sync::Semaphore>,
    session_list_gate: Arc<tokio::sync::Semaphore>,
    history_requests: Arc<Mutex<Vec<u64>>>,
    history: Arc<Mutex<Vec<Value>>>,
    connected: mpsc::UnboundedSender<mpsc::UnboundedSender<Value>>,
    submitted: mpsc::UnboundedSender<Value>,
    steered: mpsc::UnboundedSender<(Value, oneshot::Sender<bool>)>,
    rejected: mpsc::UnboundedSender<Value>,
    cancelled: mpsc::UnboundedSender<String>,
}

impl Service {
    fn active_turns(&self) -> Vec<String> {
        let mut active = std::collections::BTreeSet::new();
        if self.active {
            active.insert(REMOTE_TURN.to_owned());
        }
        for event in self.history.lock().unwrap().iter() {
            if let Some(id) = event["id"].as_str() {
                match event["type"].as_str() {
                    Some("turn_accepted") => {
                        active.insert(id.to_owned());
                    }
                    Some("turn_completed" | "turn_failed" | "turn_cancelled") => {
                        active.remove(id);
                    }
                    _ => {}
                }
            }
        }
        active.into_iter().collect()
    }

    fn latest_cursor(&self) -> String {
        self.history
            .lock()
            .unwrap()
            .last()
            .map_or("0", |event| event["cursor"].as_str().unwrap())
            .to_owned()
    }
}

async fn vault_metadata() -> Json<Value> {
    Json(json!({"vault": [{
        "id": VAULT_ID, "kind": "login", "name": "VERIFIED_SAVED_LOGIN",
        "browser_origin": "https://previous.example",
        "username": "PRIVATE_USERNAME_SENTINEL", "password": "PRIVATE_PASSWORD_SENTINEL"
    }]}))
}

async fn approve_vault_origin(
    State(service): State<Service>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    service
        .vault_writes
        .lock()
        .unwrap()
        .push(json!({"id": id, "body": body}));
    Json(json!({
        "id": id, "kind": "login", "name": "VERIFIED_SAVED_LOGIN",
        "browser_origin": body["browser_origin"], "password": "PRIVATE_PASSWORD_SENTINEL"
    }))
}

async fn list_agents(State(service): State<Service>) -> Json<Value> {
    let _permit = service.session_list_gate.acquire().await.unwrap();
    let agent = service.listed_agent.lock().unwrap().clone();
    Json(
        json!({"data": [agent], "summaries": {agent: {"title": "RETAINED_REMOTE_WORK", "created_at": 1, "updated_at": 1, "turn_count": 1}}}),
    )
}

async fn event_history(
    State(service): State<Service>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<Value> {
    let _permit = service.history_gate.acquire().await.unwrap();
    let before = query
        .get("before")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(u64::MAX);
    let limit = query
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100);
    service.history_requests.lock().unwrap().push(before);
    let events = service.history.lock().unwrap();
    let data: Vec<_> = events
        .iter()
        .filter(|event| event["cursor"].as_str().unwrap().parse::<u64>().unwrap() < before)
        .cloned()
        .collect();
    let start = data.len().saturating_sub(limit);
    Json(
        json!({"data": data[start..], "has_more": start > 0, "latest_cursor": events.last().map_or("0", |event| event["cursor"].as_str().unwrap())}),
    )
}

async fn socket(
    State(service): State<Service>,
    uri: axum::http::Uri,
    upgrade: WebSocketUpgrade,
    Query(query): Query<HashMap<String, String>>,
) -> axum::response::Response {
    let cursor = query
        .get("cursor")
        .and_then(|cursor| cursor.parse().ok())
        .unwrap_or(0);
    let agent = uri
        .path()
        .strip_prefix("/v1/agents/")
        .and_then(|path| path.strip_suffix("/ws"))
        .unwrap_or(AGENT)
        .to_owned();
    upgrade.on_upgrade(move |socket| serve(socket, service, cursor, agent))
}

async fn serve(mut socket: WebSocket, service: Service, cursor: u64, agent: String) {
    let history = service.history.lock().unwrap().clone();
    let latest_cursor = history
        .last()
        .map_or("0", |event| event["cursor"].as_str().unwrap());
    let (outgoing, mut events) = mpsc::unbounded_channel::<Value>();
    let ready = json!({
        "type": "ready", "session_id": agent, "restored": false,
        "active_turns": service.active_turns(), "active_turn_details": [], "latest_event_cursor": latest_cursor,
        "capabilities": {"durable_turns": true, "resumable_events": true,
            "live_steer": true, "live_cancel": true, "workspace": "cloudflare-computer",
            "execution_environments": true, "execution_namespace": "cwd-root-v1", "native_cross_mounts": false},
        "settings": {"model": "gpt-6-astra", "thinking": "low", "reasoning_mode": "standard", "fast_mode": false}
    });
    if socket
        .send(Message::Text(ready.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    for event in history
        .iter()
        .filter(|event| event["cursor"].as_str().unwrap().parse::<u64>().unwrap() > cursor)
    {
        if socket
            .send(Message::Text(event.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
    }
    if service.connected.send(outgoing).is_err() {
        return;
    }
    loop {
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else { break; };
                if event.is_null() { let _ = socket.send(Message::Close(None)).await; break; }
                if socket.send(Message::Text(event.to_string().into())).await.is_err() { break; }
            }
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let mut message: Value = serde_json::from_str(&text).unwrap();
                        if message["type"] == "prompt" {
                            message["fixture_agent_id"] = json!(agent);
                            let _ = service.submitted.send(message);
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

async fn state(
    State(service): State<Service>,
    axum::extract::Path(agent): axum::extract::Path<String>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let _resume = if agent != AGENT {
        Some(service.resume_gate.acquire().await.unwrap())
    } else {
        None
    };
    if !service.state_available.load(Ordering::SeqCst) {
        return Err((
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "unavailable", "message": "try again"})),
        ));
    }
    Ok(Json(json!({
        "agent_id": agent, "session_id": agent, "has_snapshot": false,
        "completed_turns": 0, "last_active": 1, "agent_loaded": true, "connected_clients": 1,
        "active_turns": service.active_turns(), "active_turn_details": [],
        "capabilities": {"durable_turns": true, "resumable_events": true,
            "live_steer": true, "live_cancel": true, "workspace": "cloudflare-computer",
            "execution_environments": true, "execution_namespace": "cwd-root-v1", "native_cross_mounts": false},
        "settings": service.settings.lock().unwrap().clone(),
        "latest_event_cursor": service.latest_cursor(), "stream_error": null
    })))
}

async fn submit(State(service): State<Service>, Json(input): Json<Value>) -> Json<Value> {
    service.submitted.send(input.clone()).unwrap();
    Json(json!({
        "turn_id": input["id"], "state": "accepted", "input": input["input"],
        "accepted_cursor": "1", "terminal_cursor": null,
        "created_at": 1, "accepted_at": 1, "updated_at": 1, "attempt_count": 1,
        "retry_at": null, "error": null, "terminal": null
    }))
}

async fn steer(
    State(service): State<Service>,
    axum::extract::Path((_, turn)): axum::extract::Path<(String, String)>,
    Json(mut input): Json<Value>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    if service.history.lock().unwrap().iter().any(|event| {
        event["turn_id"] == turn
            && matches!(
                event["type"].as_str(),
                Some("turn_completed" | "turn_cancelled" | "turn_failed")
            )
    }) {
        let _ = service.rejected.send(input);
        return Err((
            axum::http::StatusCode::CONFLICT,
            Json(json!({"error": "turn_not_active", "message": "turn is finished"})),
        ));
    }
    let (ack, acknowledged) = oneshot::channel();
    input["turn_id"] = json!(turn);
    service.steered.send((input, ack)).unwrap();
    if !acknowledged.await.unwrap_or(false) {
        return Err((
            axum::http::StatusCode::BAD_GATEWAY,
            Json(
                json!({"error": "upstream_failure", "message": "steering acknowledgement was lost"}),
            ),
        ));
    }
    Ok(Json(json!({"turn_id": turn, "state": "steering"})))
}

async fn cancel(
    State(service): State<Service>,
    axum::extract::Path((_, turn)): axum::extract::Path<(String, String)>,
) -> Json<Value> {
    service.cancelled.send(turn.clone()).unwrap();
    Json(json!({"turn_id": turn, "state": "cancelling"}))
}

struct Fixture {
    vault_writes: Arc<Mutex<Vec<Value>>>,
    listed_agent: Arc<Mutex<String>>,
    resume_gate: Arc<tokio::sync::Semaphore>,
    origin: String,
    terminal: Terminal,
    state_available: Arc<AtomicBool>,
    settings: Arc<Mutex<Value>>,
    history_gate: Arc<tokio::sync::Semaphore>,
    session_list_gate: Arc<tokio::sync::Semaphore>,
    history_requests: Arc<Mutex<Vec<u64>>>,
    events: mpsc::UnboundedSender<Value>,
    connections: mpsc::UnboundedReceiver<mpsc::UnboundedSender<Value>>,
    history: Arc<Mutex<Vec<Value>>>,
    submissions: mpsc::UnboundedReceiver<Value>,
    steers: mpsc::UnboundedReceiver<(Value, oneshot::Sender<bool>)>,
    rejections: mpsc::UnboundedReceiver<Value>,
    cancellations: mpsc::UnboundedReceiver<String>,
    server: tokio::task::JoinHandle<()>,
    cursor: u64,
}

impl Fixture {
    async fn start() -> Self {
        Self::start_with_active(false).await
    }

    async fn start_with_active(active: bool) -> Self {
        Self::start_with_history(active, active, Vec::new()).await
    }

    async fn start_with_history(active: bool, attach: bool, initial_history: Vec<Value>) -> Self {
        let fixture = Self::launch_with_history(
            active,
            attach,
            initial_history,
            Arc::new(tokio::sync::Semaphore::new(1)),
        )
        .await;
        fixture
            .terminal
            .wait_text(if active { "Enter steer" } else { "actions" })
            .await;
        fixture
    }

    async fn launch_with_history(
        active: bool,
        attach: bool,
        initial_history: Vec<Value>,
        history_gate: Arc<tokio::sync::Semaphore>,
    ) -> Self {
        let cursor = initial_history
            .last()
            .and_then(|event| event["cursor"].as_str())
            .and_then(|cursor| cursor.parse().ok())
            .unwrap_or(0);
        let (connected, mut connections) = mpsc::unbounded_channel();
        let (submitted, submissions) = mpsc::unbounded_channel();
        let (steered, steers) = mpsc::unbounded_channel();
        let (rejected, rejections) = mpsc::unbounded_channel();
        let (cancelled, cancellations) = mpsc::unbounded_channel();
        let history = Arc::new(Mutex::new(initial_history));
        let state_available = Arc::new(AtomicBool::new(true));
        let settings = Arc::new(Mutex::new(
            json!({"model": "gpt-6-astra", "thinking": "low", "reasoning_mode": "standard", "fast_mode": false}),
        ));
        let history_requests = Arc::new(Mutex::new(Vec::new()));
        let session_list_gate = Arc::new(tokio::sync::Semaphore::new(1));
        let listed_agent = Arc::new(Mutex::new(AGENT.to_owned()));
        let resume_gate = Arc::new(tokio::sync::Semaphore::new(1));
        let vault_writes = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/credentials", get(vault_metadata))
            .route("/v1/credentials/vault/login/{id}/origin", put(approve_vault_origin))
            .route("/v1/account/hands/screens", get(|| async { Json(json!({"surfaces": [{"id":"desktop","machine_id":"screen-test-hand","machine_name":"SCREEN_TEST_HAND","name":"Desktop","generation":"screen-generation","width":32,"height":18,"transport":"frames-v1"}]})) }))
            .route("/v1/account/hands/view", get(test_screen_socket))
            .route("/v1/account/hands/renew", post(|| async { Json(json!({"ok":true})) }))
            .route(
                "/v1/agents",
                post(|| async {
                    Json(json!({"agent_id": AGENT, "session_id": AGENT,
                    "events_url": format!("/v1/agents/{AGENT}/events"),
                    "websocket_url": format!("/v1/agents/{AGENT}/ws")}))
                }),
            )
            .route("/v1/agents/live", get(socket))
            .route("/v1/agents", get(list_agents))
            .route("/v1/agents/{agent}", get(state))
            .route("/v1/agents/{agent}/ws", get(socket))
            .route("/v1/agents/{agent}/events/history", get(event_history))
            .route("/v1/agents/{agent}/turns", post(submit))
            .route("/v1/agents/{agent}/turns/{turn}/steer", post(steer))
            .route("/v1/agents/{agent}/turns/{turn}/cancel", post(cancel))
            .with_state(Service {
                vault_writes: vault_writes.clone(),
                listed_agent: listed_agent.clone(),
                resume_gate: resume_gate.clone(),
                active,
                state_available: state_available.clone(),
                settings: settings.clone(),
                history_gate: history_gate.clone(),
                session_list_gate: session_list_gate.clone(),
                history_requests: history_requests.clone(),
                history: history.clone(),
                connected,
                submitted,
                steered,
                rejected,
                cancelled,
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let terminal = Terminal::start(&origin, attach);
        let events = tokio::time::timeout(TIMEOUT, connections.recv())
            .await
            .unwrap()
            .unwrap();
        Self {
            vault_writes,
            listed_agent,
            resume_gate,
            origin,
            terminal,
            state_available,
            settings,
            history_gate,
            session_list_gate,
            history_requests,
            events,
            connections,
            history,
            submissions,
            steers,
            rejections,
            cancellations,
            server,
            cursor,
        }
    }

    fn retain(&mut self, turn: &str, mut value: Value) -> Value {
        self.cursor += 1;
        value["cursor"] = json!(self.cursor.to_string());
        value["turn_id"] = json!(turn);
        self.history.lock().unwrap().push(value.clone());
        value
    }

    fn emit(&mut self, turn: &str, value: Value) {
        let value = self.retain(turn, value);
        self.events.send(value).unwrap();
    }

    fn break_stream(&self) {
        self.events
            .send(json!({"type": "invalid_stream_frame"}))
            .unwrap();
    }

    async fn replacement_connection(&mut self) {
        self.events = tokio::time::timeout(TIMEOUT, self.connections.recv())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "replacement connection missing: {}",
                    self.terminal.screen.lock().unwrap().screen().contents()
                )
            })
            .unwrap();
    }

    async fn reconnect(&mut self) {
        self.events.send(Value::Null).unwrap();
        self.events = tokio::time::timeout(TIMEOUT, self.connections.recv())
            .await
            .unwrap()
            .unwrap();
    }

    fn nested(&mut self, turn: &str, kind: &str, payload: Value) {
        self.emit(
            turn,
            json!({"type": "event", "event": {
                "protocol_version": 1, "request_id": AGENT, "seq": self.cursor + 1,
                "type": kind, "payload": payload
            }}),
        );
    }

    async fn submission(&mut self, expected: &str) -> String {
        let result = tokio::time::timeout(TIMEOUT, self.submissions.recv()).await;
        assert!(
            result.is_ok(),
            "prompt {expected:?} never reached the service"
        );
        let message = result.unwrap().unwrap();
        assert_eq!(prompt_text(&message["input"]), expected);
        let turn = message["id"].as_str().unwrap().to_owned();
        self.emit(&turn, json!({"type": "turn_accepted", "id": turn, "input": message["input"], "replayed": false}));
        turn
    }

    fn complete(&mut self, turn: &str) {
        let final_message = self
            .history
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|event| {
                event["turn_id"] == turn
                    && event["agent_id"].is_null()
                    && event["event"]["type"] == "assistant.message"
                    && event["event"]["payload"]["phase"] == "final_answer"
            })
            .and_then(|event| event["event"]["payload"]["text"].as_str())
            .unwrap_or("done")
            .to_owned();
        self.nested(turn, "run.completed", json!({"status": "completed"}));
        self.emit(
            turn,
            json!({"type": "turn_completed", "id": turn,
            "final_message": final_message, "usage": null, "citations": [], "usage_error": null}),
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[tokio::test]
async fn terminal_id_command_shows_attached_agent_without_sending_input() {
    for pasted in [false, true] {
        let mut fixture = Fixture::start_with_active(true).await;
        if pasted {
            fixture.terminal.prompt("/id", "\r");
        } else {
            fixture.terminal.input("/id\r");
        }
        fixture.terminal.wait_text("Agent ID").await;
        fixture.terminal.wait_text(AGENT).await;
        fixture.terminal.input("\r");
        fixture.terminal.wait_no_text("Agent ID").await;
        fixture.terminal.wait_no_text(AGENT).await;
        let encoded = base64::engine::general_purpose::STANDARD.encode(AGENT);
        let copy = format!("\x1b]52;c;{encoded}");
        assert!(String::from_utf8_lossy(&fixture.terminal.output.lock().unwrap()).contains(&copy));
        fixture.terminal.wait_text("Enter steer").await;
        assert!(fixture.submissions.try_recv().is_err());
        assert!(fixture.steers.try_recv().is_err());
    }
}

#[tokio::test]
async fn terminal_id_command_during_startup_does_not_submit_a_turn() {
    // Startup eagerly connects. Hold history, but allow the identity to arrive
    // before replay finishes: either ID response must remain a local control.
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let mut fixture = Fixture::launch_with_history(false, false, Vec::new(), gate.clone()).await;
    fixture.terminal.wait_text("nanocodex2").await;
    fixture.terminal.prompt("/id", "\r");
    fixture.terminal.wait_text("ID").await;
    let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
    if screen.contains(AGENT) {
        assert!(screen.contains("Agent ID"));
        fixture.terminal.input("\x1b");
        fixture.terminal.wait_no_text("Agent ID").await;
    } else {
        assert!(screen.contains("No agent ID yet"), "{screen}");
    }
    assert!(fixture.submissions.try_recv().is_err());
    gate.add_permits(1);

    fixture.terminal.prompt("create an agent", "\r");
    let turn = fixture.submission("create an agent").await;
    fixture.complete(&turn);
    fixture.terminal.wait_text("done").await;
    fixture.terminal.prompt("/id", "\r");
    fixture.terminal.wait_text("Agent ID").await;
    fixture.terminal.wait_text(AGENT).await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_no_text("Agent ID").await;
    fixture.terminal.wait_no_text(AGENT).await;
    assert!(
        !String::from_utf8_lossy(&fixture.terminal.output.lock().unwrap()).contains("\x1b]52;")
    );
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_can_cancel_a_slow_session_lookup_and_keep_steering() {
    let mut fixture = Fixture::start_with_active(true).await;
    let pause = fixture
        .session_list_gate
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    fixture.terminal.prompt("STEERING ", "");
    fixture.terminal.input("@@");
    fixture.terminal.wait_text("Loading sessions").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_no_text("Loading sessions").await;
    fixture.terminal.input("\x17");
    fixture.terminal.prompt("AFTER_CANCEL", "\r");
    let (steer, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(steer["turn_id"], REMOTE_TURN);
    assert_eq!(prompt_text(&steer["input"]), "STEERING AFTER_CANCEL");
    ack.send(true).unwrap();
    drop(pause);

    fixture.terminal.input("@@");
    fixture.terminal.wait_text("RETAINED_REMOTE_WORK").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_no_text("RETAINED_REMOTE_WORK").await;
    fixture.terminal.input("\x15");
    fixture.terminal.prompt("STEERING_AFTER_FRESH_LOOKUP", "\r");
    let (steer, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(steer["turn_id"], REMOTE_TURN);
    assert_eq!(prompt_text(&steer["input"]), "STEERING_AFTER_FRESH_LOOKUP");
    ack.send(true).unwrap();
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("Enter send").await;
    assert!(fixture.cancellations.try_recv().is_err());
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_releases_ready_followups_after_cancelling_a_session_lookup() {
    let mut fixture = Fixture::start_with_active(true).await;
    let pause = fixture
        .session_list_gate
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    fixture.terminal.prompt("QUEUED_WHILE_LOOKUP_PAUSED", "\t");
    fixture
        .terminal
        .wait_text("queue · enter steer latest")
        .await;
    fixture.terminal.prompt("PRESERVED ", "");
    fixture.terminal.input("@@");
    fixture.terminal.wait_text("Loading sessions").await;
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("done").await;
    fixture.terminal.wait_text("Loading sessions").await;
    fixture.terminal.wait_text("Esc cancel").await;
    fixture.terminal.input("\x1b");
    let next = fixture.submission("QUEUED_WHILE_LOOKUP_PAUSED").await;
    fixture.complete(&next);
    fixture.terminal.wait_text("Enter send").await;
    fixture.terminal.wait_text("PRESERVED @@").await;
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());
    assert!(fixture.cancellations.try_recv().is_err());
    drop(pause);
}

#[tokio::test]
async fn terminal_restores_a_cleared_draft_while_offline_then_steers_it_once() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal.prompt("RESTORE_OFFLINE_短", "");
    fixture.state_available.store(false, Ordering::SeqCst);
    fixture.break_stream();
    fixture.terminal.wait_text("Connection lost").await;
    fixture.terminal.input("\x03");
    fixture.terminal.wait_text("Ctrl+Z to restore").await;
    fixture.terminal.wait_no_text("RESTORE_OFFLINE_短").await;
    fixture.terminal.input("\x1a");
    fixture.terminal.wait_text("Draft restored").await;
    fixture.terminal.wait_text("RESTORE_OFFLINE_短").await;
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());
    fixture.state_available.store(true, Ordering::SeqCst);
    fixture.terminal.input("\r");
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("Reconnected").await;
    fixture.terminal.input("\r");
    let (steer, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(steer["turn_id"], REMOTE_TURN);
    assert_eq!(prompt_text(&steer["input"]), "RESTORE_OFFLINE_短");
    ack.send(true).unwrap();
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("Enter send").await;
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());
}

#[tokio::test]
async fn terminal_late_session_lookup_preserves_a_draft_edited_while_offline() {
    let mut fixture = Fixture::start_with_active(true).await;
    let pause = fixture
        .session_list_gate
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    fixture.terminal.prompt("ORIGINAL_LONG_DRAFT ", "");
    fixture.terminal.input("@@");
    fixture.terminal.wait_text("Loading sessions").await;
    fixture.state_available.store(false, Ordering::SeqCst);
    fixture.break_stream();
    fixture.terminal.wait_text("Connection lost").await;
    fixture.terminal.input("\x15");
    fixture.terminal.prompt("短", "");
    fixture.state_available.store(true, Ordering::SeqCst);
    fixture.terminal.input("\r");
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("Reconnected").await;
    drop(pause);
    fixture.terminal.wait_no_text("Loading sessions").await;
    fixture.terminal.input("\r");
    let result = tokio::time::timeout(TIMEOUT, fixture.steers.recv()).await;
    assert!(
        result.is_ok(),
        "edited draft did not reach steering; terminal output: {}",
        String::from_utf8_lossy(&fixture.terminal.output.lock().unwrap())
            .chars()
            .rev()
            .take(1800)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
    );
    let (steer, ack) = result.unwrap().unwrap();
    assert_eq!(steer["turn_id"], REMOTE_TURN);
    assert_eq!(prompt_text(&steer["input"]), "短");
    ack.send(true).unwrap();
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("Enter send").await;
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_long_session_keeps_steering_queue_edits_and_reconnects_usable() {
    let mut fixture = Fixture::start().await;
    for round in 0..36 {
        let task = format!("LONG_SESSION_TASK_{round:02}");
        fixture.terminal.prompt(&task, "\r");
        let turn = fixture.submission(&task).await;
        for chunk in 0..20 {
            fixture.nested(
                &turn,
                "assistant.delta",
                json!({
                    "model_call_index": 1, "item_id": "progress", "phase": "commentary",
                    "text": format!("step {chunk} ")
                }),
            );
        }

        let queued = format!("LONG_SESSION_FOLLOWUP_{round:02}");
        fixture.terminal.prompt(&queued, "\t");
        fixture
            .terminal
            .wait_text("queue · enter steer latest")
            .await;
        fixture.terminal.input("\t");
        fixture.terminal.wait_text("e edit").await;
        fixture.terminal.input("e");
        fixture.terminal.wait_text("editing queued message").await;
        fixture
            .terminal
            .prompt("_EDITED", if round % 2 == 0 { "\r" } else { "\x1b" });
        fixture
            .terminal
            .wait_no_text("editing queued message")
            .await;
        fixture.terminal.wait_text("e edit").await;
        fixture.terminal.input("\t");
        // Observe the focus change before disconnecting: offline input handling
        // intentionally ignores Tab, so a queued key can otherwise lose the race.
        fixture.terminal.wait_no_text("e edit").await;

        let instruction = format!("LONG_SESSION_STEER_{round:02}");
        if round % 12 == 11 {
            fixture.state_available.store(false, Ordering::SeqCst);
            fixture.break_stream();
            fixture.terminal.wait_text("Connection lost").await;
            fixture.terminal.prompt(&instruction, "");
            fixture.state_available.store(true, Ordering::SeqCst);
            fixture.terminal.input("\r");
            fixture.replacement_connection().await;
            fixture.terminal.wait_text("Reconnected").await;
            fixture.terminal.input("\r");
        } else {
            fixture.terminal.prompt(&instruction, "\r");
        }
        let (steer, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(steer["turn_id"], turn);
        assert_eq!(prompt_text(&steer["input"]), instruction);
        // Alternate acknowledgement order across the terminal boundary.
        if round % 2 == 0 {
            ack.send(true).unwrap();
            fixture.complete(&turn);
        } else {
            fixture.complete(&turn);
            ack.send(true).unwrap();
        }
        let expected = if round % 2 == 0 {
            format!("{queued}_EDITED")
        } else {
            queued
        };
        let followup = fixture.submission(&expected).await;
        fixture.complete(&followup);
        fixture.terminal.wait_text("Enter send").await;
        assert!(
            fixture.submissions.try_recv().is_err(),
            "duplicate submission in round {round}"
        );
        assert!(
            fixture.steers.try_recv().is_err(),
            "duplicate steering in round {round}"
        );
    }
    assert!(fixture.history.lock().unwrap().len() > 704);
}

#[tokio::test]
async fn terminal_steering_and_queued_followup_complete_without_duplicate_submission() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("long running task", "\r");
    let turn = fixture.submission("long running task").await;
    fixture.terminal.prompt("change direction", "\r");
    let (input, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prompt_text(&input["input"]), "change direction");
    // Durable application can arrive before the HTTP acknowledgement.
    fixture.nested(
        &turn,
        "run.steered",
        json!({"steer_index": 1, "instruction_bytes": 16}),
    );
    ack.send(true).unwrap();
    fixture.terminal.prompt("queued followup", "\t");
    fixture
        .terminal
        .wait_text("queue · enter steer latest")
        .await;
    fixture.complete(&turn);
    let followup = fixture.submission("queued followup").await;
    fixture.complete(&followup);
    fixture.terminal.prompt("still responsive", "\r");
    let last = fixture.submission("still responsive").await;
    fixture.complete(&last);
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_attached_to_active_turn_submits_queued_input_when_remote_turn_finishes() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal.prompt("remote followup", "\t");
    fixture
        .terminal
        .wait_text("queue · enter steer latest")
        .await;
    fixture.complete(REMOTE_TURN);
    let turn = fixture.submission("remote followup").await;
    fixture.complete(&turn);
}

#[tokio::test]
async fn terminal_does_not_retry_accepted_steer_with_ack_after_terminal() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("long running task", "\r");
    let turn = fixture.submission("long running task").await;
    fixture.terminal.prompt("late acknowledgement", "\r");
    let (_, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    fixture.terminal.prompt("only the queued followup", "\t");
    fixture.terminal.wait_text("only the queued followup").await;
    fixture.complete(&turn);
    ack.send(true).unwrap();
    let next = fixture.submission("only the queued followup").await;
    fixture.complete(&next);
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_displays_restore_failure_without_nested_run_events_and_after_reconnect() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("hi hi", "\r");
    let turn = fixture.submission("hi hi").await;
    fixture.emit(
        &turn,
        json!({"type": "turn_failed", "id": turn,
        "error": "durability state cannot be restored"}),
    );
    fixture
        .terminal
        .wait_text("durability state cannot be restored")
        .await;
    fixture.reconnect().await;
    fixture.terminal.prompt("next prompt", "\r");
    let next = fixture.submission("next prompt").await;
    fixture.complete(&next);
}

#[tokio::test]
async fn terminal_durable_failure_without_nested_terminal_releases_queued_input() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("long running task", "\r");
    let turn = fixture.submission("long running task").await;
    fixture.terminal.prompt("recover after failure", "\t");
    fixture
        .terminal
        .wait_text("queue · enter steer latest")
        .await;
    fixture.emit(&turn, json!({"type": "turn_failed", "id": turn, "error": "runtime failed before publishing its nested terminal"}));
    let recovered = fixture.submission("recover after failure").await;
    fixture.complete(&recovered);
}

#[tokio::test]
async fn terminal_does_not_retry_accepted_steering_after_a_run_failure() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("long running task", "\r");
    let turn = fixture.submission("long running task").await;
    fixture.terminal.prompt("accepted instruction", "\r");
    let (_, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    ack.send(true).unwrap();
    fixture.terminal.wait_text("steering accepted").await;
    fixture.terminal.prompt("known unsent followup", "\t");
    fixture.terminal.wait_text("known unsent followup").await;
    fixture.nested(&turn, "run.failed", json!({"status": "failed"}));
    fixture.emit(&turn, json!({"type": "turn_failed", "id": turn, "error": "test tool failed before steering boundary"}));
    let next = fixture.submission("known unsent followup").await;
    fixture.complete(&next);
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_reconnects_before_admission_without_creating_another_turn() {
    let mut fixture = Fixture::start().await;
    fixture
        .terminal
        .prompt("survive lost acknowledgement", "\r");
    let original = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
        .await
        .unwrap()
        .unwrap();
    fixture.reconnect().await;
    let replayed = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        replayed, original,
        "reconnect must reuse the durable prompt ID"
    );
    let turn = original["id"].as_str().unwrap();
    fixture.emit(
        turn,
        json!({"type": "turn_accepted", "id": turn, "input": original["input"], "replayed": true}),
    );
    fixture.complete(turn);
    fixture.terminal.prompt("work after reconnect", "\t");
    let next = fixture.submission("work after reconnect").await;
    fixture.complete(&next);
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_preserves_steering_and_queue_across_repeated_connection_drops() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("long running task", "\r");
    let turn = fixture.submission("long running task").await;
    for index in 1..=3 {
        fixture.reconnect().await;
        let instruction = format!("change direction {index}");
        fixture.terminal.prompt(&instruction, "\r");
        let (input, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(prompt_text(&input["input"]), instruction);
        fixture.nested(
            &turn,
            "run.steered",
            json!({"steer_index": index, "instruction_bytes": instruction.len()}),
        );
        ack.send(true).unwrap();
    }
    fixture
        .terminal
        .prompt("followup after repeated drops", "\t");
    fixture
        .terminal
        .wait_text("queue · enter steer latest")
        .await;
    fixture.reconnect().await;
    fixture.complete(&turn);
    let followup = fixture.submission("followup after repeated drops").await;
    fixture.complete(&followup);
    assert!(fixture.submissions.try_recv().is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_preserves_shell_waiting_prompts_and_followups_across_reconnect() {
    for finishes_offline in [false, true] {
        let mut fixture = Fixture::start().await;
        let command = r#"/bin/sh -c 'i=0; while [ ! -e release-shell ] && [ "$i" -lt 500 ]; do sleep 0.02; i=$((i+1)); done; printf "SHELL_%s\n" FINISHED; : > shell-finished'"#;
        fixture.terminal.prompt(&format!("!{command}"), "\r");
        fixture.terminal.wait_text("Shell").await;
        fixture.terminal.prompt("USE_THE_SHELL_RESULT", "\r");
        fixture
            .terminal
            .prompt("FOLLOWUP_AFTER_SHELL_RECOVERY", "\t");
        fixture
            .terminal
            .wait_text("queue · enter steer latest")
            .await;
        let early = fixture.submissions.try_recv();
        assert!(
            early.is_err(),
            "the prompt must wait for its shell context: {early:?}"
        );
        fixture.state_available.store(false, Ordering::SeqCst);
        fixture.break_stream();
        fixture.terminal.wait_text("Connection lost").await;
        let release = fixture.terminal._workspace.path().join("release-shell");
        if finishes_offline {
            std::fs::write(&release, "").unwrap();
            let finished = fixture.terminal._workspace.path().join("shell-finished");
            tokio::time::timeout(TIMEOUT, async {
                while !finished.exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
        fixture.state_available.store(true, Ordering::SeqCst);
        fixture.terminal.input("\r");
        fixture.replacement_connection().await;
        fixture.terminal.wait_text("Reconnected").await;
        if !finishes_offline {
            assert!(
                fixture.submissions.try_recv().is_err(),
                "reconnect must keep waiting for the running shell"
            );
            std::fs::write(&release, "").unwrap();
        }
        let expected = format!(
            "<local_shell_result>\ncommand: {command}\noutcome: exit 0\noutput:\nSHELL_FINISHED\n\n</local_shell_result>\n\nUSE_THE_SHELL_RESULT"
        );
        let turn = fixture.submission(&expected).await;
        fixture.complete(&turn);
        let followup = fixture.submission("FOLLOWUP_AFTER_SHELL_RECOVERY").await;
        fixture.complete(&followup);
        fixture.terminal.wait_text("Enter send").await;
        assert!(fixture.submissions.try_recv().is_err());
        assert!(fixture.steers.try_recv().is_err());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_interrupts_a_local_shell_and_accepts_the_next_prompt() {
    for disconnected in [false, true] {
        let mut fixture = Fixture::start().await;
        fixture.terminal.prompt("!sleep 30", "\r");
        fixture.terminal.wait_text("Shell").await;
        if disconnected {
            fixture.state_available.store(false, Ordering::SeqCst);
            fixture.break_stream();
            fixture.terminal.wait_text("Connection lost").await;
        }
        fixture.terminal.input("\x1b");
        fixture.terminal.wait_text("Interrupt").await;
        fixture.terminal.input("\x1b");
        fixture.terminal.wait_text("cancelled by user").await;
        if disconnected {
            fixture.state_available.store(true, Ordering::SeqCst);
            fixture.terminal.input("\r");
            fixture.replacement_connection().await;
            fixture.terminal.wait_text("Reconnected").await;
        }
        fixture
            .terminal
            .prompt("work after shell cancellation", "\r");
        let next = fixture.submission("<local_shell_result>\ncommand: sleep 30\noutcome: cancelled by user\noutput:\n\n</local_shell_result>\n\nwork after shell cancellation").await;
        fixture.complete(&next);
    }
}

#[tokio::test]
async fn terminal_delivers_rapid_attached_steers_in_order() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal.prompt("first instruction", "\r");
    let (first, first_ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prompt_text(&first["input"]), "first instruction");
    fixture.terminal.prompt("second instruction", "\r");
    assert!(
        tokio::time::timeout(Duration::from_millis(300), fixture.steers.recv())
            .await
            .is_err(),
        "later steering must wait until the earlier instruction is acknowledged"
    );
    fixture.nested(
        REMOTE_TURN,
        "run.steered",
        json!({"steer_index": 1, "instruction_bytes": 17}),
    );
    first_ack.send(true).unwrap();
    let (second, second_ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prompt_text(&second["input"]), "second instruction");
    fixture.nested(
        REMOTE_TURN,
        "run.steered",
        json!({"steer_index": 2, "instruction_bytes": 18}),
    );
    second_ack.send(true).unwrap();
    fixture
        .terminal
        .prompt("followup after rapid steering", "\t");
    fixture
        .terminal
        .wait_text("queue · enter steer latest")
        .await;
    fixture.complete(REMOTE_TURN);
    let next = fixture.submission("followup after rapid steering").await;
    fixture.complete(&next);
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_cancels_during_a_steer_ack_without_repeating_applied_input() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal.prompt("already applied instruction", "\r");
    let (_, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    fixture.nested(
        REMOTE_TURN,
        "run.steered",
        json!({"steer_index": 1, "instruction_bytes": 27}),
    );
    fixture.terminal.prompt("instruction still waiting", "\r");
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_text("Interrupt").await;
    fixture.terminal.input("\x1b");
    assert_eq!(
        tokio::time::timeout(TIMEOUT, fixture.cancellations.recv())
            .await
            .unwrap()
            .unwrap(),
        REMOTE_TURN
    );
    fixture.terminal.wait_text("Interrupted response").await;
    fixture.emit(
        REMOTE_TURN,
        json!({"type": "turn_cancelled", "id": REMOTE_TURN}),
    );
    ack.send(true).unwrap();
    let next = fixture.submission("instruction still waiting").await;
    fixture.complete(&next);
    assert!(
        fixture.steers.try_recv().is_err(),
        "cancelled pending steering must not be sent to the old turn"
    );
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_recovers_multiple_waiting_steers_in_order_when_the_turn_ends() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal.prompt("first unapplied instruction", "\r");
    let (_, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    fixture
        .terminal
        .prompt("second unapplied instruction", "\r");
    fixture.terminal.prompt("third unapplied instruction", "\r");
    fixture.terminal.prompt("regular followup", "\t");
    fixture.terminal.wait_text("regular followup").await;
    // Retain completion before notifying the observer so both waiting requests
    // deterministically race with completion and receive real HTTP rejections.
    fixture.cursor += 1;
    let terminal = json!({
        "cursor": fixture.cursor.to_string(), "turn_id": REMOTE_TURN,
        "type": "turn_completed", "id": REMOTE_TURN,
        "final_message": "done", "usage": null, "citations": [], "usage_error": null,
    });
    fixture.history.lock().unwrap().push(terminal.clone());
    ack.send(true).unwrap();
    for expected in [
        "second unapplied instruction",
        "third unapplied instruction",
    ] {
        let rejected = tokio::time::timeout(TIMEOUT, fixture.rejections.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(prompt_text(&rejected["input"]), expected);
    }
    fixture.events.send(terminal).unwrap();
    let next = fixture
        .submission(
            "second unapplied instruction\n\nthird unapplied instruction\n\nregular followup",
        )
        .await;
    fixture.complete(&next);
    assert!(fixture.steers.try_recv().is_err());
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_drains_a_burst_of_steering_sent_before_prompt_admission() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("long running task", "\r");
    let original = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
        .await
        .unwrap()
        .unwrap();
    let turn = original["id"].as_str().unwrap();
    let instructions = (1..=12)
        .map(|index| {
            if index == 12 {
                "TWELFTH_MESSAGE".to_owned()
            } else {
                format!("burst instruction {index:02}")
            }
        })
        .collect::<Vec<_>>();
    for instruction in &instructions {
        fixture.terminal.prompt(instruction, "\r");
    }
    fixture.terminal.wait_text("TWELFTH_MESSAGE").await;
    fixture.emit(
        turn,
        json!({"type": "turn_accepted", "id": turn, "input": original["input"], "replayed": false}),
    );
    for (index, instruction) in instructions.iter().enumerate() {
        let (input, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(prompt_text(&input["input"]), *instruction);
        if index % 2 == 0 {
            fixture.nested(
                turn,
                "run.steered",
                json!({"steer_index": index + 1, "instruction_bytes": instruction.len()}),
            );
            ack.send(true).unwrap();
        } else {
            ack.send(true).unwrap();
            fixture.nested(
                turn,
                "run.steered",
                json!({"steer_index": index + 1, "instruction_bytes": instruction.len()}),
            );
        }
    }
    fixture.complete(turn);
    fixture
        .terminal
        .prompt("followup after twelve steers", "\t");
    let next = fixture.submission("followup after twelve steers").await;
    fixture.complete(&next);
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_failed_ack_does_not_repeat_an_applied_steer() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture
        .terminal
        .prompt("applied despite failed acknowledgement", "\r");
    let (_, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    fixture.nested(
        REMOTE_TURN,
        "run.steered",
        json!({"steer_index": 1, "instruction_bytes": 38}),
    );
    ack.send(false).unwrap();
    fixture
        .terminal
        .prompt("followup after failed acknowledgement", "\t");
    fixture
        .terminal
        .wait_text("followup after failed acknowledgement")
        .await;
    fixture.complete(REMOTE_TURN);
    let next = fixture
        .submission("followup after failed acknowledgement")
        .await;
    fixture.complete(&next);
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_foreign_steering_cannot_confirm_pending_or_uncertain_local_input() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal.prompt("my uncertain instruction", "\r");
    let (_, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    // A second client steers the same durable turn before our HTTP reply arrives.
    fixture.nested(
        REMOTE_TURN,
        "run.steered",
        json!({"steer_index": 1, "instruction_bytes": 24}),
    );
    fixture
        .terminal
        .prompt("ONLY_UNSENT_AFTER_FOREIGN_EVENT", "\r");
    fixture
        .terminal
        .wait_text("ONLY_UNSENT_AFTER_FOREIGN_EVENT")
        .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), fixture.steers.recv())
            .await
            .is_err()
    );
    ack.send(false).unwrap();
    fixture
        .terminal
        .wait_text("Could not confirm steering")
        .await;
    // Resizing also checks that uncertain input survives a full terminal redraw.
    // Raw ANSI diffs can otherwise split this label across cursor movements.
    fixture.terminal.resize(161);
    fixture.terminal.wait_text("[delivery unknown]").await;
    // Identical text lengths and more foreign application telemetry still prove nothing
    // about our failed acknowledgement. They must not release the next HTTP request.
    fixture.nested(
        REMOTE_TURN,
        "run.steered",
        json!({"steer_index": 2, "instruction_bytes": 24}),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(300), fixture.steers.recv())
            .await
            .is_err()
    );
    fixture.complete(REMOTE_TURN);
    let next = fixture.submission("ONLY_UNSENT_AFTER_FOREIGN_EVENT").await;
    fixture.complete(&next);
    fixture.terminal.prompt("another safe followup", "\r");
    let next = fixture.submission("another safe followup").await;
    fixture.complete(&next);
    assert!(
        fixture.submissions.try_recv().is_err(),
        "uncertain input must never be retried automatically"
    );
}

#[tokio::test]
async fn terminal_retains_unconfirmed_steering_and_sends_only_unsent_followups() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal.prompt("unconfirmed instruction", "\r");
    let (_, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    ack.send(false).unwrap();
    fixture
        .terminal
        .wait_text("Could not confirm steering")
        .await;
    fixture.terminal.prompt("waiting instruction", "\r");
    fixture.terminal.wait_text("waiting instruction").await;
    fixture.complete(REMOTE_TURN);
    let next = fixture.submission("waiting instruction").await;
    fixture.complete(&next);
    fixture.terminal.prompt("another turn remains usable", "\r");
    let next = fixture.submission("another turn remains usable").await;
    fixture.complete(&next);
    assert!(fixture.steers.try_recv().is_err());
}

#[tokio::test]
async fn terminal_preserves_applied_steer_with_failed_ack_after_local_turn_finishes() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("local task", "\r");
    let turn = fixture.submission("local task").await;
    fixture.terminal.prompt("already handled locally", "\r");
    let (_, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    fixture.nested(
        &turn,
        "run.steered",
        json!({"steer_index": 1, "instruction_bytes": 23}),
    );
    fixture.terminal.prompt("only this followup", "\t");
    fixture.terminal.wait_text("only this followup").await;
    fixture.complete(&turn);
    ack.send(false).unwrap();
    let next = fixture.submission("only this followup").await;
    fixture.complete(&next);
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_cancellation_remains_usable_with_unknown_steering_delivery() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal.prompt("uncertain then applied", "\r");
    let (_, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    ack.send(false).unwrap();
    fixture
        .terminal
        .wait_text("Could not confirm steering")
        .await;
    fixture
        .terminal
        .prompt("waiting through cancellation", "\r");
    fixture
        .terminal
        .wait_text("waiting through cancellation")
        .await;
    fixture.terminal.input("\x1b");
    tokio::time::sleep(Duration::from_millis(100)).await;
    fixture.terminal.input("\x1b");
    let cancelled = tokio::time::timeout(TIMEOUT, fixture.cancellations.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled, REMOTE_TURN);
    fixture.terminal.wait_text("Interrupted response").await;
    fixture.nested(
        REMOTE_TURN,
        "run.steered",
        json!({"steer_index": 1, "instruction_bytes": 22}),
    );
    fixture.emit(
        REMOTE_TURN,
        json!({"type": "turn_cancelled", "id": REMOTE_TURN}),
    );
    let next = fixture.submission("waiting through cancellation").await;
    fixture.complete(&next);
    assert!(fixture.steers.try_recv().is_err());
}

#[tokio::test]
async fn terminal_cancelled_queue_edit_does_not_reappear_through_history() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal.prompt("ORIGINAL_QUEUED_INPUT", "\t");
    fixture
        .terminal
        .wait_text("queue · enter steer latest")
        .await;
    fixture.terminal.input("\t");
    fixture.terminal.wait_text("e edit").await;
    fixture.terminal.input("e");
    fixture.terminal.wait_text("editing queued message").await;
    fixture.terminal.input("\x15");
    fixture.terminal.prompt("ABANDONED_REVISION", "\x1b[A");
    fixture.terminal.input("\x1b");
    fixture
        .terminal
        .wait_no_text("editing queued message")
        .await;
    fixture.terminal.input("\t\x1b[B");
    fixture.terminal.prompt("FRESH_STEERING_ONLY", "\r");
    let (steer, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prompt_text(&steer["input"]), "FRESH_STEERING_ONLY");
    ack.send(true).unwrap();
    fixture.terminal.wait_text("steering accepted").await;
    fixture.complete(REMOTE_TURN);
    let next = fixture.submission("ORIGINAL_QUEUED_INPUT").await;
    fixture.complete(&next);
    assert!(fixture.steers.try_recv().is_err());
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_command_enter_starts_reflection_and_returns_to_normal_chat() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.input("/reflection");
    fixture.terminal.wait_text("Reflect on session").await;
    fixture.terminal.input("\r");
    fixture.terminal.wait_text("Reflection instructions").await;
    fixture
        .terminal
        .prompt("REFLECT_THIS_SESSION", "\x1b[13;9u");
    let turn = fixture.submission("Reflect on this managed conversation and return a concise, actionable report.\n\nREFLECT_THIS_SESSION").await;
    fixture
        .terminal
        .wait_no_text("Reflection instructions")
        .await;
    fixture.complete(&turn);
    fixture.terminal.wait_text("actions").await;
    fixture.terminal.prompt("NORMAL_AFTER_REFLECTION", "\r");
    let next = fixture.submission("NORMAL_AFTER_REFLECTION").await;
    fixture.complete(&next);
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_cancels_a_queue_edit_while_offline_and_preserves_both_original_inputs() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal.prompt("ORIGINAL_QUEUED_MESSAGE", "\t");
    fixture
        .terminal
        .wait_text("queue · enter steer latest")
        .await;
    fixture.terminal.prompt("PRESERVED_COMPOSER_DRAFT", "");
    // Shift+Tab focuses the queue without queuing the current draft.
    fixture.terminal.input("\x1b[Z");
    fixture.terminal.wait_text("e edit").await;
    fixture.terminal.input("e");
    fixture.terminal.wait_text("editing queued message").await;
    fixture.terminal.input("\x15");
    fixture.terminal.prompt("UNSAVED_OFFLINE_REVISION", "");
    fixture.state_available.store(false, Ordering::SeqCst);
    fixture.break_stream();
    fixture.terminal.wait_text("Connection lost").await;
    fixture.terminal.input("\x1b");
    fixture
        .terminal
        .wait_no_text("editing queued message")
        .await;
    fixture.terminal.wait_text("PRESERVED_COMPOSER_DRAFT").await;
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());
    fixture.state_available.store(true, Ordering::SeqCst);
    fixture.terminal.input("\r");
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("Reconnected").await;
    fixture.complete(REMOTE_TURN);
    let next = fixture.submission("ORIGINAL_QUEUED_MESSAGE").await;
    fixture.complete(&next);
    fixture.terminal.wait_text("Enter send").await;
    fixture.terminal.input("\r");
    let next = fixture.submission("PRESERVED_COMPOSER_DRAFT").await;
    fixture.complete(&next);
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_tab_in_queue_editor_does_not_submit_a_cancelled_revision() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal.prompt("original queued instruction", "\t");
    fixture
        .terminal
        .wait_text("queue · enter steer latest")
        .await;
    fixture.terminal.input("\t");
    fixture.terminal.wait_text("e edit").await;
    fixture.terminal.input("e");
    fixture.terminal.wait_text("editing queued message").await;
    fixture.terminal.input("\x15");
    fixture.terminal.prompt("UNSAVED_QUEUE_REVISION", "\t");
    fixture.terminal.input("\x1b");
    fixture
        .terminal
        .wait_no_text("editing queued message")
        .await;
    fixture.complete(REMOTE_TURN);
    let next = fixture.submission("original queued instruction").await;
    fixture.complete(&next);
    assert!(fixture.steers.try_recv().is_err());
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_requires_explicit_edit_and_save_to_retry_unknown_delivery() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture
        .terminal
        .prompt("instruction for explicit retry", "\r");
    let (_, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    ack.send(false).unwrap();
    fixture.terminal.wait_text("[delivery unknown]").await;
    fixture.complete(REMOTE_TURN);
    fixture.terminal.input("\t");
    fixture.terminal.wait_text("e edit/retry").await;
    fixture.terminal.input("\r");
    assert!(
        tokio::time::timeout(Duration::from_millis(300), fixture.submissions.recv())
            .await
            .is_err()
    );
    fixture.terminal.input("e");
    fixture.terminal.wait_text("editing queued message").await;
    fixture.terminal.input("\x1b");
    assert!(
        tokio::time::timeout(Duration::from_millis(300), fixture.submissions.recv())
            .await
            .is_err()
    );
    fixture.terminal.input("e\r");
    let next = fixture.submission("instruction for explicit retry").await;
    fixture.complete(&next);
    assert!(fixture.steers.try_recv().is_err());
}

#[tokio::test]
async fn terminal_recovers_from_a_fatal_stream_error_without_losing_queue_or_draft() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture
        .terminal
        .prompt("followup after fatal stream error", "\t");
    fixture
        .terminal
        .wait_text("followup after fatal stream error")
        .await;
    fixture.terminal.prompt("DRAFT_SURVIVES_RECOVERY", "");
    fixture.terminal.wait_text("DRAFT_SURVIVES_RECOVERY").await;
    fixture
        .events
        .send(json!({"type": "invalid_stream_frame"}))
        .unwrap();
    fixture.events = tokio::time::timeout(TIMEOUT, fixture.connections.recv())
        .await
        .unwrap_or_else(|_| {
            panic!(
                "terminal must replace the stopped connection: {}",
                fixture.terminal.screen.lock().unwrap().screen().contents()
            )
        })
        .unwrap();
    fixture.terminal.wait_text("Reconnected").await;
    fixture.complete(REMOTE_TURN);
    let next = fixture
        .submission("followup after fatal stream error")
        .await;
    fixture.complete(&next);
    fixture.terminal.input("\r");
    let next = fixture.submission("DRAFT_SURVIVES_RECOVERY").await;
    fixture.complete(&next);
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_recovers_local_activity_and_controls_after_a_fatal_disconnect() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("local task survives", "\r");
    let turn = fixture.submission("local task survives").await;
    fixture
        .terminal
        .prompt("FOLLOWUP_AFTER_LOCAL_RECOVERY", "\t");
    fixture
        .terminal
        .wait_text("FOLLOWUP_AFTER_LOCAL_RECOVERY")
        .await;
    fixture.break_stream();
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("Reconnected").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), fixture.submissions.recv())
            .await
            .is_err()
    );
    fixture.terminal.prompt("steer recovered local turn", "\r");
    let (input, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prompt_text(&input["input"]), "steer recovered local turn");
    ack.send(true).unwrap();
    fixture.complete(&turn);
    let next = fixture.submission("FOLLOWUP_AFTER_LOCAL_RECOVERY").await;
    fixture.complete(&next);
    assert!(fixture.cancellations.try_recv().is_err());
}

#[tokio::test]
async fn terminal_can_edit_its_draft_and_retry_a_failed_reconnection() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.state_available.store(false, Ordering::SeqCst);
    fixture.break_stream();
    fixture.terminal.wait_text("Connection lost").await;
    fixture.terminal.prompt("DRAFT_TYPED_WHILE_OFFLINE", "");
    fixture
        .terminal
        .wait_text("DRAFT_TYPED_WHILE_OFFLINE")
        .await;
    fixture.settings.lock().unwrap()["thinking"] = json!("high");
    fixture.state_available.store(true, Ordering::SeqCst);
    fixture.terminal.input("\r");
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("Reconnected").await;
    fixture.terminal.wait_text("high").await;
    fixture.terminal.wait_text("Thinking").await;
    assert!(fixture.submissions.try_recv().is_err());
    fixture.complete(REMOTE_TURN);
    fixture.terminal.input("\r");
    let next = fixture.submission("DRAFT_TYPED_WHILE_OFFLINE").await;
    fixture.complete(&next);
}

#[tokio::test]
async fn terminal_stops_repeated_fatal_reconnects_until_the_user_retries() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal.prompt("AFTER_REPEATED_FAILURE", "\t");
    fixture.terminal.wait_text("AFTER_REPEATED_FAILURE").await;
    fixture.break_stream();
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("Reconnected").await;
    fixture.break_stream();
    fixture.terminal.wait_text("Connection lost").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(250), fixture.connections.recv())
            .await
            .is_err(),
        "a repeatedly failing connection must not spin in automatic retries"
    );
    fixture.terminal.input("\r");
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("Reconnected").await;
    fixture.complete(REMOTE_TURN);
    let next = fixture.submission("AFTER_REPEATED_FAILURE").await;
    fixture.complete(&next);
}

#[tokio::test]
async fn terminal_catches_up_paginated_history_and_live_completion_after_lost_admission() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("original durable request", "\r");
    let request = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
        .await
        .unwrap()
        .unwrap();
    let turn = request["id"].as_str().unwrap().to_owned();
    fixture.terminal.prompt("ONLY_FOLLOWUP_AFTER_CATCHUP", "\t");
    fixture
        .terminal
        .wait_text("ONLY_FOLLOWUP_AFTER_CATCHUP")
        .await;
    let initial_history_requests = fixture.history_requests.lock().unwrap().len();
    let pause = fixture.history_gate.clone().acquire_owned().await.unwrap();
    fixture.retain(
        &turn,
        json!({"type": "turn_accepted", "id": turn, "input": request["input"], "replayed": false}),
    );
    for index in 1..=300 {
        fixture.retain(
            &turn,
            json!({"type": "event", "event": {
                "protocol_version": 1, "request_id": AGENT, "seq": index,
                "type": "run.steered", "payload": {"steer_index": index, "instruction_bytes": 4}
            }}),
        );
    }
    fixture.break_stream();
    fixture.replacement_connection().await;
    fixture.terminal.prompt("DRAFT_DURING_CATCHUP", "");
    fixture.terminal.wait_text("DRAFT_DURING_CATCHUP").await;
    fixture.complete(&turn);
    drop(pause);
    fixture.terminal.wait_text("Reconnected").await;
    let next = fixture.submission("ONLY_FOLLOWUP_AFTER_CATCHUP").await;
    fixture.complete(&next);
    fixture.terminal.input("\r");
    let next = fixture.submission("DRAFT_DURING_CATCHUP").await;
    fixture.complete(&next);
    assert!(
        fixture.history_requests.lock().unwrap().len() >= initial_history_requests + 2,
        "catch-up must fetch beyond its first history page"
    );
    assert!(
        !fixture
            .terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .contains("[delivery unknown]")
    );
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_keeps_an_unacknowledged_prompt_available_after_reconnecting() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("UNACKNOWLEDGED_ORIGINAL", "\r");
    let _request = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
        .await
        .unwrap()
        .unwrap();
    fixture.terminal.prompt("KNOWN_UNSENT_FOLLOWUP", "\t");
    fixture.terminal.wait_text("KNOWN_UNSENT_FOLLOWUP").await;
    fixture.break_stream();
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("Reconnected").await;
    let next = fixture.submission("KNOWN_UNSENT_FOLLOWUP").await;
    fixture.complete(&next);
    fixture
        .terminal
        .wait_text("[delivery unknown] UNACKNOWLEDGED_ORIGINAL")
        .await;
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_keeps_uncertain_steering_ordered_across_a_replacement_connection() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal.prompt("UNCERTAIN_AT_DISCONNECT", "\r");
    let (_, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    fixture.terminal.prompt("BEFORE_FAILURE", "\r");
    fixture.terminal.wait_text("BEFORE_FAILURE").await;
    fixture.break_stream();
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("Reconnected").await;
    fixture.terminal.prompt("AFTER_RECOVERY", "\r");
    fixture.terminal.wait_text("AFTER_RECOVERY").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), fixture.steers.recv())
            .await
            .is_err()
    );
    let _ = ack.send(true);
    fixture.complete(REMOTE_TURN);
    let next = fixture.submission("BEFORE_FAILURE\n\nAFTER_RECOVERY").await;
    fixture.complete(&next);
    fixture
        .terminal
        .wait_text("[delivery unknown] UNCERTAIN_AT_DISCONNECT")
        .await;
    assert!(fixture.steers.try_recv().is_err());
}

#[tokio::test]
async fn terminal_late_admission_after_recovery_does_not_duplicate_the_original_prompt() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("ORIGINAL_SHOWN_ONCE", "\r");
    let request = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
        .await
        .unwrap()
        .unwrap();
    let turn = request["id"].as_str().unwrap().to_owned();
    fixture.break_stream();
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("Reconnected").await;
    fixture
        .terminal
        .wait_text("[delivery unknown] ORIGINAL_SHOWN_ONCE")
        .await;
    fixture.emit(
        &turn,
        json!({"type": "turn_accepted", "id": turn, "input": request["input"], "replayed": false}),
    );
    fixture.nested(&turn, "assistant.message", json!({"model_call_index": 0, "item_id": "late-result", "phase": "final_answer", "text": "LATE_RECEIPT_PROCESSED"}));
    fixture.complete(&turn);
    fixture.terminal.wait_text("LATE_RECEIPT_PROCESSED").await;
    let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
    assert!(!screen.contains("[delivery unknown]"), "{screen}");
    assert_eq!(screen.matches("ORIGINAL_SHOWN_ONCE").count(), 1, "{screen}");
    fixture
        .terminal
        .prompt("NEXT_PROMPT_AFTER_LATE_RECEIPT", "\r");
    let next = fixture.submission("NEXT_PROMPT_AFTER_LATE_RECEIPT").await;
    fixture.complete(&next);
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_preserves_the_session_when_one_live_update_cannot_be_decoded() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture
        .terminal
        .prompt("FOLLOWUP_AFTER_INVALID_UPDATE", "\t");
    fixture
        .terminal
        .wait_text("FOLLOWUP_AFTER_INVALID_UPDATE")
        .await;
    fixture
        .terminal
        .prompt("PRESERVED_DRAFT_AFTER_INVALID_UPDATE", "");
    fixture.nested(REMOTE_TURN, "unrecognized.session.update", json!({}));
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("Reconnected").await;
    fixture
        .terminal
        .wait_text("Could not display session update")
        .await;
    fixture.complete(REMOTE_TURN);
    let next = fixture.submission("FOLLOWUP_AFTER_INVALID_UPDATE").await;
    fixture.complete(&next);
    fixture.terminal.input("\r");
    let next = fixture
        .submission("PRESERVED_DRAFT_AFTER_INVALID_UPDATE")
        .await;
    fixture.complete(&next);
}

#[tokio::test]
async fn terminal_recent_prompt_picker_preserves_active_status_and_draft() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("WORK_REMAINS_ACTIVE", "\r");
    let turn = fixture.submission("WORK_REMAINS_ACTIVE").await;
    fixture.terminal.wait_text("Thinking").await;
    fixture.terminal.prompt("DRAFT_THROUGH_PROMPT_PICKER", "");
    fixture.terminal.input("\x12");
    fixture.terminal.wait_text("Recent prompts").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_no_text("Recent prompts").await;
    fixture
        .terminal
        .wait_text("DRAFT_THROUGH_PROMPT_PICKER")
        .await;
    fixture.terminal.wait_text("Thinking").await;
    fixture.nested(&turn, "model.warmup.started", json!({}));
    fixture.terminal.wait_text("Warming model").await;
    fixture.terminal.input("\x12");
    fixture.terminal.wait_text("Recent prompts").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_no_text("Recent prompts").await;
    fixture.terminal.wait_text("Warming model").await;
    fixture.terminal.input("\r");
    let (steer, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prompt_text(&steer["input"]), "DRAFT_THROUGH_PROMPT_PICKER");
    ack.send(true).unwrap();
    fixture.complete(&turn);
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_streams_each_chunk_before_completion_without_duplicate_answers() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("stream an answer", "\r");
    let turn = fixture.submission("stream an answer").await;
    let payload = |item: &str, phase: &str, text: &str| json!({"model_call_index": 1, "item_id": item, "phase": phase, "text": text});

    fixture.nested(
        &turn,
        "assistant.delta",
        payload("comment", "commentary", "STREAM_COMMENT"),
    );
    fixture.terminal.wait_text("STREAM_COMMENT").await;
    fixture.nested(
        &turn,
        "assistant.message",
        payload("comment", "commentary", "STREAM_COMMENT"),
    );
    fixture.nested(
        &turn,
        "assistant.delta",
        payload("answer", "final_answer", "STREAM_FIRST"),
    );
    // Completion is deliberately withheld until the terminal has rendered each
    // chunk. A client that buffers until assistant.message times out here.
    fixture.terminal.wait_text("STREAM_FIRST").await;
    fixture.nested(
        &turn,
        "assistant.delta",
        payload("answer", "final_answer", "_SECOND"),
    );
    fixture.terminal.wait_text("STREAM_FIRST_SECOND").await;
    let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
    assert_eq!(screen.matches("STREAM_FIRST").count(), 1, "{screen}");

    fixture.nested(
        &turn,
        "assistant.message",
        payload("answer", "final_answer", "STREAM_FIRST_SECOND_FINAL"),
    );
    fixture.complete(&turn);
    fixture
        .terminal
        .wait_text("STREAM_FIRST_SECOND_FINAL")
        .await;
    fixture.terminal.wait_text("Enter send").await;
    let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
    assert_eq!(screen.matches("STREAM_FIRST").count(), 1, "{screen}");
    assert_eq!(screen.matches("STREAM_COMMENT").count(), 1, "{screen}");
}

#[tokio::test]
async fn terminal_final_only_response_does_not_overwrite_the_previous_turn() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("first question", "\r");
    let first = fixture.submission("first question").await;
    fixture.nested(&first, "assistant.delta", json!({"model_call_index": 1, "item_id": "first-answer", "phase": "final_answer", "text": "FIRST_ANSWER_REMAINS_VISIBLE"}));
    fixture.nested(&first, "assistant.message", json!({"model_call_index": 1, "item_id": "first-answer", "phase": "final_answer", "text": "FIRST_ANSWER_REMAINS_VISIBLE"}));
    fixture.complete(&first);
    fixture
        .terminal
        .wait_text("FIRST_ANSWER_REMAINS_VISIBLE")
        .await;
    fixture.terminal.wait_text("Enter send").await;
    fixture.terminal.prompt("second question", "\r");
    let second = fixture.submission("second question").await;
    fixture.nested(&second, "assistant.message", json!({"model_call_index": 1, "item_id": "second-answer", "phase": "final_answer", "text": "SECOND_FINAL_ONLY_ANSWER"}));
    fixture.complete(&second);
    fixture.terminal.wait_text("SECOND_FINAL_ONLY_ANSWER").await;
    let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
    assert!(screen.contains("FIRST_ANSWER_REMAINS_VISIBLE"), "{screen}");
    assert_eq!(
        screen.matches("SECOND_FINAL_ONLY_ANSWER").count(),
        1,
        "{screen}"
    );
}

#[tokio::test]
async fn terminal_answers_without_item_ids_stay_with_their_own_turn() {
    let mut fixture = Fixture::start().await;
    for (question, answer) in [
        ("first anonymous question", "FIRST_ANONYMOUS_ANSWER"),
        ("second anonymous question", "SECOND_ANONYMOUS_ANSWER"),
    ] {
        fixture.terminal.prompt(question, "\r");
        let turn = fixture.submission(question).await;
        fixture.nested(&turn, "assistant.delta", json!({"model_call_index": 1, "item_id": null, "phase": "final_answer", "text": "partial"}));
        fixture.nested(&turn, "assistant.message", json!({"model_call_index": 1, "item_id": null, "phase": "final_answer", "text": answer}));
        fixture.complete(&turn);
        fixture.terminal.wait_text(answer).await;
        fixture.terminal.wait_text("Enter send").await;
    }
    let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
    assert!(screen.contains("FIRST_ANONYMOUS_ANSWER"), "{screen}");
    assert!(screen.contains("SECOND_ANONYMOUS_ANSWER"), "{screen}");
}

#[tokio::test]
async fn terminal_long_session_keeps_every_answer_when_scrolling_back() {
    let mut fixture = Fixture::start().await;
    for index in 0..12 {
        let question = format!("history question {index:02}");
        let answer = format!("HISTORY_ANSWER_{index:02}");
        fixture.terminal.prompt(&question, "\r");
        let turn = fixture.submission(&question).await;
        if index % 2 == 0 {
            fixture.nested(&turn, "assistant.delta", json!({"model_call_index": 1, "item_id": null, "phase": "final_answer", "text": "partial answer"}));
        }
        let item = (index % 3 == 0).then(|| format!("answer-{index}"));
        fixture.nested(&turn, "assistant.message", json!({"model_call_index": 1, "item_id": item, "phase": "final_answer", "text": answer}));
        fixture.complete(&turn);
        fixture.terminal.wait_text(&answer).await;
        fixture.terminal.wait_text("Enter send").await;
    }
    let mut visible_history = String::new();
    for _ in 0..20 {
        let before = fixture.terminal.screen.lock().unwrap().screen().contents();
        visible_history.push_str(&before);
        if before.contains("HISTORY_ANSWER_00") {
            break;
        }
        fixture.terminal.input("\x1b[5~");
        tokio::time::timeout(TIMEOUT, async {
            while fixture.terminal.screen.lock().unwrap().screen().contents() == before {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("PageUp should reveal older transcript entries");
    }
    for index in 0..12 {
        assert!(
            visible_history.contains(&format!("HISTORY_ANSWER_{index:02}")),
            "answer {index} disappeared from retained history: {visible_history}"
        );
    }
}

#[tokio::test]
async fn terminal_shows_the_durable_answer_when_the_final_stream_message_is_missing() {
    let mut fixture = Fixture::start().await;
    fixture
        .terminal
        .prompt("finish without a final stream message", "\r");
    let turn = fixture
        .submission("finish without a final stream message")
        .await;
    fixture.nested(&turn, "assistant.delta", json!({"model_call_index": 1, "item_id": "partial", "phase": "final_answer", "text": "DURABLE_ANSWER"}));
    fixture.emit(&turn, json!({"type": "turn_completed", "id": turn, "final_message": "DURABLE_ANSWER_IS_COMPLETE", "usage": null, "citations": [], "usage_error": null}));
    fixture
        .terminal
        .wait_text("DURABLE_ANSWER_IS_COMPLETE")
        .await;
    fixture.terminal.wait_text("Enter send").await;
    let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
    assert_eq!(screen.matches("DURABLE_ANSWER").count(), 1, "{screen}");
    fixture
        .terminal
        .prompt("finish without any streamed text", "\r");
    let turn = fixture.submission("finish without any streamed text").await;
    fixture.emit(&turn, json!({"type": "turn_completed", "id": turn, "final_message": "ANSWER_WITHOUT_ANY_STREAM", "usage": null, "citations": [], "usage_error": null}));
    fixture
        .terminal
        .wait_text("ANSWER_WITHOUT_ANY_STREAM")
        .await;
    fixture.terminal.wait_text("Enter send").await;
}

#[tokio::test]
async fn terminal_settles_tools_when_only_the_durable_completion_arrives() {
    let mut fixture = Fixture::start().await;
    fixture
        .terminal
        .prompt("finish a tool without its last stream events", "\r");
    let turn = fixture
        .submission("finish a tool without its last stream events")
        .await;
    fixture.nested(&turn, "run.started", json!({}));
    fixture.nested(&turn, "tool.call", json!({"call_id": "unfinished-read", "tool": "read_file", "arguments": {"path": "MISSING_TOOL_RESULT.txt"}}));
    fixture.terminal.wait_text("MISSING_TOOL_RESULT.txt").await;
    fixture.emit(&turn, json!({"type": "turn_completed", "id": turn, "final_message": "DURABLE_TOOL_TURN_FINISHED", "usage": null, "citations": []}));
    fixture
        .terminal
        .wait_text("DURABLE_TOOL_TURN_FINISHED")
        .await;
    fixture
        .terminal
        .wait_text("tool call ended without a terminal result")
        .await;
    fixture.terminal.wait_text("Enter send").await;
    fixture
        .terminal
        .prompt("NEXT_TURN_AFTER_MISSING_TERMINAL", "\r");
    let next = fixture.submission("NEXT_TURN_AFTER_MISSING_TERMINAL").await;
    fixture.complete(&next);
    fixture.terminal.wait_text("Enter send").await;
}

#[tokio::test]
async fn terminal_does_not_assign_a_previous_turns_error_to_the_next_failure() {
    let mut fixture = Fixture::start().await;
    fixture
        .terminal
        .prompt("recover from a transient connection failure", "\r");
    let first = fixture
        .submission("recover from a transient connection failure")
        .await;
    fixture.nested(&first, "run.started", json!({}));
    fixture.nested(
        &first,
        "model.connection.failed",
        json!({"error": "PREVIOUS_TURN_CONNECTION_ERROR"}),
    );
    fixture.emit(&first, json!({"type": "turn_completed", "id": first, "final_message": "FIRST_TURN_RECOVERED", "usage": null, "citations": []}));
    fixture.terminal.wait_text("FIRST_TURN_RECOVERED").await;
    fixture.terminal.wait_text("Enter send").await;
    fixture.terminal.prompt("a separate turn fails", "\r");
    let second = fixture.submission("a separate turn fails").await;
    fixture.nested(&second, "run.started", json!({}));
    fixture.nested(&second, "run.failed", json!({}));
    fixture.terminal.wait_text("The agent run failed").await;
    fixture.emit(
        &second,
        json!({"type": "turn_failed", "id": second, "error": "CURRENT_TURN_FAILURE"}),
    );
    fixture.terminal.wait_text("CURRENT_TURN_FAILURE").await;
    fixture.terminal.wait_no_text("The agent run failed").await;
    let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
    assert!(
        !screen.contains("PREVIOUS_TURN_CONNECTION_ERROR"),
        "{screen}"
    );
    fixture.terminal.wait_text("Enter send").await;
}

#[tokio::test]
async fn terminal_keeps_interleaved_progress_summaries_separate_and_the_queue_responsive() {
    let mut fixture = Fixture::start_with_active(true).await;
    let other = "019fc927-b282-79a7-8445-1b9996ad2fb0";
    fixture.nested(REMOTE_TURN, "run.started", json!({}));
    fixture.emit(other, json!({"type": "turn_accepted", "id": other, "input": "another client task", "replayed": false}));
    fixture.nested(other, "run.started", json!({}));
    for (turn, text) in [(REMOTE_TURN, "FIRST_BEFORE_"), (other, "SECOND_BEFORE_")] {
        fixture.nested(
            turn,
            "reasoning.summary.delta",
            json!({"model_call_index": 1, "text": text}),
        );
    }
    fixture.terminal.wait_text("SECOND_BEFORE_").await;
    fixture.terminal.prompt("KEEP_EACH_TASK_FOCUSED", "\r");
    let (steer, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(steer["turn_id"], REMOTE_TURN);
    assert_eq!(prompt_text(&steer["input"]), "KEEP_EACH_TASK_FOCUSED");
    ack.send(true).unwrap();
    fixture.terminal.wait_text("steering accepted").await;
    for (turn, text) in [(REMOTE_TURN, "FIRST_AFTER"), (other, "SECOND_AFTER")] {
        fixture.nested(
            turn,
            "reasoning.summary.delta",
            json!({"model_call_index": 1, "text": text}),
        );
    }
    fixture.terminal.wait_text("SECOND_AFTER").await;
    fixture.terminal.wait_text("FIRST_BEFORE_FIRST_AFTER").await;
    fixture
        .terminal
        .wait_text("SECOND_BEFORE_SECOND_AFTER")
        .await;
    fixture.terminal.prompt("FOLLOW_UP_AFTER_BOTH", "\t");
    fixture.terminal.wait_text("FOLLOW_UP_AFTER_BOTH").await;
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("done").await;
    fixture.terminal.wait_text("Enter steer").await;
    assert!(fixture.submissions.try_recv().is_err());
    fixture.complete(other);
    let next = fixture.submission("FOLLOW_UP_AFTER_BOTH").await;
    fixture.complete(&next);
    fixture.terminal.wait_text("Enter send").await;
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_late_events_from_finished_turns_do_not_revive_retry_or_busy_state() {
    for outcome in ["completed", "failed", "cancelled"] {
        let mut fixture = Fixture::start().await;
        fixture.terminal.prompt("first turn", "\r");
        let first = fixture.submission("first turn").await;
        fixture.nested(&first, "run.started", json!({}));
        let terminal = match outcome {
            "completed" => {
                json!({"type": "turn_completed", "id": first, "final_message": "FIRST_FINISHED", "usage": null, "citations": []})
            }
            "failed" => json!({"type": "turn_failed", "id": first, "error": "FIRST_FAILED"}),
            _ => json!({"type": "turn_cancelled", "id": first}),
        };
        fixture.emit(&first, terminal);
        fixture.terminal.wait_text("Enter send").await;
        fixture.terminal.prompt("second turn", "\r");
        let second = fixture.submission("second turn").await;
        fixture.nested(&second, "run.started", json!({}));
        fixture.nested(&first, "run.started", json!({}));
        fixture.nested(
            &first,
            "model.attempt.retrying",
            json!({"delay_ns": 60_000_000_000_u64, "error": "late retry from finished turn"}),
        );
        // A later event on the same stream is the processing barrier for the stale events.
        fixture.nested(&second, "assistant.message", json!({"model_call_index": 1, "phase": "commentary", "text": "CURRENT_PROGRESS_BARRIER"}));
        fixture.terminal.wait_text("CURRENT_PROGRESS_BARRIER").await;
        fixture.terminal.wait_no_text("Retrying in").await;
        fixture.terminal.prompt("STEER_THE_CURRENT_TURN", "\r");
        let (steer, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(steer["turn_id"], second);
        assert_eq!(prompt_text(&steer["input"]), "STEER_THE_CURRENT_TURN");
        ack.send(true).unwrap();
        fixture.terminal.wait_text("steering accepted").await;
        fixture.complete(&second);
        fixture.terminal.wait_text("Enter send").await;
        fixture.terminal.prompt("still responsive", "\r");
        let next = fixture.submission("still responsive").await;
        fixture.complete(&next);
        fixture.terminal.wait_text("Enter send").await;
    }
}

#[tokio::test]
async fn terminal_preserves_retry_status_when_other_turns_finish() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.nested(REMOTE_TURN, "run.started", json!({}));
    let other = "other-active-turn";
    fixture.emit(other, json!({"type": "turn_accepted", "id": other, "input": "other client work", "replayed": false}));
    fixture.nested(other, "run.started", json!({}));
    fixture.nested(
        REMOTE_TURN,
        "model.attempt.retrying",
        json!({"delay_ns": 60_000_000_000_u64, "error": "temporary provider failure"}),
    );
    fixture.terminal.wait_text("Retrying in").await;
    fixture.terminal.prompt("DRAFT_DURING_OTHER_TURNS", "");
    fixture.emit(other, json!({"type": "turn_completed", "id": other, "final_message": "OTHER_TURN_FINISHED", "usage": null, "citations": []}));
    fixture.terminal.wait_text("OTHER_TURN_FINISHED").await;
    fixture.terminal.wait_text("Retrying in").await;
    let newer = "newer-active-turn";
    fixture.emit(newer, json!({"type": "turn_accepted", "id": newer, "input": "more client work", "replayed": false}));
    fixture.nested(newer, "run.started", json!({}));
    fixture.nested(newer, "model.warmup.started", json!({}));
    fixture.terminal.wait_text("Warming model").await;
    fixture.emit(newer, json!({"type": "turn_completed", "id": newer, "final_message": "NEWER_TURN_FINISHED", "usage": null, "citations": []}));
    fixture.terminal.wait_text("NEWER_TURN_FINISHED").await;
    fixture.terminal.wait_text("Retrying in").await;
    fixture.terminal.wait_text("DRAFT_DURING_OTHER_TURNS").await;
    fixture.terminal.input("\r");
    let (steer, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prompt_text(&steer["input"]), "DRAFT_DURING_OTHER_TURNS");
    ack.send(true).unwrap();
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("Enter send").await;
}

#[tokio::test]
async fn terminal_keeps_background_commands_connected_across_turns() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("start a background build", "\r");
    let first = fixture.submission("start a background build").await;
    fixture.nested(&first, "run.started", json!({}));
    fixture.nested(&first, "tool.call", json!({"call_id": "background-build", "tool": "exec_command", "arguments": {"cmd": "BACKGROUND_BUILD_COMMAND"}}));
    fixture.nested(&first, "tool.result", json!({"call_id": "background-build", "tool": "exec_command", "status": "completed", "duration_ns": 1, "result": {"session_id": 7, "exit_code": null, "output": "BUILD_STARTED\n"}}));
    fixture.nested(
        &first,
        "assistant.message",
        json!({"model_call_index": 1, "phase": "final_answer", "text": "BUILD_IS_RUNNING"}),
    );
    fixture.complete(&first);
    fixture.terminal.wait_text("BUILD_IS_RUNNING").await;
    fixture.terminal.wait_text("Enter send").await;
    let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
    assert!(
        !screen.contains("tool call ended without a terminal result"),
        "a yielded process is not an orphaned call: {screen}"
    );
    fixture.terminal.prompt("check that build", "\r");
    let second = fixture.submission("check that build").await;
    fixture.nested(&second, "run.started", json!({}));
    fixture.nested(
        &second,
        "tool.call",
        json!({"call_id": "build-poll", "tool": "write_stdin", "arguments": {"session_id": 7}}),
    );
    fixture.nested(&second, "tool.result", json!({"call_id": "build-poll", "tool": "write_stdin", "status": "completed", "duration_ns": 1, "result": {"session_id": 7, "exit_code": 0, "output": "BUILD_FINISHED\n"}}));
    fixture.nested(
        &second,
        "assistant.message",
        json!({"model_call_index": 1, "phase": "final_answer", "text": "BUILD_COMPLETE"}),
    );
    fixture.complete(&second);
    fixture.terminal.wait_text("BUILD_COMPLETE").await;
    fixture.terminal.wait_text("Enter send").await;
    let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
    assert_eq!(
        screen.matches("BACKGROUND_BUILD_COMMAND").count(),
        1,
        "{screen}"
    );
    assert!(
        !screen.contains("tool call ended without a terminal result"),
        "{screen}"
    );
}

fn active_restore_history(kind: &str, payload: Value) -> Vec<Value> {
    vec![
        json!({"cursor": "1", "turn_id": REMOTE_TURN, "type": "turn_accepted", "id": REMOTE_TURN, "input": "RESTORED_ACTIVE_PROMPT", "replayed": false}),
        json!({"cursor": "2", "turn_id": REMOTE_TURN, "type": "event", "event": {"protocol_version": 1, "request_id": AGENT, "seq": 1, "type": "run.started", "payload": {}}}),
        json!({"cursor": "3", "turn_id": REMOTE_TURN, "type": "event", "event": {"protocol_version": 1, "request_id": AGENT, "seq": 2, "type": kind, "payload": payload}}),
    ]
}

#[tokio::test]
async fn terminal_live_restore_keeps_an_attached_tool_running() {
    let history = active_restore_history(
        "tool.call",
        json!({"call_id": "pending-read", "tool": "read_file", "arguments": {"path": "PENDING_ON_ATTACH.txt"}}),
    );
    let mut fixture = Fixture::start_with_history(true, true, history).await;
    fixture.terminal.wait_text("PENDING_ON_ATTACH.txt").await;
    let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
    assert!(
        !screen.contains("tool call ended without a terminal result"),
        "active history must stay open: {screen}"
    );
    fixture.terminal.prompt("DRAFT_AFTER_ATTACH", "");
    fixture.nested(REMOTE_TURN, "tool.result", json!({"call_id": "pending-read", "tool": "read_file", "status": "completed", "duration_ns": 1, "result": {"text": "file contents"}}));
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("Enter send").await;
    assert!(fixture.submissions.try_recv().is_err());
    fixture.terminal.input("\r");
    let next = fixture.submission("DRAFT_AFTER_ATTACH").await;
    fixture.complete(&next);
}

#[tokio::test]
async fn terminal_cancels_a_slow_session_switch_and_can_resume_again() {
    const OTHER_AGENT: &str = "019fc927-b280-79a7-8445-1b9996ad2fb1";
    for (cancel, disconnected) in [("\x1b", false), ("\x03", false), ("\x1b", true)] {
        let mut fixture = Fixture::start().await;
        *fixture.listed_agent.lock().unwrap() = OTHER_AGENT.to_owned();
        let pause = fixture.resume_gate.clone().acquire_owned().await.unwrap();
        fixture.terminal.input("/");
        fixture.terminal.wait_text("Resume session").await;
        fixture.terminal.input("restore");
        fixture
            .terminal
            .wait_no_text("finish active work first")
            .await;
        fixture.terminal.input("\r");
        fixture.terminal.wait_text("RETAINED_REMOTE_WORK").await;
        fixture.terminal.input("\r");
        fixture.terminal.wait_text("Resuming session").await;
        if disconnected {
            fixture.break_stream();
            fixture
                .terminal
                .wait_text("Previous session disconnected")
                .await;
        }
        fixture.terminal.input(cancel);
        if disconnected {
            fixture.replacement_connection().await;
            fixture.terminal.wait_text("Reconnected").await;
        } else {
            fixture.terminal.wait_text("Session switch cancelled").await;
        }
        fixture.terminal.wait_no_text("Resuming session").await;
        fixture
            .terminal
            .prompt("STILL_IN_THE_ORIGINAL_SESSION", "\r");
        let message = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message["fixture_agent_id"], AGENT);
        assert_eq!(
            prompt_text(&message["input"]),
            "STILL_IN_THE_ORIGINAL_SESSION"
        );
        let turn = message["id"].as_str().unwrap().to_owned();
        fixture.emit(&turn, json!({"type": "turn_accepted", "id": turn, "input": message["input"], "replayed": false}));
        fixture.complete(&turn);
        fixture.terminal.wait_text("Enter send").await;
        fixture.terminal.input("/");
        fixture.terminal.wait_text("Resume session").await;
        fixture.terminal.input("restore");
        fixture
            .terminal
            .wait_no_text("finish active work first")
            .await;
        fixture.terminal.input("\r");
        fixture.terminal.wait_text("RETAINED_REMOTE_WORK").await;
        fixture.terminal.input("\r");
        fixture.terminal.wait_text("Resuming session").await;
        drop(pause);
        fixture.replacement_connection().await;
        fixture.terminal.wait_no_text("Resuming session").await;
        fixture.terminal.prompt("AFTER_A_FRESH_RESUME", "\r");
        let message = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message["fixture_agent_id"], OTHER_AGENT);
        assert_eq!(prompt_text(&message["input"]), "AFTER_A_FRESH_RESUME");
        let turn = message["id"].as_str().unwrap().to_owned();
        fixture.emit(&turn, json!({"type": "turn_accepted", "id": turn, "input": message["input"], "replayed": false}));
        fixture.complete(&turn);
        fixture.terminal.wait_text("Enter send").await;
        assert!(fixture.submissions.try_recv().is_err());
        assert!(fixture.connections.try_recv().is_err());
    }
}

#[tokio::test]
async fn terminal_does_not_reactivate_the_old_agent_during_a_slow_session_switch() {
    const OTHER_AGENT: &str = "019fc927-b280-79a7-8445-1b9996ad2fb1";
    for succeeds in [true, false] {
        let mut fixture = Fixture::start().await;
        *fixture.listed_agent.lock().unwrap() = OTHER_AGENT.to_owned();
        let pause = fixture.resume_gate.clone().acquire_owned().await.unwrap();
        fixture.terminal.input("/");
        fixture.terminal.wait_text("Resume session").await;
        fixture.terminal.input("restore\r");
        fixture.terminal.wait_text("RETAINED_REMOTE_WORK").await;
        fixture.terminal.input("\r");
        fixture.terminal.wait_text("Resuming session").await;
        fixture.break_stream();
        fixture
            .terminal
            .wait_text("Previous session disconnected")
            .await;
        fixture.terminal.wait_text("Resuming session").await;
        assert!(fixture.connections.try_recv().is_err());
        assert!(fixture.submissions.try_recv().is_err());
        fixture.state_available.store(succeeds, Ordering::SeqCst);
        drop(pause);
        if succeeds {
            fixture.replacement_connection().await;
            fixture.terminal.wait_no_text("Resuming session").await;
        } else {
            fixture.terminal.wait_text("Connection lost").await;
            fixture.state_available.store(true, Ordering::SeqCst);
            fixture.terminal.input("\r");
            fixture.replacement_connection().await;
            fixture.terminal.wait_text("Reconnected").await;
        }
        fixture.terminal.prompt("FOR_THE_SELECTED_AGENT", "\r");
        let message = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            message["fixture_agent_id"],
            if succeeds { OTHER_AGENT } else { AGENT }
        );
        assert_eq!(prompt_text(&message["input"]), "FOR_THE_SELECTED_AGENT");
        let turn = message["id"].as_str().unwrap().to_owned();
        fixture.emit(&turn, json!({"type": "turn_accepted", "id": turn, "input": message["input"], "replayed": false}));
        fixture.complete(&turn);
        fixture.terminal.wait_text("Enter send").await;
        assert!(fixture.submissions.try_recv().is_err());
        assert!(fixture.connections.try_recv().is_err());
    }
}

#[tokio::test]
async fn terminal_keeps_local_shell_context_scoped_to_the_session_after_resume() {
    const OTHER_AGENT: &str = "019fc927-b280-79a7-8445-1b9996ad2fb1";
    for succeeds in [true, false] {
        let mut fixture = Fixture::start().await;
        fixture
            .terminal
            .prompt("!printf OLD_SESSION_SHELL_OUTPUT", "\r");
        fixture.terminal.wait_text("exit 0").await;
        *fixture.listed_agent.lock().unwrap() = OTHER_AGENT.to_owned();
        fixture.terminal.input("/");
        fixture.terminal.wait_text("Resume session").await;
        fixture.terminal.input("restore\r");
        fixture.terminal.wait_text("RETAINED_REMOTE_WORK").await;
        fixture.state_available.store(succeeds, Ordering::SeqCst);
        fixture.terminal.input("\r");
        if succeeds {
            fixture.replacement_connection().await;
            fixture
                .terminal
                .wait_no_text("OLD_SESSION_SHELL_OUTPUT")
                .await;
        } else {
            fixture.terminal.wait_text("try again").await;
            fixture.state_available.store(true, Ordering::SeqCst);
        }
        fixture.terminal.wait_no_text("Resuming session").await;
        fixture.terminal.prompt("PROMPT_AFTER_RESUME_ATTEMPT", "\r");
        let message = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            message["fixture_agent_id"],
            if succeeds { OTHER_AGENT } else { AGENT }
        );
        assert_eq!(
            prompt_text(&message["input"]),
            if succeeds {
                "PROMPT_AFTER_RESUME_ATTEMPT"
            } else {
                "<local_shell_result>\ncommand: printf OLD_SESSION_SHELL_OUTPUT\noutcome: exit 0\noutput:\nOLD_SESSION_SHELL_OUTPUT\n</local_shell_result>\n\nPROMPT_AFTER_RESUME_ATTEMPT"
            }
        );
        let turn = message["id"].as_str().unwrap().to_owned();
        fixture.emit(&turn, json!({"type": "turn_accepted", "id": turn, "input": message["input"], "replayed": false}));
        fixture.complete(&turn);
        fixture.terminal.wait_text("Enter send").await;
        assert!(fixture.submissions.try_recv().is_err());
    }
}

#[tokio::test]
async fn terminal_live_restore_from_the_session_picker_keeps_retry_status() {
    let mut fixture = Fixture::start().await;
    for mut event in active_restore_history(
        "model.attempt.retrying",
        json!({"delay_ns": 60_000_000_000_u64, "error": "temporary failure"}),
    ) {
        event.as_object_mut().unwrap().remove("cursor");
        fixture.retain(REMOTE_TURN, event);
    }
    fixture.terminal.input("/");
    fixture.terminal.wait_text("Resume session").await;
    fixture.terminal.input("restore\r");
    fixture.terminal.wait_text("RETAINED_REMOTE_WORK").await;
    fixture.terminal.input("\r");
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("RESTORED_ACTIVE_PROMPT").await;
    fixture.terminal.wait_text("Retrying in").await;
    fixture.terminal.prompt("STEER_AFTER_RESTORE", "\r");
    let (steer, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prompt_text(&steer["input"]), "STEER_AFTER_RESTORE");
    ack.send(true).unwrap();
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("Enter send").await;
}

#[tokio::test]
async fn terminal_steering_and_queueing_preserve_the_active_warmup_status() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.nested(REMOTE_TURN, "run.started", json!({}));
    fixture.nested(REMOTE_TURN, "model.warmup.started", json!({}));
    fixture.terminal.wait_text("Warming model").await;
    fixture.terminal.prompt("STEER_DURING_WARMUP", "\r");
    let (steer, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prompt_text(&steer["input"]), "STEER_DURING_WARMUP");
    ack.send(true).unwrap();
    fixture.terminal.wait_text("steering accepted").await;
    fixture.terminal.wait_text("Warming model").await;
    fixture.terminal.prompt("QUEUE_DURING_WARMUP", "\t");
    fixture.terminal.wait_text("QUEUE_DURING_WARMUP").await;
    fixture.terminal.wait_text("Warming model").await;
    fixture.complete(REMOTE_TURN);
    let next = fixture.submission("QUEUE_DURING_WARMUP").await;
    fixture.complete(&next);
    fixture.terminal.wait_text("Enter send").await;
}

#[tokio::test]
async fn terminal_failed_initial_attach_retries_without_submitting_its_draft() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.state_available.store(false, Ordering::SeqCst);
    fixture.terminal = Terminal::start(&fixture.origin, true);
    fixture.terminal.wait_text("Connection lost").await;
    fixture.terminal.prompt("DRAFT_THROUGH_ATTACH_RETRY", "");
    fixture
        .terminal
        .wait_text("DRAFT_THROUGH_ATTACH_RETRY")
        .await;
    fixture.state_available.store(true, Ordering::SeqCst);
    fixture.terminal.input("\r");
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("Enter steer").await;
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());
    fixture.terminal.input("\r");
    let (steer, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(steer["turn_id"], REMOTE_TURN);
    assert_eq!(prompt_text(&steer["input"]), "DRAFT_THROUGH_ATTACH_RETRY");
    ack.send(true).unwrap();
    fixture.terminal.wait_text("steering accepted").await;
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("Enter send").await;
}

#[tokio::test]
async fn terminal_voice_during_attach_waits_and_can_be_muted_or_cancelled() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let mut fixture = Fixture::launch_with_history(false, true, Vec::new(), gate.clone()).await;
    fixture.terminal.wait_text("Connecting").await;
    fixture.terminal.prompt("/voice", "\r");
    fixture.terminal.wait_text("ctrl+x mute").await;
    fixture.terminal.prompt("DRAFT_WHILE_VOICE_CONNECTS", "");
    fixture.terminal.input("\x18");
    fixture.terminal.wait_text("ctrl+x unmute").await;
    fixture
        .terminal
        .wait_text("DRAFT_WHILE_VOICE_CONNECTS")
        .await;
    fixture.terminal.input("\x15");
    fixture.terminal.prompt("/voice", "\r");
    fixture
        .terminal
        .wait_text_presence("ctrl+x unmute", false)
        .await;
    gate.add_permits(1);
    fixture.terminal.wait_text("Enter send").await;
    fixture.terminal.prompt("/voice status", "\r");
    fixture.terminal.wait_text("Voice is off").await;
    let output = fixture.terminal.output.lock().unwrap();
    assert!(!String::from_utf8_lossy(&output).contains("Try /voice when connected"));
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_enter_during_attach_does_not_start_an_unintended_parallel_turn() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let history = active_restore_history("run.warming", json!({}));
    let mut fixture = Fixture::launch_with_history(true, true, history, gate.clone()).await;
    fixture.terminal.wait_text("Connecting").await;
    fixture.terminal.prompt("STEER_AFTER_ATTACH", "\r");
    fixture.terminal.prompt("_EDITED", "");
    fixture.terminal.wait_text("_EDITED").await;
    gate.add_permits(1);
    fixture.terminal.wait_text("Enter steer").await;
    fixture.terminal.input("\r");
    let (steer, ack) = tokio::time::timeout(TIMEOUT, async {
        tokio::select! {
            steer = fixture.steers.recv() => steer.unwrap(),
            submission = fixture.submissions.recv() => {
                panic!("attaching must not start an unintended parallel turn: {submission:?}");
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(steer["turn_id"], REMOTE_TURN);
    assert_eq!(prompt_text(&steer["input"]), "STEER_AFTER_ATTACH_EDITED");
    ack.send(true).unwrap();
    fixture.terminal.wait_text("steering accepted").await;
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("Enter send").await;
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_keeps_a_draft_and_completion_received_while_attach_history_is_loading() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let history = active_restore_history(
        "tool.call",
        json!({"call_id": "attach-read", "tool": "read_file", "arguments": {"path": "file"}}),
    );
    let mut fixture = Fixture::launch_with_history(true, true, history, gate.clone()).await;
    fixture.terminal.wait_text("Connecting").await;
    fixture.terminal.prompt("DRAFT_DURING_INITIAL_ATTACH", "");
    fixture
        .terminal
        .wait_text("DRAFT_DURING_INITIAL_ATTACH")
        .await;
    fixture.nested(REMOTE_TURN, "tool.result", json!({"call_id": "attach-read", "tool": "read_file", "status": "completed", "duration_ns": 1, "result": {"text": "file contents"}}));
    fixture.emit(REMOTE_TURN, json!({"type": "turn_completed", "id": REMOTE_TURN, "final_message": "COMPLETED_DURING_ATTACH", "usage": null, "citations": []}));
    gate.add_permits(1);
    fixture.terminal.wait_text("COMPLETED_DURING_ATTACH").await;
    fixture.terminal.wait_text("Enter send").await;
    assert_eq!(
        fixture.history_requests.lock().unwrap()[0],
        4,
        "history must stop at the pre-completion snapshot cursor"
    );
    fixture
        .terminal
        .wait_text("DRAFT_DURING_INITIAL_ATTACH")
        .await;
    assert!(fixture.submissions.try_recv().is_err());
    fixture.terminal.input("\r");
    let next = fixture.submission("DRAFT_DURING_INITIAL_ATTACH").await;
    fixture.complete(&next);
}

async fn assert_terminal_durable_stop(cancelled: bool) {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.nested(REMOTE_TURN, "run.started", json!({}));
    fixture.nested(REMOTE_TURN, "assistant.delta", json!({"model_call_index": 1, "item_id": "partial", "phase": "final_answer", "text": "PARTIAL_BEFORE_DURABLE_STOP"}));
    fixture.nested(REMOTE_TURN, "tool.call", json!({"call_id": "unfinished-stop-read", "tool": "read_file", "arguments": {"path": "UNFINISHED_STOP_READ.txt"}}));
    fixture.terminal.wait_text("UNFINISHED_STOP_READ.txt").await;
    if cancelled {
        fixture.terminal.input("\x1b");
        fixture.terminal.wait_text("Interrupt").await;
        fixture.terminal.input("\x1b");
        assert_eq!(
            tokio::time::timeout(TIMEOUT, fixture.cancellations.recv())
                .await
                .unwrap()
                .unwrap(),
            REMOTE_TURN
        );
        fixture.terminal.wait_text("Interrupted response").await;
    }
    fixture.terminal.prompt("DRAFT_AFTER_DURABLE_STOP", "");
    fixture.emit(
        REMOTE_TURN,
        if cancelled {
            json!({"type": "turn_cancelled", "id": REMOTE_TURN})
        } else {
            json!({"type": "turn_failed", "id": REMOTE_TURN, "error": "DURABLE_FAILURE_REASON"})
        },
    );
    fixture
        .terminal
        .wait_text("tool call ended without a terminal result")
        .await;
    if !cancelled {
        fixture.terminal.wait_text("DURABLE_FAILURE_REASON").await;
    }
    fixture.terminal.wait_text("Enter send").await;
    let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
    assert!(screen.contains("PARTIAL_BEFORE_DURABLE_STOP"), "{screen}");
    assert!(screen.contains("DRAFT_AFTER_DURABLE_STOP"), "{screen}");
    assert!(fixture.submissions.try_recv().is_err());
    fixture.terminal.input("\r");
    let next = fixture.submission("DRAFT_AFTER_DURABLE_STOP").await;
    fixture.complete(&next);
    fixture.terminal.wait_text("Enter send").await;
}

#[tokio::test]
async fn terminal_durable_failure_settles_tools_and_preserves_the_next_draft() {
    assert_terminal_durable_stop(false).await;
}

#[tokio::test]
async fn terminal_durable_cancellation_settles_tools_and_preserves_the_next_draft() {
    assert_terminal_durable_stop(true).await;
}

#[tokio::test]
async fn terminal_retains_a_long_older_response_across_history_page_boundaries() {
    let mut history = Vec::new();
    let old = "older-history-turn";
    let new = "newer-history-turn";
    let mut retain = |turn: &str, mut event: Value| {
        event["cursor"] = json!((history.len() + 1).to_string());
        event["turn_id"] = json!(turn);
        history.push(event);
    };
    retain(
        old,
        json!({"type": "turn_accepted", "id": old, "input": "OLDEST_RETAINED_QUESTION", "replayed": false}),
    );
    for index in 0..700 {
        retain(
            old,
            json!({"type": "event", "event": {"protocol_version": 1, "request_id": AGENT, "seq": index + 1, "type": "run.steered", "payload": {"steer_index": index, "instruction_bytes": 1}}}),
        );
    }
    retain(
        old,
        json!({"type": "turn_completed", "id": old, "final_message": "OLDEST_RETAINED_ANSWER", "usage": null, "citations": []}),
    );
    retain(
        new,
        json!({"type": "turn_accepted", "id": new, "input": "newest question", "replayed": false}),
    );
    retain(
        new,
        json!({"type": "turn_completed", "id": new, "final_message": "LATEST_RETAINED_ANSWER", "usage": null, "citations": []}),
    );
    let mut fixture = Fixture::start_with_history(false, true, history).await;
    fixture.terminal.wait_text("LATEST_RETAINED_ANSWER").await;
    fixture.terminal.prompt("DRAFT_DURING_OLDER_HISTORY", "");
    tokio::time::timeout(TIMEOUT, async {
        loop {
            fixture.terminal.input("\x1b[5~");
            tokio::time::sleep(Duration::from_millis(25)).await;
            if fixture
                .terminal
                .screen
                .lock()
                .unwrap()
                .screen()
                .contents()
                .contains("OLDEST_RETAINED_QUESTION")
            {
                break;
            }
        }
    })
    .await
    .expect("scrolling back should reach the oldest prompt");
    let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
    assert!(screen.contains("OLDEST_RETAINED_ANSWER"), "{screen}");
    assert!(screen.contains("DRAFT_DURING_OLDER_HISTORY"), "{screen}");
    assert!(fixture.history_requests.lock().unwrap().len() >= 3);
    assert!(fixture.submissions.try_recv().is_err());
    fixture.terminal.input("\r");
    let turn = fixture.submission("DRAFT_DURING_OLDER_HISTORY").await;
    fixture.complete(&turn);
}

#[tokio::test]
async fn terminal_batch_children_expand_independently_and_collapse_with_parent() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.nested(
        REMOTE_TURN,
        "tool.call",
        json!({
            "call_id": "batch", "tool": "exec", "arguments": "await runChecks()"
        }),
    );
    for (id, command, output) in [
        ("batch/code-0", "check-first", "FIRST_CHILD_OUTPUT"),
        ("batch/code-1", "check-second", "SECOND_CHILD_OUTPUT"),
    ] {
        fixture.nested(
            REMOTE_TURN,
            "tool.call",
            json!({
                "call_id": id, "tool": "exec_command", "arguments": {"cmd": command}
            }),
        );
        fixture.nested(
            REMOTE_TURN,
            "tool.result",
            json!({
                "call_id": id, "tool": "exec_command", "status": "completed",
                "duration_ns": 1, "result": {"output": output, "exit_code": 0}
            }),
        );
    }
    fixture.nested(
        REMOTE_TURN,
        "tool.result",
        json!({
            "call_id": "batch", "tool": "exec", "status": "completed",
            "duration_ns": 1, "result": null
        }),
    );
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("2 tools").await;
    fixture.terminal.wait_no_text("check-first").await;
    fixture.terminal.wait_no_text("check-second").await;

    fn click_row(terminal: &mut Terminal, text: &str) {
        let row = terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .lines()
            .position(|line| line.contains(text))
            .unwrap()
            + 1;
        terminal.input(&format!("\x1b[<0;2;{row}M\x1b[<0;2;{row}m"));
    }

    click_row(&mut fixture.terminal, "2 tools");
    fixture.terminal.wait_text("check-first").await;
    fixture.terminal.wait_text("check-second").await;
    fixture.terminal.wait_no_text("FIRST_CHILD_OUTPUT").await;
    fixture.terminal.wait_no_text("SECOND_CHILD_OUTPUT").await;
    let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
    assert!(
        screen
            .lines()
            .find(|line| line.contains("check-first"))
            .unwrap()
            .contains("├─")
    );

    click_row(&mut fixture.terminal, "check-first");
    fixture.terminal.wait_text("FIRST_CHILD_OUTPUT").await;
    fixture.terminal.wait_no_text("SECOND_CHILD_OUTPUT").await;
    click_row(&mut fixture.terminal, "2 tools");
    fixture.terminal.wait_no_text("check-first").await;
    fixture.terminal.wait_no_text("check-second").await;
    fixture.terminal.wait_no_text("FIRST_CHILD_OUTPUT").await;

    click_row(&mut fixture.terminal, "2 tools");
    fixture.terminal.wait_text("FIRST_CHILD_OUTPUT").await;
    fixture.terminal.wait_text("check-second").await;
    fixture.terminal.wait_no_text("SECOND_CHILD_OUTPUT").await;
    click_row(&mut fixture.terminal, "check-second");
    fixture.terminal.wait_text("SECOND_CHILD_OUTPUT").await;
    click_row(&mut fixture.terminal, "check-first");
    fixture.terminal.wait_no_text("FIRST_CHILD_OUTPUT").await;
    fixture.terminal.wait_text("SECOND_CHILD_OUTPUT").await;
}

async fn test_screen_socket(
    upgrade: WebSocketUpgrade,
    Query(query): Query<HashMap<String, String>>,
) -> axum::response::Response {
    assert_eq!(
        query.get("machine_id").map(String::as_str),
        Some("screen-test-hand")
    );
    assert_eq!(query.get("surface_id").map(String::as_str), Some("desktop"));
    assert_eq!(
        query.get("generation").map(String::as_str),
        Some("screen-generation")
    );
    upgrade.on_upgrade(|mut socket| async move {
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut bytes)
            .encode_image(&image::RgbImage::from_pixel(
                32,
                18,
                image::Rgb([40, 120, 200]),
            ))
            .unwrap();
        let jpeg = base64::engine::general_purpose::STANDARD.encode(bytes);
        socket
            .send(Message::Text(
                json!({"type":"ready", "connection_id":"screen-test-connection"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        while let Some(Ok(Message::Text(text))) = socket.recv().await {
            let message: Value = serde_json::from_str(&text).unwrap();
            if message["type"] == "ping" {
                if socket
                    .send(Message::Text(json!({"type":"pong"}).to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
                continue;
            }
            // Watching must never acquire the remote input lease.
            assert_eq!(message["type"], "frame_request");
            tokio::time::sleep(Duration::from_millis(16)).await;
            if socket
                .send(Message::Text(
                    json!({"type":"frame", "jpeg":jpeg,"width":32,"height":18})
                        .to_string()
                        .into(),
                ))
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

#[tokio::test]
async fn terminal_screen_selection_zoom_and_tabs_preserve_chat_draft() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("/screen", "\r");
    fixture.terminal.wait_text("Select Hand").await;
    fixture.terminal.wait_text("SCREEN_TEST_HAND").await;
    fixture.terminal.input("\r");
    fixture.terminal.wait_text("Watching").await;
    fixture.terminal.input("\t");
    fixture.terminal.prompt("DRAFT_WHILE_WATCHING", "");
    fixture.terminal.wait_text("DRAFT_WHILE_WATCHING").await;
    fixture.terminal.input("\t");
    fixture.terminal.prompt("/zoom", "\r");
    fixture.terminal.wait_text(": restore").await;
    fixture.terminal.wait_no_text("DRAFT_WHILE_WATCHING").await;
    fixture.terminal.input("\t");
    fixture.terminal.wait_text("DRAFT_WHILE_WATCHING").await;
    fixture.terminal.wait_no_text("Watching").await;
    fixture.terminal.input("\t\x1b");
    fixture.terminal.wait_text("DRAFT_WHILE_WATCHING").await;
    fixture.terminal.wait_no_text("Screen").await;
    assert!(fixture.submissions.try_recv().is_err());
    fixture.terminal.input("\r");
    let turn = fixture.submission("DRAFT_WHILE_WATCHING").await;
    fixture.complete(&turn);
}

#[tokio::test]
async fn terminal_vault_approval_cancel_then_explicit_approve_sends_one_safe_receipt() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.nested(REMOTE_TURN, "tool.call", json!({
        "call_id": "vault-approval", "tool": "request_vault_intake",
        "arguments": {"operation": "authorize_origin", "kind": "login", "vault_id": VAULT_ID, "origin": VAULT_ORIGIN}
    }));
    fixture.nested(REMOTE_TURN, "tool.result", json!({
        "call_id": "vault-approval", "tool": "request_vault_intake", "status": "completed", "duration_ns": 1,
        "result": {"type": "vault_intake", "status": "input_required", "operation": "authorize_origin",
            "kind": "login", "vault_id": VAULT_ID, "origin": VAULT_ORIGIN, "name": "UNVERIFIED_TOOL_LABEL"}
    }));
    fixture.terminal.wait_text("Approve Vault website").await;
    fixture.complete(REMOTE_TURN);
    assert!(fixture.vault_writes.lock().unwrap().is_empty());

    for approve in [false, true] {
        if approve {
            fixture.terminal.prompt("/vault", "\r");
        }
        fixture.terminal.wait_text("Approve Vault website").await;
        fixture.terminal.wait_text("VERIFIED_SAVED_LOGIN").await;
        fixture.terminal.wait_text(VAULT_ID).await;
        fixture.terminal.wait_text(VAULT_ORIGIN).await;
        fixture.terminal.wait_text("https://previous.example").await;
        fixture
            .terminal
            .wait_text("Press Ctrl+Enter to approve")
            .await;
        assert!(fixture.vault_writes.lock().unwrap().is_empty());
        assert!(fixture.submissions.try_recv().is_err());
        if approve {
            fixture.terminal.input("\x1b[13;5u");
        } else {
            fixture.terminal.input("\x1b");
            fixture.terminal.wait_no_text("Approve Vault website").await;
            fixture.terminal.wait_text("Enter send").await;
            assert!(fixture.vault_writes.lock().unwrap().is_empty());
            assert!(fixture.submissions.try_recv().is_err());
        }
    }
    let receipt = format!(
        "Vault website approval saved.\nLogin: VERIFIED_SAVED_LOGIN\nVault ID: {VAULT_ID}\nApproved website: {VAULT_ORIGIN}\nPassword stayed in Vault."
    );
    let turn = fixture.submission(&receipt).await;
    fixture.complete(&turn);
    fixture
        .terminal
        .wait_text("Vault website approval saved.")
        .await;
    fixture.terminal.wait_text("Enter send").await;
    assert_eq!(
        *fixture.vault_writes.lock().unwrap(),
        vec![json!({
            "id": VAULT_ID, "body": {"browser_origin": VAULT_ORIGIN}
        })]
    );
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());
    let output = fixture.terminal.output.lock().unwrap();
    let output = String::from_utf8_lossy(&output);
    for forbidden in [
        "PRIVATE_USERNAME_SENTINEL",
        "PRIVATE_PASSWORD_SENTINEL",
        "UNVERIFIED_TOOL_LABEL",
        "\"vault_intake\"",
        "\"input_required\"",
        "\"browser_origin\"",
    ] {
        assert!(
            !output.contains(forbidden),
            "unsafe/raw Vault output: {forbidden}"
        );
    }
}
