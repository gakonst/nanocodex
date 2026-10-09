//! Shipped CLI + real OS-owned Hand; only the remote managed service is a fixture.
#![cfg(unix)]

use axum::{
    Json, Router,
    body::Body,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{get, post},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    convert::Infallible,
    path::Path,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    process::Command,
    sync::{mpsc, oneshot},
};

const TIMEOUT: Duration = Duration::from_secs(30);
const OWNER: &str = "background-hand-journey-account";
const AGENT: &str = "019fc927-b280-79a7-8445-1b9996ad2fb0";
const TURN: &str = "019fc927-b281-79a7-8445-1b9996ad2fb0";

struct Call {
    frame: Value,
    result: oneshot::Sender<Value>,
}
#[derive(Clone)]
struct Cloud {
    calls: mpsc::UnboundedSender<Call>,
    receiver: Arc<Mutex<Option<mpsc::UnboundedReceiver<Call>>>>,
    catalog: Arc<Mutex<Option<Value>>>,
    origins: Arc<Mutex<Vec<Value>>>,
    account_connections: Arc<AtomicUsize>,
    agent_connections: Arc<AtomicUsize>,
    model_reads: Arc<AtomicUsize>,
}

fn credential() -> String {
    format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43))
}
fn authorize(headers: &HeaderMap) {
    assert_eq!(headers["authorization"], format!("Bearer {}", credential()));
}

async fn send(socket: &mut WebSocket, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}
async fn account_socket(
    State(state): State<Cloud>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    authorize(&headers);
    state.account_connections.fetch_add(1, Ordering::SeqCst);
    ws.on_upgrade(move |mut socket| async move {
        let mut calls = state.receiver.lock().unwrap().take().expect("a second publisher connected");
        let mut pending: Option<(String, oneshot::Sender<Value>)> = None;
        loop {
            tokio::select! {
                call = calls.recv(), if pending.is_none() => {
                    let Some(call) = call else { return };
                    let id = call.frame["call_id"].as_str().unwrap().to_owned();
                    pending = Some((id, call.result));
                    send(&mut socket, call.frame).await;
                }
                frame = socket.recv() => {
                    let Some(Ok(Message::Text(text))) = frame else { return };
                    let frame: Value = serde_json::from_str(&text).unwrap();
                    match frame["type"].as_str().unwrap() {
                        "catalog" => {
                            send(&mut socket, json!({"type":"ready"})).await;
                            *state.catalog.lock().unwrap() = Some(frame);
                        }
                        "ping" => send(&mut socket, json!({"type":"pong","nonce":frame["nonce"]})).await,
                        "diagnostic" => {},
                        "result" => {
                            let (id, result) = pending.take().expect("unsolicited result");
                            assert_eq!(frame["call_id"], id);
                            send(&mut socket, json!({"type":"ack","call_id":id})).await;
                            let _ = result.send(frame);
                        }
                        other => panic!("unexpected account frame {other}: {frame}"),
                    }
                }
            }
        }
    })
}

impl Cloud {
    async fn exec(&self, cwd: &str, proof: &str) {
        let (result, received) = oneshot::channel();
        self.calls.send(Call {
            frame: json!({"type":"call", "session_id":AGENT, "call_id":proof,
                "model":"gpt-6.1-sol", "name":"exec_command",
                "input":{"cmd":format!("pwd -P > {proof}; cat {proof}"), "workdir":cwd, "login":false},
                "output_token_budget":1024,"output_byte_budget":131072,"deadline_at":9_000_000_000_000_u64}),
            result,
        }).unwrap();
        let frame = tokio::time::timeout(TIMEOUT, received)
            .await
            .expect("real Hand exec timed out")
            .unwrap();
        assert_eq!(frame["outcome"]["status"], "completed", "{frame}");
        assert_eq!(frame["outcome"]["output"]["success"], true, "{frame}");
        assert!(
            frame["outcome"]["output"]["output"]
                .as_str()
                .unwrap()
                .contains(cwd),
            "{frame}"
        );
        assert_eq!(
            std::fs::read_to_string(Path::new(cwd).join(proof))
                .unwrap()
                .trim(),
            cwd
        );
        eprintln!("ACCOUNT WS real exec {proof}: {frame}");
    }
}

async fn run(State(state): State<Cloud>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    authorize(&headers);
    assert_eq!(headers["accept"], "text/event-stream");
    let context: Value =
        serde_json::from_str(headers["x-nanocodex-client-context"].to_str().unwrap()).unwrap();
    let machine = state.catalog.lock().unwrap().as_ref().unwrap()["machines"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(context["client"], "nanocodex2");
    assert_eq!(context["hand"], format!("user:{machine}"));
    assert_eq!(context["cwd"], format!("/{machine}"));
    state.origins.lock().unwrap().push(context.clone());
    let proof = body["input"].as_str().unwrap();
    assert!(matches!(proof, "project-a-proof" | "project-b-proof"));
    state
        .exec(context["native_cwd"].as_str().unwrap(), proof)
        .await;
    eprintln!("MANAGED admission: {context}");
    let receipt = json!({"agent_id":AGENT,"session_id":AGENT,"turn_id":TURN,
        "turn_idempotency_key":headers["idempotency-key"].to_str().unwrap(), "state":"accepted","input":proof,
        "accepted_cursor":"1","terminal_cursor":null,"created_at":1,"accepted_at":1,
        "updated_at":1,"attempt_count":1,"retry_at":null,"error":null,"terminal":null});
    let mut stream = format!("event: run\ndata: {receipt}\n\n");
    for (cursor, kind, payload) in [
        (
            2,
            "assistant.message",
            json!({"message":"BACKGROUND_HAND_OK"}),
        ),
        (3, "run.completed", json!({"status":"completed"})),
    ] {
        let event = json!({"cursor":cursor.to_string(),"created_at":cursor,"turn_id":TURN,"type":"event",
            "event":{"protocol_version":1,"request_id":"background-request","seq":cursor-1,"type":kind,"payload":payload}});
        stream.push_str(&format!("id: {cursor}\nevent: event\ndata: {event}\n\n"));
    }
    let terminal = json!({"cursor":"4","created_at":4,"turn_id":TURN,"type":"turn_completed","id":TURN,
        "final_message":"BACKGROUND_HAND_OK","usage":null,"citations":[],"usage_error":null});
    stream.push_str(&format!(
        "id: 4\nevent: turn_completed\ndata: {terminal}\n\n"
    ));
    Response::builder().status(StatusCode::CREATED).header("content-type","text/event-stream")
        .header("x-nanocodex-settings",json!({"model":"gpt-6.1-sol","thinking":"low","reasoning_mode":"standard","fast_mode":false}).to_string())
        .body(Body::from_stream(futures_util::stream::once(async move { Ok::<_, Infallible>(stream) }))).unwrap()
}

fn settings() -> Value {
    json!({"model":"gpt-6.1-sol","thinking":"low","reasoning_mode":"standard","fast_mode":false})
}
fn capabilities() -> Value {
    json!({"durable_turns":true,"resumable_events":true,"workspace":"private-hosted-tools-v1",
        "execution_environments":true,"execution_namespace":"cwd-root-v1","native_cross_mounts":false})
}
async fn tui_socket(
    State(state): State<Cloud>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    authorize(&headers);
    let context: Value =
        serde_json::from_str(headers["x-nanocodex-client-context"].to_str().unwrap()).unwrap();
    let machine = state.catalog.lock().unwrap().as_ref().unwrap()["machines"][0]["id"].clone();
    assert_eq!(
        context["hand"],
        format!("user:{}", machine.as_str().unwrap())
    );
    state.origins.lock().unwrap().push(context.clone());
    ws.on_upgrade(move |mut socket| async move {
        send(&mut socket, json!({"type":"ready","session_id":AGENT,"restored":true,"active_turns":[],
            "settings":settings(),"capabilities":capabilities(),"latest_event_cursor":"0"})).await;
        while let Some(Ok(Message::Text(frame))) = socket.recv().await {
            let frame: Value = serde_json::from_str(&frame).unwrap();
            if frame["type"] != "prompt" { continue; }
            assert_eq!(frame["input"][0]["text"], "tui-proof");
            let id = frame["id"].as_str().unwrap();
            send(&mut socket, json!({"type":"turn_accepted","id":id,"turn_id":id,"cursor":"1","input":frame["input"],"replayed":false})).await;
            state.exec(context["native_cwd"].as_str().unwrap(), "tui-proof").await;
            for (cursor, kind, payload) in [(2,"assistant.message",json!({"message":"TUI_BACKGROUND_HAND_OK"})),
                (3,"run.completed",json!({"status":"completed"}))] {
                send(&mut socket, json!({"type":"event","cursor":cursor.to_string(),"turn_id":id,
                    "event":{"protocol_version":1,"request_id":"tui-request","seq":cursor-1,"type":kind,"payload":payload}})).await;
            }
            send(&mut socket, json!({"type":"turn_completed","id":id,"turn_id":id,"cursor":"4",
                "final_message":"TUI_BACKGROUND_HAND_OK","usage":null,"citations":[],"usage_error":null})).await;
        }
    })
}

async fn terminal(home: &Path, origin: &str, cwd: &Path) {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::{Read, Write};
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 32,
            cols: 140,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_nanocodex"));
    command.env_clear();
    for (key, value) in [
        ("PATH", std::env::var("PATH").unwrap_or_default()),
        ("HOME", home.display().to_string()),
        ("CODEX_HOME", home.join(".codex").display().to_string()),
        ("NANOCODEX_HOME", home.display().to_string()),
        ("NANOCODEX_COMPUTER", "off".into()),
        ("NANOCODEX_MANAGED_URL", origin.into()),
        ("NC_API_KEY", credential()),
        ("TERM", "xterm-256color".into()),
    ] {
        command.env(key, value);
    }
    command.args(["attach", AGENT]);
    command.cwd(cwd);
    struct Child(Box<dyn portable_pty::Child + Send + Sync>);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
        }
    }
    let started = std::time::Instant::now();
    let mut child = Child(pair.slave.spawn_command(command).unwrap());
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let screen = Arc::new(Mutex::new(vt100::Parser::new(32, 140, 0)));
    let captured = screen.clone();
    std::thread::spawn(move || {
        let mut bytes = [0; 8192];
        while let Ok(count) = reader.read(&mut bytes) {
            if count == 0 {
                break;
            }
            captured.lock().unwrap().process(&bytes[..count]);
        }
    });
    tokio::time::timeout(TIMEOUT, async {
        loop {
            if screen
                .lock()
                .unwrap()
                .screen()
                .contents()
                .contains("Enter send")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("TUI composer did not become ready");
    let ready_ms = started.elapsed().as_millis();
    writer.write_all(b"tui-proof\r").unwrap();
    writer.flush().unwrap();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            if screen
                .lock()
                .unwrap()
                .screen()
                .contents()
                .contains("TUI_BACKGROUND_HAND_OK")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "TUI answer missing: {}",
            screen.lock().unwrap().screen().contents()
        )
    });
    let answer_ms = started.elapsed().as_millis();
    let before_close = std::time::Instant::now();
    writer.write_all(b"\x03\x03").unwrap();
    writer.flush().unwrap();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("TUI did not close");
    eprintln!(
        "TUI real PTY: ready_ms={ready_ms} answer_from_spawn_ms={answer_ms} close_ms={} screen={}",
        before_close.elapsed().as_millis(),
        screen.lock().unwrap().screen().contents()
    );
}

fn command(home: &Path, origin: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nanocodex"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("CODEX_HOME", home.join(".codex"))
        .env("NANOCODEX_HOME", home)
        .env("NC_API_KEY", credential())
        .env("NANOCODEX_MANAGED_URL", origin)
        .env("NANOCODEX_COMPUTER", "off")
        .current_dir(home)
        .kill_on_drop(true);
    command
}
fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value))
}

/// The shipped companion asks the real daemon over its real lease socket.
/// macOS consent itself is never exercised here: a test must not ask TCC on
/// behalf of the test runner, so the successful request runs where the daemon
/// needs no consent (Linux) and reports that rather than claiming a grant.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn permission_request_targets_only_the_running_daemon() {
    eprintln!(
        "Reproduce: cargo test -p nanocodex-bin --test nanocodex2_background_hand permission_request -- --nocapture"
    );
    let (calls, receiver) = mpsc::unbounded_channel();
    let state = Cloud {
        calls,
        receiver: Arc::new(Mutex::new(Some(receiver))),
        catalog: Arc::new(Mutex::new(None)),
        origins: Arc::new(Mutex::new(Vec::new())),
        account_connections: Arc::new(AtomicUsize::new(0)),
        agent_connections: Arc::new(AtomicUsize::new(0)),
        model_reads: Arc::new(AtomicUsize::new(0)),
    };
    let app = Router::new()
        .route(
            "/v1/me",
            get(|headers: HeaderMap| async move {
                authorize(&headers);
                Json(json!({"user":{"id":OWNER}}))
            }),
        )
        .route("/v1/account/tool-host", get(account_socket))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let temporary = tempfile::Builder::new()
        .prefix("nc-perm-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temporary.path().canonicalize().unwrap();
    let daemon_log = std::fs::File::create(home.join("daemon.log")).unwrap();
    let mut daemon = command(&home, &origin)
        .env("NANOCODEX_EXTERNAL_VM_FACTORY", "retained-fixture")
        .args(["hand"])
        .stdout(Stdio::from(daemon_log.try_clone().unwrap()))
        .stderr(Stdio::from(daemon_log))
        .spawn()
        .unwrap();
    let pid = daemon.id().unwrap();
    let status = home
        .join(".nanocodex/hands")
        .join(digest(&format!("{origin}\0{OWNER}")))
        .join("status.json");
    tokio::time::timeout(TIMEOUT, async {
        while !status.exists() || state.catalog.lock().unwrap().is_none() {
            assert!(daemon.try_wait().unwrap().is_none(), "daemon exited");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "daemon readiness timed out: {}",
            std::fs::read_to_string(home.join("daemon.log")).unwrap()
        )
    });

    // The CLI forwards `hand` to the Hand executable, which owns the daemon.
    let executable = Path::new(env!("CARGO_BIN_EXE_nanocodex-hand"));
    let request = |pid: u32| {
        let mut command = command(&home, &origin);
        command
            .args(["__device-hand", "--request-permissions", "--daemon-pid"])
            .arg(pid.to_string())
            .arg("--daemon-executable")
            .arg(executable);
        command
    };
    // A PID that is not the publisher is refused without contacting it.
    let other = std::process::id();
    let refused = tokio::time::timeout(TIMEOUT, request(other).output())
        .await
        .unwrap()
        .unwrap();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    eprintln!(
        "PERMISSIONS wrong pid {other}: status={} stderr={stderr}",
        refused.status
    );
    assert!(!refused.status.success());
    assert!(stderr.contains(&format!("PID {other}")), "{stderr}");
    if cfg!(target_os = "linux") {
        let output = tokio::time::timeout(TIMEOUT, request(pid).output())
            .await
            .unwrap()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        eprintln!(
            "PERMISSIONS daemon pid {pid}: status={} stdout={stdout}",
            output.status
        );
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let reply: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
        assert_eq!(reply["daemon"]["pid"], pid);
        assert_eq!(
            Path::new(reply["daemon"]["executable"].as_str().unwrap())
                .canonicalize()
                .unwrap(),
            executable.canonicalize().unwrap()
        );
        assert!(reply["permissions"]["unsupported"].is_string(), "{reply}");
    }
    assert!(
        daemon.try_wait().unwrap().is_none(),
        "permission request stopped daemon"
    );
    daemon.kill().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_daemon_survives_two_clients_and_routes_native_cwds() {
    eprintln!(
        "Reproduce: cargo test -p nanocodex-bin --test nanocodex2_background_hand -- --nocapture"
    );
    let (calls, receiver) = mpsc::unbounded_channel();
    let state = Cloud {
        calls,
        receiver: Arc::new(Mutex::new(Some(receiver))),
        catalog: Arc::new(Mutex::new(None)),
        origins: Arc::new(Mutex::new(Vec::new())),
        account_connections: Arc::new(AtomicUsize::new(0)),
        agent_connections: Arc::new(AtomicUsize::new(0)),
        model_reads: Arc::new(AtomicUsize::new(0)),
    };
    let app = Router::new()
        .route(
            "/v1/me",
            get(|headers: HeaderMap| async move {
                authorize(&headers);
                Json(json!({"user":{"id":OWNER}}))
            }),
        )
        .route("/v1/account/tool-host", get(account_socket))
        .route(
            "/v1/agents/{id}/tool-host",
            get(|State(state): State<Cloud>| async move {
                state.agent_connections.fetch_add(1, Ordering::SeqCst);
                StatusCode::NOT_FOUND
            }),
        )
        .route(
            "/v1/models",
            get(|State(state): State<Cloud>| async move {
                state.model_reads.fetch_add(1, Ordering::SeqCst);
                Json(json!({"object":"list","default_model":"gpt-6.1-sol","data":[
                    {"id":"gpt-6.1-sol","name":"Sol","provider":"openai","thinking":["low"],"fast_mode":true,"reasoning_modes":["standard"]}]}))
            }),
        )
        .route("/v1/agents/{id}", get(|| async { Json(json!({"agent_id":AGENT,"session_id":AGENT,
            "has_snapshot":false,"completed_turns":0,"last_active":1,"active_turns":[],"agent_loaded":false,
            "connected_clients":0,"capabilities":capabilities(),"settings":settings(),"latest_event_cursor":"0","stream_error":null})) }))
        .route("/v1/agents/{id}/events/history", get(|| async { Json(json!({"data":[],"has_more":false,"latest_cursor":"0"})) }))
        .route("/v1/agents/{id}/ws", get(tui_socket))
        .route("/v1/agent-runs", post(run))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    // A short canonical HOME keeps the real Unix socket under sockaddr_un limits.
    let temporary = tempfile::Builder::new()
        .prefix("nc-bg-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temporary.path().canonicalize().unwrap();
    let daemon_log = std::fs::File::create(home.join("daemon.log")).unwrap();
    let mut daemon = command(&home, &origin)
        .env("NANOCODEX_EXTERNAL_VM_FACTORY", "retained-fixture")
        .args(["hand"])
        .stdout(Stdio::from(daemon_log.try_clone().unwrap()))
        .stderr(Stdio::from(daemon_log))
        .spawn()
        .unwrap();
    let scope = digest(&format!("{origin}\0{OWNER}"));
    let ipc = home
        .join(".nanocodex/s")
        .join(format!("{}.sock", &scope[..24]));
    tokio::time::timeout(TIMEOUT, async {
        loop {
            assert!(
                daemon.try_wait().unwrap().is_none(),
                "daemon exited: {}",
                std::fs::read_to_string(home.join("daemon.log")).unwrap()
            );
            if state.catalog.lock().unwrap().is_some()
                && tokio::net::UnixStream::connect(&ipc).await.is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "daemon readiness timed out: {}",
            std::fs::read_to_string(home.join("daemon.log")).unwrap()
        )
    });
    assert!(
        state.catalog.lock().unwrap().as_ref().unwrap()["machines"][0]["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|capability| capability == "vm_factory:retained-fixture")
    );
    let identity_path = home
        .join(".nanocodex/hands")
        .join(scope)
        .join("identity.json");
    let identity = std::fs::read(&identity_path).unwrap();
    for (project, proof) in [
        ("project Α 🚀", "project-a-proof"),
        ("project B", "project-b-proof"),
    ] {
        let cwd = home.join(project);
        std::fs::create_dir(&cwd).unwrap();
        let output = tokio::time::timeout(
            TIMEOUT,
            command(&home, &origin)
                .args(["run", proof, "--model", "gpt-6.1-sol", "--thinking", "low"])
                .current_dir(&cwd)
                .output(),
        )
        .await
        .expect("CLI timed out")
        .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "CLI failed: {stdout}\n{stderr}");
        assert!(stdout.contains("BACKGROUND_HAND_OK"), "{stdout}\n{stderr}");
        assert!(!stderr.contains("Hand unavailable"), "{stderr}");
        assert_eq!(
            state.origins.lock().unwrap().last().unwrap()["native_cwd"],
            cwd.to_str().unwrap()
        );
        assert_eq!(std::fs::read(&identity_path).unwrap(), identity);
        assert!(
            daemon.try_wait().unwrap().is_none(),
            "client exit stopped daemon"
        );
        eprintln!("CLIENT {project} exited successfully: {stdout}\n{stderr}");
    }
    assert_eq!(state.model_reads.load(Ordering::SeqCst), 0);
    terminal(&home, &origin, &home.join("project B")).await;
    state
        .exec(
            home.join("project B").to_str().unwrap(),
            "after-clients-proof",
        )
        .await;
    assert!(daemon.try_wait().unwrap().is_none());
    assert_eq!(state.origins.lock().unwrap().len(), 3);
    assert_eq!(state.account_connections.load(Ordering::SeqCst), 1);
    assert_eq!(state.agent_connections.load(Ordering::SeqCst), 0);
    eprintln!(
        "PASS: stable identity; two native cwd proofs; one account connection; no agent publisher; post-client exec. Daemon log:\n{}",
        std::fs::read_to_string(home.join("daemon.log")).unwrap()
    );
    // Kill only the owned child, never a service manager or another installed Hand.
    daemon.kill().await.unwrap();
    daemon.wait().await.unwrap();
    server.abort();
}

// Only the remote account and optional external MCP provider are fixtures.
// The daemon, workspace process retention and account WebSocket are shipped code.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_computer_provider_preserves_daemon_and_running_shell() {
    use std::os::unix::fs::PermissionsExt;
    let (calls, receiver) = mpsc::unbounded_channel();
    let state = Cloud {
        calls,
        receiver: Arc::new(Mutex::new(Some(receiver))),
        catalog: Arc::new(Mutex::new(None)),
        origins: Arc::new(Mutex::new(Vec::new())),
        account_connections: Arc::new(AtomicUsize::new(0)),
        agent_connections: Arc::new(AtomicUsize::new(0)),
        model_reads: Arc::new(AtomicUsize::new(0)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route(
            "/v1/me",
            get(|| async { Json(json!({"user":{"id":OWNER}})) }),
        )
        .route("/v1/account/tool-host", get(account_socket))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let temporary = tempfile::Builder::new()
        .prefix("nc-cua-")
        .tempdir()
        .unwrap();
    let home = temporary.path().canonicalize().unwrap();
    let managed = home.join("runtimes/openai-cua");
    std::fs::create_dir_all(&managed).unwrap();
    let provider = home.join("provider");
    // A managed receipt may precede completion/recovery of its executable.
    std::fs::write(
        managed.join("provider.json"),
        json!({"status":"installed", "transport":"mcp",
        "executable":provider, "dependency_contract":"nanocodex-native-no-codex-v1"})
        .to_string(),
    )
    .unwrap();
    let log = std::fs::File::create(home.join("daemon.log")).unwrap();
    let mut daemon = command(&home, &origin)
        .env_remove("NANOCODEX_COMPUTER")
        .env("NANOCODEX_DIR", &home)
        .args(["hand"])
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    tokio::time::timeout(TIMEOUT, async {
        while state.catalog.lock().unwrap().is_none() {
            assert!(
                daemon.try_wait().unwrap().is_none(),
                "{}",
                std::fs::read_to_string(home.join("daemon.log")).unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let catalog = state.catalog.lock().unwrap().clone().unwrap();
    assert!(
        catalog["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["definition"]["name"] == "mcp__cua_repl__js")
    );
    let call = |id: &str, name: &str, input: Value| {
        let (result, received) = oneshot::channel();
        state.calls.send(Call { frame: json!({"type":"call","session_id":AGENT,"call_id":id,
            "model":"gpt-6.1-sol", "name":name,"input":input,
            "output_token_budget":4096,"output_byte_budget":131072,"deadline_at":9_000_000_000_000_u64}), result }).unwrap();
        async move {
            let frame = tokio::time::timeout(TIMEOUT, received)
                .await
                .unwrap()
                .unwrap();
            eprintln!("LATE PROVIDER call: {frame}");
            assert_eq!(frame["outcome"]["status"], "completed", "{frame}");
            assert_eq!(frame["outcome"]["output"]["success"], true, "{frame}");
            frame
        }
    };
    let preparing = call("preparing", "mcp__cua_repl__js", json!({})).await;
    let receipt = |frame: &Value| {
        serde_json::from_str::<Value>(
            frame["outcome"]["output"]["structured_result"]["content"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(receipt(&preparing)["status"], "preparing");
    let running = call("start-shell", "exec_command", json!({"cmd":"read answer; printf 'retained:%s' \"$answer\"", "workdir":home, "tty":true,"yield_time_ms":100,"login":false})).await;
    let session = running["outcome"]["output"]["structured_result"]["session_id"].clone();
    assert!(!session.is_null(), "{running}");
    std::fs::write(&provider, r#"#!/usr/bin/env python3
import sys,json
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 m=r['method']
 if m=='initialize': out={'protocolVersion':'2025-06-18','capabilities':{}}
 elif m=='tools/list': out={'tools':[{'name':'js','description':'Exact late provider documentation.','inputSchema':{'type':'object','required':['code'],'properties':{'code':{'type':'string'}}}},{'name':'js_reset','description':'Exact reset','inputSchema':{'type':'object'}}]}
 else:
  with open(__file__+'.calls','a') as f: f.write(json.dumps(r)+'\n')
  out={'content':[{'type':'text','text':json.dumps(r['params'])}]}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':out}),flush=True)
"#).unwrap();
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o700)).unwrap();
    let ready = call("ready", "mcp__cua_repl__js", json!({})).await;
    assert!(
        ready
            .to_string()
            .contains("Exact late provider documentation."),
        "{ready}"
    );
    assert_eq!(receipt(&ready)["status"], "ready");
    let definitions = receipt(&ready)["definitions"].as_array().unwrap().clone();
    assert_eq!(
        definitions
            .iter()
            .find(|tool| tool["name"] == "js")
            .unwrap()["parameters"]["required"],
        json!(["code"])
    );
    let action = call(
        "action",
        "mcp__cua_repl__js",
        json!({"code":"single-action"}),
    )
    .await;
    assert!(action.to_string().contains("single-action"), "{action}");
    let done = call(
        "finish-shell",
        "write_stdin",
        json!({"session_id":session,"chars":"ok\n","yield_time_ms":1000}),
    )
    .await;
    assert!(done.to_string().contains("retained:ok"), "{done}");
    assert_eq!(
        std::fs::read_to_string(provider.with_extension("calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(state.account_connections.load(Ordering::SeqCst), 1);
    assert!(daemon.try_wait().unwrap().is_none());
    eprintln!(
        "PASS: preparing -> exact catalog ready; one action dispatch; retained exec; one publisher connection.\n{}",
        std::fs::read_to_string(home.join("daemon.log")).unwrap()
    );
    daemon.kill().await.unwrap();
    daemon.wait().await.unwrap();
    server.abort();
}
