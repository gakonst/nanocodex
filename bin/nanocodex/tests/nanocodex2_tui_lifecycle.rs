//! Exercise real terminal input against a controlled managed service.

use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    routing::{get, post},
};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

const AGENT: &str = "019fc927-b280-79a7-8445-1b9996ad2fb0";
const REMOTE_TURN: &str = "019fc927-b281-79a7-8445-1b9996ad2fb0";
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
        std::thread::spawn(move || {
            let mut bytes = [0; 8192];
            while let Ok(count) = reader.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                captured.lock().unwrap().extend_from_slice(&bytes[..count]);
            }
        });
        Self {
            child,
            writer,
            output,
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
        tokio::time::timeout(TIMEOUT, async {
            loop {
                if String::from_utf8_lossy(&self.output.lock().unwrap()).contains(text) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            let output = self.output.lock().unwrap().clone();
            panic!(
                "terminal should render {text:?}: {:?}",
                String::from_utf8_lossy(&output[output.len().saturating_sub(4000)..])
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
    active: bool,
    history: Arc<Mutex<Vec<Value>>>,
    connected: mpsc::UnboundedSender<mpsc::UnboundedSender<Value>>,
    submitted: mpsc::UnboundedSender<Value>,
    steered: mpsc::UnboundedSender<(Value, oneshot::Sender<bool>)>,
    rejected: mpsc::UnboundedSender<Value>,
    cancelled: mpsc::UnboundedSender<String>,
}

async fn socket(
    State(service): State<Service>,
    upgrade: WebSocketUpgrade,
    Query(query): Query<HashMap<String, String>>,
) -> axum::response::Response {
    let cursor = query
        .get("cursor")
        .and_then(|cursor| cursor.parse().ok())
        .unwrap_or(0);
    upgrade.on_upgrade(move |socket| serve(socket, service, cursor))
}

async fn serve(mut socket: WebSocket, service: Service, cursor: u64) {
    let history = service.history.lock().unwrap().clone();
    let latest_cursor = history
        .last()
        .map_or("0", |event| event["cursor"].as_str().unwrap());
    let (outgoing, mut events) = mpsc::unbounded_channel::<Value>();
    let ready = json!({
        "type": "ready", "session_id": AGENT, "restored": false,
        "active_turns": if service.active { vec![REMOTE_TURN] } else { vec![] }, "active_turn_details": [], "latest_event_cursor": latest_cursor,
        "capabilities": {"durable_turns": true, "resumable_events": true,
            "live_steer": true, "live_cancel": true, "workspace": "cloudflare-computer",
            "execution_environments": true, "execution_namespace": "cwd-root-v1", "native_cross_mounts": false},
        "settings": {"model": "gpt-6-astra", "thinking": "low", "reasoning_mode": "standard", "fast_mode": false}
    });
    socket
        .send(Message::Text(ready.to_string().into()))
        .await
        .unwrap();
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
                        let message: Value = serde_json::from_str(&text).unwrap();
                        if message["type"] == "prompt" { let _ = service.submitted.send(message); }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

async fn state(State(service): State<Service>) -> Json<Value> {
    Json(json!({
        "agent_id": AGENT, "session_id": AGENT, "has_snapshot": false,
        "completed_turns": 0, "last_active": 1, "agent_loaded": true, "connected_clients": 1,
        "active_turns": if service.active { vec![REMOTE_TURN] } else { vec![] }, "active_turn_details": [],
        "capabilities": {"durable_turns": true, "resumable_events": true,
            "live_steer": true, "live_cancel": true, "workspace": "cloudflare-computer",
            "execution_environments": true, "execution_namespace": "cwd-root-v1", "native_cross_mounts": false},
        "settings": {"model": "gpt-6-astra", "thinking": "low", "reasoning_mode": "standard", "fast_mode": false},
        "latest_event_cursor": "0", "stream_error": null
    }))
}

async fn steer(
    State(service): State<Service>,
    axum::extract::Path((_, turn)): axum::extract::Path<(String, String)>,
    Json(input): Json<Value>,
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
    terminal: Terminal,
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
        let (connected, mut connections) = mpsc::unbounded_channel();
        let (submitted, submissions) = mpsc::unbounded_channel();
        let (steered, steers) = mpsc::unbounded_channel();
        let (rejected, rejections) = mpsc::unbounded_channel();
        let (cancelled, cancellations) = mpsc::unbounded_channel();
        let history = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/agents/live", get(socket))
            .route("/v1/agents/{agent}", get(state))
            .route("/v1/agents/{agent}/ws", get(socket))
            .route(
                "/v1/agents/{agent}/events",
                get(|| async {
                    Json(json!({"data": [], "has_more": false, "latest_cursor": "0"}))
                }),
            )
            .route("/v1/agents/{agent}/turns/{turn}/steer", post(steer))
            .route("/v1/agents/{agent}/turns/{turn}/cancel", post(cancel))
            .with_state(Service {
                active,
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
        let terminal = Terminal::start(&origin, active);
        terminal
            .wait_text(if active { "queue" } else { "actions" })
            .await;
        let events = tokio::time::timeout(TIMEOUT, connections.recv())
            .await
            .unwrap()
            .unwrap();
        Self {
            terminal,
            events,
            connections,
            history,
            submissions,
            steers,
            rejections,
            cancellations,
            server,
            cursor: 0,
        }
    }

    fn emit(&mut self, turn: &str, mut value: Value) {
        self.cursor += 1;
        value["cursor"] = json!(self.cursor.to_string());
        value["turn_id"] = json!(turn);
        self.history.lock().unwrap().push(value.clone());
        self.events.send(value).unwrap();
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
        self.nested(turn, "run.completed", json!({"status": "completed"}));
        self.emit(
            turn,
            json!({"type": "turn_completed", "id": turn,
            "final_message": "done", "usage": null, "citations": [], "usage_error": null}),
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
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
async fn terminal_interrupts_a_local_shell_and_accepts_the_next_prompt() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("!sleep 30", "\r");
    fixture.terminal.wait_text("Shell").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_text("Interrupt").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_text("cancelled by user").await;
    fixture
        .terminal
        .prompt("work after shell cancellation", "\r");
    let next = fixture.submission("<local_shell_result>\ncommand: sleep 30\noutcome: cancelled by user\noutput:\n\n</local_shell_result>\n\nwork after shell cancellation").await;
    fixture.complete(&next);
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
