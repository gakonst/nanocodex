// Fake-account CLI fixtures must not contact the host's installed Hand service.
use std::{
    collections::HashMap,
    convert::Infallible,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    body::Body,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, Response, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};

const AGENT_ID: &str = "019fc927-b280-79a7-8445-1b9996ad2fb0";
const TURN_ID: &str = "019fc927-b281-7a11-8445-1b9996ad2fb0";
const FIRST_TURN_ID: &str = "019fc927-b282-7a11-8445-1b9996ad2fb0";
const SECOND_TURN_ID: &str = "019fc927-b283-7a11-8445-1b9996ad2fb0";
const PROCESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[tokio::test]
async fn hand_help_exposes_the_vm_and_machine_contract() {
    let home = tempfile::tempdir().unwrap();
    let output = fixture_command(home.path())
        .env("NANOCODEX_DISABLE_HAND", "1")
        .args(["hand", "--help"])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Usage: nanocodex hand [OPTIONS]"),
        "{stdout}"
    );
    assert!(!stdout.contains("AGENT_ID"), "{stdout}");
    for expected in [
        "--vm <ROOTFS>",
        "--docker <IMAGE>",
        "--volume <VOLUME>",
        "--network <NETWORK>",
        "--runtime <RUNTIME>",
        "--guest-runtime <ELF>",
        "--workspace <PATH>",
        "--cpus <COUNT>",
        "--memory <MIB>",
        "--machine-id <MACHINE_ID>",
        "--machine-name <MACHINE_NAME>",
        "--log-filter <LOG_FILTER>",
        "--log-format <LOG_FORMAT>",
        "--log-file <LOG_FILE>",
        "--otel-endpoint <OTEL_ENDPOINT>",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
}

#[tokio::test]
async fn hand_rejects_mixed_options_and_removed_native_command() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        vec!["native-hand", "--workspace", "."],
        vec!["hand", "--docker", "image"],
        vec!["hand", "--volume", "work"],
        vec!["hand", "--docker", "image", "--volume", "work", "--gpu"],
        vec![
            "hand",
            "--docker",
            "image",
            "--volume",
            "work",
            "--network",
            "bogus",
        ],
        vec!["hand", "--vm", "root", "--runtime", "runsc"],
        vec![
            "hand",
            "--vm",
            "root.ext4",
            "--docker",
            "image",
            "--docker-volume",
            "work",
        ],
        vec!["hand", "--vm", "root.ext4", "--docker-internet"],
        vec![
            "hand",
            "--docker",
            "image",
            "--docker-volume",
            "work",
            "--vm-firmware",
            "/tmp/fw",
        ],
        vec![
            "hand",
            "--docker",
            "image",
            "--docker-volume",
            "work",
            "--docker-internet",
            "--vm-no-network",
        ],
    ] {
        let output = fixture_command(home.path())
            .env("NANOCODEX_DISABLE_HAND", "1")
            .args(&args)
            .env_remove("NANOCODEX_VM_GUEST_RUNTIME")
            .env_remove("NANOCODEX_KRUNFW_DIR")
            .output()
            .await
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[tokio::test]
async fn host_help_exposes_the_bounded_vm_pool_contract() {
    let home = tempfile::tempdir().unwrap();
    let output = fixture_command(home.path())
        .env("NANOCODEX_DISABLE_HAND", "1")
        .args(["host", "--help"])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(
            "Usage: nanocodex host [OPTIONS] --factory-name <FACTORY_NAME> --vm-template <ROOTFS> --state-dir <PATH> --vm-guest-runtime <ELF>"
        ),
        "{stdout}"
    );
    for expected in [
        "--scope <SCOPE>",
        "--agent <AGENT_ID>",
        "--factory-name <FACTORY_NAME>",
        "--vm-template <ROOTFS>",
        "--state-dir <PATH>",
        "--max-vms <COUNT>",
        "--host-id <UUID>",
        "--vm-guest-runtime <ELF>",
        "--vm-workspace <PATH>",
        "--vm-cpus <COUNT>",
        "--vm-memory-mib <MIB>",
        "--log-filter <LOG_FILTER>",
        "--otel-endpoint <OTEL_ENDPOINT>",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
    assert!(stdout.contains("[default: user]"), "{stdout}");
    assert!(
        stdout.contains("possible values: user, agent, system"),
        "{stdout}"
    );
}

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[tokio::test]
async fn hand_json_tracing_exposes_resources_without_paths_or_credentials() {
    let api_key = format!("ncx_live_{}_{}", "7".repeat(12), "8".repeat(43));
    let root_parent = tempfile::tempdir().unwrap();
    let missing_root = root_parent.path().join("private-root-sentinel.ext4");
    let output = fixture_command(root_parent.path())
        .env("NANOCODEX_DISABLE_HAND", "1")
        .args([
            "hand",
            "--vm",
            missing_root.to_str().unwrap(),
            "--machine-id",
            "trace-hand",
            "--machine-name",
            "private-name-sentinel",
            "--vm-cpus",
            "24",
            "--vm-memory-mib",
            "98304",
            "--log-format",
            "json",
            "--log-filter",
            "warn,nanocodex2=info,nanocodex_oai_tools::attachment=info",
        ])
        .env("NANOCODEX_MANAGED_URL", "http://127.0.0.1:9")
        .env("NC_API_KEY", &api_key)
        .env_remove("NANOCODEX_API_KEY")
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    for secret in [api_key.as_str(), "private-name-sentinel"] {
        assert!(
            !stderr.contains(secret),
            "stderr leaked {secret:?}: {stderr}"
        );
    }
    let traces = stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>();
    assert!(!traces.is_empty(), "{stderr}");
    let encoded = serde_json::to_string(&traces).unwrap();
    assert!(encoded.contains("hand.preflight"), "{encoded}");
    assert!(encoded.contains("failed"), "{encoded}");
    for expected in ["trace-hand", "24", "98304", "missing"] {
        assert!(
            encoded.contains(expected),
            "missing {expected:?} in {encoded}"
        );
    }
    for secret in [
        api_key.as_str(),
        missing_root.to_str().unwrap(),
        "private-root-sentinel",
        "private-name-sentinel",
    ] {
        assert!(
            !encoded.contains(secret),
            "trace leaked {secret:?}: {encoded}"
        );
    }
}

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[tokio::test]
async fn vm_child_entrypoint_does_not_require_managed_credentials() {
    let home = tempfile::tempdir().unwrap();
    let missing = tempfile::tempdir()
        .unwrap()
        .path()
        .join("missing-launch-record");
    let output = fixture_command(home.path())
        .env("NANOCODEX_DISABLE_HAND", "1")
        .args(["__vm-run-config", "--config"])
        .arg(missing)
        .env_remove("NANOCODEX_API_KEY")
        .env_remove("NC_API_KEY")
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("failed to read VM launch record"),
        "{stderr}"
    );
    assert!(!stderr.contains("must be set"), "{stderr}");
}

#[derive(Clone)]
struct TestState {
    authorization: String,
    idempotency_key: &'static str,
    authorized_requests: Arc<AtomicUsize>,
    tool_host_attempts: Arc<AtomicUsize>,
    completed: Arc<tokio::sync::Notify>,
    origin: String,
    expect_local_tool: bool,
    catalogs: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[tokio::test]
async fn run_flushes_each_assistant_delta_before_completion() {
    use tokio::io::AsyncBufReadExt;

    let next = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::clone(&next);
    let app = Router::new()
        .route("/v1/models", fixture_catalog(format!("Bearer ncx_live_{}_{}", "a".repeat(12), "b".repeat(43))))
        .route("/v1/agent-runs", post(move |headers: HeaderMap, axum::Json(body): axum::Json<serde_json::Value>| {
            let gate = gate.clone();
            async move {
                assert_eq!(headers["idempotency-key"], "stream-request");
                assert_eq!(body["input"], "stream answer");
                let stream = futures_util::stream::unfold(0, move |step| {
                    let gate = gate.clone();
                    async move {
                        let chunk = match step {
                            0 => combined_receipt("stream answer"),
                            1 | 2 => {
                                if step == 2 { gate.notified().await; }
                                let event = serde_json::json!({"cursor": (step+1).to_string(), "created_at": step+1, "turn_id": TURN_ID,
                                    "type": "event", "event": {"protocol_version": 1, "request_id": "server-request", "seq": step,
                                    "type": "assistant.delta", "payload": {"model_call_index": 1, "item_id": "answer", "phase": "final_answer",
                                    "text": if step == 1 { "first" } else { " second" }}}});
                                format!("id: {}\nevent: event\ndata: {event}\n\n", step+1)
                            }
                            3 => { gate.notified().await; durable_turn_events(TURN_ID, "first second", 4, 3) }
                            _ => return None,
                        };
                        Some((Ok::<_, Infallible>(chunk), step+1))
                    }
                });
                Response::builder().status(StatusCode::CREATED).header("content-type", "text/event-stream")
        .header("x-nanocodex-settings", agent_settings().to_string())
                    .body(Body::from_stream(stream)).unwrap()
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let workspace = tempfile::tempdir().unwrap();
    let (config_home, decoy) = configure_workspace(workspace.path());
    let mut child = fixture_command(config_home.path())
        .env("NANOCODEX_DISABLE_HAND", "1")
        .args([
            "run",
            "stream answer",
            "--idempotency-key",
            "stream-request",
        ])
        .env("NANOCODEX_MANAGED_URL", origin)
        .env(
            "NC_API_KEY",
            format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)),
        )
        .env_remove("NANOCODEX_API_KEY")
        .env("NANOCODEX_HOME", config_home.path())
        .env_remove("OPENAI_API_KEY")
        .current_dir(decoy.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    for expected in ["first", " second"] {
        let line = tokio::time::timeout(PROCESS_TIMEOUT, lines.next_line())
            .await
            .expect("assistant delta was buffered until completion")
            .unwrap()
            .unwrap();
        let event: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(event["type"], "assistant.delta");
        assert_eq!(event["payload"]["text"], expected);
        assert!(child.try_wait().unwrap().is_none());
        next.notify_one();
    }
    let output = tokio::time::timeout(PROCESS_TIMEOUT, child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut remaining = Vec::new();
    while let Some(line) = lines.next_line().await.unwrap() {
        remaining.push(serde_json::from_str::<serde_json::Value>(&line).unwrap());
    }
    assert_eq!(remaining.len(), 2);
    assert_eq!(remaining[0]["type"], "assistant.message");
    assert_eq!(remaining[1]["type"], "run.completed");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("first second")
            .count(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn run_streams_cloud_answer_with_configured_directory_context() {
    run_workspace_lifecycle(false, false).await;
}

#[tokio::test]
async fn pinned_run_combines_creation_and_first_prompt() {
    run_workspace_lifecycle(true, false).await;
}

#[tokio::test]
async fn run_does_not_discover_an_explicit_missing_computer_provider() {
    // Provider selection belongs to the background daemon, not the CLI agent.
    run_workspace_lifecycle(false, true).await;
}

#[cfg(unix)]
#[tokio::test]
async fn tui_does_not_discover_an_explicit_missing_computer_provider() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::{Read, Write};

    let home = tempfile::tempdir().unwrap();
    let api_key = format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43));
    let authorization = format!("Bearer {api_key}");
    let attempts = Arc::new(AtomicUsize::new(0));
    let observed_attempts = attempts.clone();
    let connected = Arc::new(tokio::sync::Notify::new());
    let observed_connected = connected.clone();
    let app = Router::new()
        .route("/v1/models", fixture_catalog(authorization.clone()))
        .route(
            "/v1/agents/{agent}",
            get(move |headers: HeaderMap| {
                let authorization = authorization.clone();
                async move {
                    assert_eq!(headers["authorization"], authorization);
                    json_response(StatusCode::OK, agent_state_value("0"))
                }
            }),
        )
        .route(
            "/v1/agents/{agent}/events/history",
            get(|| async {
                axum::Json(serde_json::json!({"data": [], "has_more": false, "latest_cursor": "0"}))
            }),
        )
        .route(
            "/v1/agents/{agent}/tool-host",
            get(move || {
                observed_attempts.fetch_add(1, Ordering::SeqCst);
                async { StatusCode::NOT_FOUND }
            }),
        )
        .route(
            "/v1/agents/{agent}/ws",
            get(move |upgrade: WebSocketUpgrade| {
                let connected = observed_connected.clone();
                async move {
                    upgrade.on_upgrade(move |mut socket| async move {
                        send_ready(&mut socket, "0", false).await;
                        connected.notify_one();
                        while let Some(Ok(Message::Text(frame))) = socket.recv().await {
                            let frame: serde_json::Value = serde_json::from_str(&frame).unwrap();
                            if frame["type"] != "prompt" {
                                continue;
                            }
                            assert_eq!(frame["input"][0]["text"], "provider ownership probe");
                            let id = frame["id"].as_str().unwrap();
                            send_accepted(&mut socket, id, "provider ownership probe", 1).await;
                            send_turn_messages(
                                &mut socket,
                                id,
                                "cloud provider ownership answer",
                                2,
                                1,
                            )
                            .await;
                        }
                    })
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 30,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_nanocodex"));
    command.env_clear();
    command.env("PATH", std::env::var_os("PATH").unwrap_or_default());
    command.env("HOME", home.path());
    command.env("CODEX_HOME", home.path().join(".codex"));
    command.env("NANOCODEX_HOME", home.path());
    command.env("NANOCODEX_DISABLE_HAND", "1");
    command.env("NANOCODEX_COMPUTER", home.path().join("missing-provider"));
    command.env("NANOCODEX_MANAGED_URL", origin);
    command.env("NC_API_KEY", &api_key);
    command.env("TERM", "xterm-256color");
    command.args(["attach", AGENT_ID]);
    command.cwd(home.path());
    struct ChildGuard(Box<dyn portable_pty::Child + Send + Sync>);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
        }
    }
    let _child = ChildGuard(pair.slave.spawn_command(command).unwrap());
    drop(pair.slave);
    let mut writer = pair.master.take_writer().unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let transcript = Arc::new(Mutex::new(Vec::new()));
    let captured = transcript.clone();
    std::thread::spawn(move || {
        let mut bytes = [0; 8192];
        while let Ok(count) = reader.read(&mut bytes) {
            if count == 0 {
                break;
            }
            captured.lock().unwrap().extend_from_slice(&bytes[..count]);
        }
    });
    tokio::time::timeout(PROCESS_TIMEOUT, async {
        connected.notified().await;
        writer.write_all(b"provider ownership probe\r").unwrap();
        writer.flush().unwrap();
        loop {
            if String::from_utf8_lossy(&transcript.lock().unwrap())
                .contains("cloud provider ownership answer")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "TUI did not answer: {}",
            String::from_utf8_lossy(&transcript.lock().unwrap())
        )
    });
    assert_eq!(attempts.load(Ordering::SeqCst), 0);
    let transcript = String::from_utf8_lossy(&transcript.lock().unwrap()).into_owned();
    assert!(!transcript.contains(&api_key));
    eprintln!(
        "JOURNEY TUI attach with missing provider: cloud answer rendered, zero per-agent publishers\n{transcript}"
    );
    server.abort();
}

async fn run_workspace_lifecycle(pinned: bool, missing_computer: bool) {
    let workspace = tempfile::tempdir().unwrap();
    let expected_workspace = workspace.path().to_string_lossy().into_owned();
    let idempotency_key = "stable-request";
    let api_key = format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = TestState {
        authorization: format!("Bearer {api_key}"),
        idempotency_key,
        authorized_requests: Arc::new(AtomicUsize::new(0)),
        tool_host_attempts: Arc::new(AtomicUsize::new(0)),
        completed: Arc::new(tokio::sync::Notify::new()),
        origin: format!("http://{address}"),
        expect_local_tool: false,
        catalogs: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/v1/models", fixture_catalog(state.authorization.clone()))
        .route(
            "/v1/agent-runs",
            post(
                move |State(state): State<TestState>,
                      headers: HeaderMap,
                      axum::Json(body): axum::Json<serde_json::Value>| {
                    let expected_workspace = expected_workspace.clone();
                    async move {
                        let context: serde_json::Value = serde_json::from_str(
                            headers["x-nanocodex-client-context"].to_str().unwrap(),
                        )
                        .unwrap();
                        assert_eq!(
                            context,
                            serde_json::json!({
                                "client": "nanocodex2", "native_cwd": expected_workspace
                            })
                        );
                        assert_eq!(
                            body["configuration"]["chatgpt_account_id"].as_str(),
                            pinned.then_some("account-a")
                        );
                        combined_cli_run(State(state), headers, axum::Json(body)).await
                    }
                },
            ),
        )
        .route("/v1/agents/{agent}", get(agent_state))
        .route("/v1/agents/{agent}/tool-host", get(tool_host))
        .route("/v1/agents/{agent}/ws", get(managed_socket))
        .route("/v1/agents/{agent}/turns", post(submit_turn))
        .route("/v1/agents/{agent}/events", get(events))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (config_home, decoy) = configure_workspace(workspace.path());
    let mut command = fixture_command(config_home.path());
    if missing_computer {
        command.env(
            "NANOCODEX_COMPUTER",
            config_home.path().join("missing-provider"),
        );
    }
    let output = tokio::time::timeout(
        PROCESS_TIMEOUT,
        command
            .env("NANOCODEX_DISABLE_HAND", "1")
            .args([
                "run",
                "answer from managed",
                "--idempotency-key",
                idempotency_key,
            ])
            .args(if pinned {
                vec!["--chatgpt-account", "account-a"]
            } else {
                vec![]
            })
            .env("NANOCODEX_MANAGED_URL", &state.origin)
            .env("NC_API_KEY", &api_key)
            .env_remove("NANOCODEX_API_KEY")
            .env("NANOCODEX_HOME", config_home.path())
            .env_remove("OPENAI_API_KEY")
            .current_dir(decoy.path())
            .output(),
    )
    .await
    .expect("nanocodex2 managed lifecycle timed out")
    .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    let agent_event: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(agent_event["type"], "assistant.message");
    let terminal_event: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(terminal_event["type"], "run.completed");
    assert_eq!(
        stderr,
        format!("Managed agent: {AGENT_ID}\nmanaged answer\n")
    );
    assert!(!stdout.contains(&api_key));
    assert!(!stderr.contains(&api_key));
    assert_eq!(state.authorized_requests.load(Ordering::SeqCst), 1);
    assert_eq!(state.tool_host_attempts.load(Ordering::SeqCst), 0);
    assert!(state.catalogs.lock().unwrap().is_empty());
    assert!(!workspace.path().join("hosted-proof.txt").exists());
    assert!(!decoy.path().join("hosted-proof.txt").exists());
    assert!(!config_home.path().join("host-id").exists());
    assert!(!config_home.path().join("attachment-id").exists());
    eprintln!(
        "JOURNEY cloud-only run: pinned={pinned} missing_provider={missing_computer}; streamed answer; zero tool-host connections/catalogs"
    );
    server.abort();
}

#[tokio::test]
async fn run_rejects_a_malformed_combined_receipt() {
    let api_key = format!("ncx_live_{}_{}", "1".repeat(12), "2".repeat(43));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = TestState {
        authorization: format!("Bearer {api_key}"),
        idempotency_key: "unused",
        authorized_requests: Arc::new(AtomicUsize::new(0)),
        tool_host_attempts: Arc::new(AtomicUsize::new(0)),
        completed: Arc::new(tokio::sync::Notify::new()),
        origin: format!("http://{address}"),
        expect_local_tool: false,
        catalogs: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/v1/models", fixture_catalog(state.authorization.clone()))
        .route(
            "/v1/agent-runs",
            post(
                |State(state): State<TestState>, headers: HeaderMap| async move {
                    assert!(authorized(&state, &headers));
                    sse_response(async {
                        "event: run\ndata: {\"agent_id\":\"wrong-agent\"}\n\n".to_owned()
                    })
                },
            ),
        )
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let workspace = tempfile::tempdir().unwrap();
    let (config_home, decoy) = configure_workspace(workspace.path());

    let output = tokio::time::timeout(
        PROCESS_TIMEOUT,
        fixture_command(config_home.path())
            .env("NANOCODEX_DISABLE_HAND", "1")
            .args(["run", "this turn must not submit"])
            .env("NANOCODEX_COMPUTER", "off")
            .env("NANOCODEX_MANAGED_URL", &state.origin)
            .env("NC_API_KEY", &api_key)
            .env_remove("NANOCODEX_API_KEY")
            .env("NANOCODEX_HOME", config_home.path())
            .env_remove("OPENAI_API_KEY")
            .current_dir(decoy.path())
            .output(),
    )
    .await
    .expect("nanocodex2 failed-open lifecycle timed out")
    .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("Error: "));
    assert!(!stderr.contains(&api_key));
    assert_eq!(state.authorized_requests.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn run_reopens_one_durable_cloud_agent_without_publishing_local_tools() {
    let api_key = format!("ncx_live_{}_{}", "e".repeat(12), "f".repeat(43));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = DurableState {
        authorization: format!("Bearer {api_key}"),
        origin: format!("http://{address}"),
        creates: Arc::new(AtomicUsize::new(0)),
        state_reads: Arc::new(Mutex::new(Vec::new())),
        event_cursors: Arc::new(Mutex::new(Vec::new())),
        submissions: Arc::new(Mutex::new(Vec::new())),
        tool_host_attempts: Arc::new(AtomicUsize::new(0)),
        changed: Arc::new(tokio::sync::Notify::new()),
    };
    let app = Router::new()
        .route("/v1/models", fixture_catalog(state.authorization.clone()))
        .route(
            "/v1/agent-runs",
            post(
                |State(state): State<DurableState>,
                 headers: HeaderMap,
                 axum::Json(body): axum::Json<serde_json::Value>| async move {
                    assert!(durable_authorized(&state, &headers));
                    assert_eq!(headers["idempotency-key"], "durable-turn-one");
                    assert_eq!(body["input"], "first durable turn");
                    state.creates.fetch_add(1, Ordering::SeqCst);
                    state
                        .submissions
                        .lock()
                        .unwrap()
                        .push(("durable-turn-one".into(), "first durable turn".into()));
                    combined_stream("first durable turn", async move {
                        durable_turn_events(TURN_ID, "first cloud answer", 2, 1)
                    })
                },
            ),
        )
        .route("/v1/agents/{agent}", get(durable_agent_state))
        .route("/v1/agents/{agent}/tool-host", get(durable_tool_host))
        .route("/v1/agents/{agent}/ws", get(durable_socket))
        .route("/v1/agents/{agent}/turns", post(durable_submit_turn))
        .route("/v1/agents/{agent}/events", get(durable_events))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("fixture.txt"), "durable workspace\n").unwrap();
    let (config_home, first_decoy) = configure_workspace(workspace.path());
    let second_decoy = tempfile::tempdir().unwrap();
    std::fs::write(
        first_decoy.path().join("hosted-proof.txt"),
        "first decoy sentinel\n",
    )
    .unwrap();
    std::fs::write(
        second_decoy.path().join("hosted-proof.txt"),
        "second decoy sentinel\n",
    )
    .unwrap();

    let first = tokio::time::timeout(
        PROCESS_TIMEOUT,
        fixture_command(config_home.path())
            .env("NANOCODEX_DISABLE_HAND", "1")
            .args([
                "run",
                "first durable turn",
                "--idempotency-key",
                "durable-turn-one",
            ])
            .env("NANOCODEX_COMPUTER", "off")
            .env("NANOCODEX_MANAGED_URL", &state.origin)
            .env("NC_API_KEY", &api_key)
            .env_remove("NANOCODEX_API_KEY")
            .env("NANOCODEX_HOME", config_home.path())
            .env_remove("OPENAI_API_KEY")
            .current_dir(first_decoy.path())
            .output(),
    )
    .await
    .expect("first nanocodex2 process timed out")
    .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_process_events(
        &first.stdout,
        &[("assistant.message", 1), ("run.completed", 2)],
    );
    assert_eq!(
        String::from_utf8(first.stderr).unwrap(),
        format!("Managed agent: {AGENT_ID}\nfirst cloud answer\n")
    );
    assert!(!workspace.path().join("hosted-proof.txt").exists());

    let second = tokio::time::timeout(
        PROCESS_TIMEOUT,
        fixture_command(config_home.path())
            .env("NANOCODEX_DISABLE_HAND", "1")
            .args([
                "run",
                "second durable turn",
                "--agent",
                AGENT_ID,
                "--idempotency-key",
                "durable-turn-two",
            ])
            .env("NANOCODEX_COMPUTER", "off")
            .env("NANOCODEX_MANAGED_URL", &state.origin)
            .env("NC_API_KEY", &api_key)
            .env_remove("NANOCODEX_API_KEY")
            .env("NANOCODEX_HOME", config_home.path())
            .env_remove("OPENAI_API_KEY")
            .current_dir(second_decoy.path())
            .output(),
    )
    .await
    .expect("reopened nanocodex2 process timed out")
    .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_process_events(
        &second.stdout,
        &[("assistant.message", 1), ("run.completed", 2)],
    );
    assert_eq!(
        String::from_utf8(second.stderr.clone()).unwrap(),
        "second cloud answer\n"
    );

    assert_eq!(state.creates.load(Ordering::SeqCst), 1);
    assert_eq!(state.state_reads.lock().unwrap().as_slice(), [AGENT_ID]);
    let cursors = state.event_cursors.lock().unwrap();
    assert_eq!(cursors.first().map(String::as_str), Some("3"));
    assert!(cursors.iter().any(|cursor| cursor == "3"), "{cursors:?}");
    assert!(cursors.iter().all(|cursor| cursor == "0" || cursor == "3"));
    drop(cursors);
    let submissions = state.submissions.lock().unwrap();
    assert_eq!(
        submissions.as_slice(),
        [
            (
                "durable-turn-one".to_owned(),
                "first durable turn".to_owned()
            ),
            (
                "durable-turn-two".to_owned(),
                "second durable turn".to_owned()
            ),
        ]
    );
    drop(submissions);
    assert_eq!(state.tool_host_attempts.load(Ordering::SeqCst), 0);
    assert!(!workspace.path().join("hosted-proof.txt").exists());
    assert_eq!(
        std::fs::read_to_string(first_decoy.path().join("hosted-proof.txt")).unwrap(),
        "first decoy sentinel\n",
    );
    assert_eq!(
        std::fs::read_to_string(second_decoy.path().join("hosted-proof.txt")).unwrap(),
        "second decoy sentinel\n",
    );
    for bytes in [&first.stdout, &second.stdout, &second.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains(&api_key));
    }
    server.abort();
}

#[derive(Clone)]
struct DurableState {
    authorization: String,
    origin: String,
    creates: Arc<AtomicUsize>,
    state_reads: Arc<Mutex<Vec<String>>>,
    event_cursors: Arc<Mutex<Vec<String>>>,
    submissions: Arc<Mutex<Vec<(String, String)>>>,
    tool_host_attempts: Arc<AtomicUsize>,
    changed: Arc<tokio::sync::Notify>,
}

async fn durable_agent_state(
    State(state): State<DurableState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !durable_authorized(&state, &headers) {
        return unauthorized();
    }
    assert_eq!(agent, AGENT_ID);
    let latest_event_cursor = {
        let mut reads = state.state_reads.lock().unwrap();
        reads.push(agent);
        "3"
    };
    json_response(StatusCode::OK, agent_state_value(latest_event_cursor))
}

fn agent_state_value(latest_event_cursor: &str) -> serde_json::Value {
    serde_json::json!({
        "agent_id": AGENT_ID,
        "session_id": AGENT_ID,
        "has_snapshot": latest_event_cursor != "0",
        "completed_turns": usize::from(latest_event_cursor != "0"),
        "last_active": 1,
        "active_turns": [],
        "agent_loaded": latest_event_cursor != "0",
        "connected_clients": 0,
        "capabilities": {
            "durable_turns": true,
            "resumable_events": true,
            "workspace": "private-hosted-tools-v1",
            "execution_environments": true,
            "execution_namespace": "cwd-root-v1",
            "native_cross_mounts": false
        },
        "settings": agent_settings(),
        "latest_event_cursor": latest_event_cursor,
        "stream_error": null
    })
}

async fn durable_tool_host(State(state): State<DurableState>) -> StatusCode {
    state.tool_host_attempts.fetch_add(1, Ordering::SeqCst);
    StatusCode::NOT_FOUND
}

async fn durable_socket(
    State(state): State<DurableState>,
    Path(agent): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    if !durable_authorized(&state, &headers) {
        return unauthorized();
    }
    assert_eq!(agent, AGENT_ID);
    let cursor = query.get("cursor").cloned().unwrap();
    state.event_cursors.lock().unwrap().push(cursor.clone());
    upgrade
        .on_upgrade(move |socket| serve_durable_socket(socket, state, cursor))
        .into_response()
}

async fn serve_durable_socket(mut socket: WebSocket, state: DurableState, cursor: String) {
    send_ready(&mut socket, &cursor, cursor != "0").await;
    let Some(Ok(Message::Text(prompt))) = socket.recv().await else {
        return;
    };
    let prompt: serde_json::Value = serde_json::from_str(&prompt).unwrap();
    assert_eq!(prompt["type"], "prompt");
    let key = prompt["id"].as_str().unwrap().to_owned();
    let input = prompt["input"].as_str().unwrap().to_owned();
    let index = {
        let mut submissions = state.submissions.lock().unwrap();
        submissions.push((key.clone(), input.clone()));
        submissions.len()
    };
    let (answer, first_cursor, sequence) = match (index, key.as_str(), input.as_str()) {
        (1, "durable-turn-one", "first durable turn") => ("first cloud answer", 1, 1),
        (2, "durable-turn-two", "second durable turn") => ("second cloud answer", 4, 3),
        unexpected => panic!("unexpected durable socket submission: {unexpected:?}"),
    };
    state.changed.notify_waiters();
    send_accepted(&mut socket, &key, &input, first_cursor).await;
    send_turn_messages(&mut socket, &key, answer, first_cursor + 1, sequence).await;
}

async fn durable_submit_turn(
    State(state): State<DurableState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    if !durable_authorized(&state, &headers) {
        return unauthorized();
    }
    assert_eq!(agent, AGENT_ID);
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_owned();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        body.get("id").is_none(),
        "client supplied a turn ID: {body}"
    );
    let prompt = body["input"].as_str().unwrap().to_owned();
    let index = {
        let mut submissions = state.submissions.lock().unwrap();
        submissions.push((key.clone(), prompt.clone()));
        submissions.len()
    };
    let (turn_id, accepted_cursor) = match (index, key.as_str(), prompt.as_str()) {
        (1, "durable-turn-one", "first durable turn") => (FIRST_TURN_ID, "1"),
        (2, "durable-turn-two", "second durable turn") => (SECOND_TURN_ID, "4"),
        unexpected => panic!("unexpected durable submission: {unexpected:?}"),
    };
    state.changed.notify_waiters();
    json_response(
        StatusCode::ACCEPTED,
        serde_json::json!({
            "turn_id": turn_id,
            "state": "accepted",
            "input": prompt,
            "accepted_cursor": accepted_cursor,
            "terminal_cursor": null,
            "created_at": index,
            "accepted_at": index,
            "updated_at": index,
            "attempt_count": 0,
            "retry_at": null,
            "error": null,
            "terminal": null,
        }),
    )
}

async fn durable_events(
    State(state): State<DurableState>,
    Path(agent): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !durable_authorized(&state, &headers) {
        return unauthorized();
    }
    assert_eq!(agent, AGENT_ID);
    let cursor = query.get("cursor").cloned().unwrap();
    state.event_cursors.lock().unwrap().push(cursor.clone());
    match cursor.as_str() {
        "0" => sse_response(async move {
            wait_for_durable_state(&state, || !state.submissions.lock().unwrap().is_empty()).await;
            durable_turn_events(FIRST_TURN_ID, "first cloud answer", 1, 1)
        }),
        "3" => sse_response(async move {
            wait_for_durable_state(&state, || state.submissions.lock().unwrap().len() >= 2).await;
            durable_turn_events(SECOND_TURN_ID, "second cloud answer", 4, 3)
        }),
        other => panic!("unexpected durable event cursor {other}"),
    }
}

fn sse_response<F>(body: F) -> Response<Body>
where
    F: Future<Output = String> + Send + 'static,
{
    let body = Body::from_stream(futures_util::stream::once(async move {
        Ok::<_, Infallible>(body.await)
    }));
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("x-nanocodex-settings", agent_settings().to_string())
        .body(body)
        .unwrap()
}

async fn wait_for_durable_state(state: &DurableState, ready: impl Fn() -> bool) {
    loop {
        let changed = state.changed.notified();
        if ready() {
            return;
        }
        changed.await;
    }
}

fn durable_turn_events(turn_id: &str, answer: &str, cursor: u64, seq: u64) -> String {
    let assistant = serde_json::json!({
        "cursor": cursor.to_string(),
        "created_at": cursor,
        "turn_id": turn_id,
        "type": "event",
        "event": {
            "protocol_version": 1,
            "request_id": format!("request-{turn_id}"),
            "seq": seq,
            "type": "assistant.message",
            "payload": {"message": answer}
        }
    });
    let completed = serde_json::json!({
        "cursor": (cursor + 1).to_string(),
        "created_at": cursor + 1,
        "turn_id": turn_id,
        "type": "event",
        "event": {
            "protocol_version": 1,
            "request_id": format!("request-{turn_id}"),
            "seq": seq + 1,
            "type": "run.completed",
            "payload": {"status": "completed"}
        }
    });
    let terminal = serde_json::json!({
        "cursor": (cursor + 2).to_string(),
        "created_at": cursor + 2,
        "turn_id": turn_id,
        "type": "turn_completed",
        "id": turn_id,
        "final_message": answer,
        "usage": null,
        "citations": [],
        "usage_error": null
    });
    format!(
        "id: {cursor}\nevent: event\ndata: {assistant}\n\nid: {}\nevent: event\ndata: {completed}\n\nid: {}\nevent: turn_completed\ndata: {terminal}\n\n",
        cursor + 1,
        cursor + 2,
    )
}

fn durable_authorized(state: &DurableState, headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        == Some(state.authorization.as_str())
}

fn assert_process_events(bytes: &[u8], expected: &[(&str, u64)]) {
    let stdout = String::from_utf8(bytes.to_vec()).unwrap();
    let events = stdout
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), expected.len(), "{stdout}");
    for (event, (kind, seq)) in events.iter().zip(expected) {
        assert_eq!(event["type"], *kind, "{event}");
        assert_eq!(event["seq"], *seq, "{event}");
    }
}

async fn agent_state(State(state): State<TestState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    json_response(
        StatusCode::OK,
        serde_json::json!({
            "agent_id": AGENT_ID,
            "session_id": AGENT_ID,
            "has_snapshot": false,
            "completed_turns": 0,
            "last_active": 1,
            "active_turns": [],
            "agent_loaded": false,
            "connected_clients": 0,
            "capabilities": {
                "durable_turns": true,
                "resumable_events": true,
                "workspace": "private-hosted-tools-v1",
                "execution_environments": true,
                "execution_namespace": "cwd-root-v1",
                "native_cross_mounts": false
            },
            "settings": agent_settings(),
            "latest_event_cursor": "0",
            "stream_error": null
        }),
    )
}

async fn tool_host(State(state): State<TestState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    state.tool_host_attempts.fetch_add(1, Ordering::SeqCst);
    json_response(
        StatusCode::NOT_FOUND,
        serde_json::json!({"error":"no_cli_publisher"}),
    )
}

async fn managed_socket(
    State(state): State<TestState>,
    Path(agent): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    assert_eq!(agent, AGENT_ID);
    assert_eq!(query.get("cursor").map(String::as_str), Some("0"));
    upgrade
        .on_upgrade(move |socket| serve_managed_socket(socket, state))
        .into_response()
}

fn combined_receipt(input: &str) -> String {
    let receipt = serde_json::json!({
        "agent_id": AGENT_ID, "session_id": AGENT_ID,
        "turn_id": TURN_ID, "turn_idempotency_key": "server-derived-key",
        "state": "accepted", "input": input, "accepted_cursor": "1",
        "terminal_cursor": null, "created_at": 1, "accepted_at": 1,
        "updated_at": 1, "attempt_count": 1, "retry_at": null,
        "error": null, "terminal": null
    });
    format!("event: run\ndata: {receipt}\n\n")
}

fn combined_stream<F>(input: &str, remaining: F) -> Response<Body>
where
    F: Future<Output = String> + Send + 'static,
{
    use futures_util::StreamExt;
    let receipt = combined_receipt(input);
    let stream = futures_util::stream::once(async move { Ok::<_, Infallible>(receipt) }).chain(
        futures_util::stream::once(async move { Ok::<_, Infallible>(remaining.await) }),
    );
    Response::builder()
        .status(StatusCode::CREATED)
        .header("content-type", "text/event-stream")
        .header("x-nanocodex-settings", agent_settings().to_string())
        .body(Body::from_stream(stream))
        .unwrap()
}

async fn combined_cli_run(
    State(state): State<TestState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Response<Body> {
    assert!(authorized(&state, &headers));
    assert_eq!(headers["idempotency-key"], state.idempotency_key);
    assert_eq!(headers["accept"], "text/event-stream");
    assert_eq!(body["input"], "answer from managed");
    assert_eq!(
        body["settings"],
        serde_json::json!({"model":"gpt-6-astra", "thinking":"low", "reasoning_mode":"standard", "fast_mode":false})
    );
    state.completed.notify_one();
    combined_stream("answer from managed", async move {
        assert!(!state.expect_local_tool);
        durable_turn_events(TURN_ID, "managed answer", 2, 1)
    })
}

async fn serve_managed_socket(mut socket: WebSocket, state: TestState) {
    send_ready(&mut socket, "0", false).await;
    let Some(Ok(Message::Text(prompt))) = socket.recv().await else {
        return;
    };
    let prompt: serde_json::Value = serde_json::from_str(&prompt).unwrap();
    assert_eq!(prompt["type"], "prompt");
    assert_eq!(prompt["id"], state.idempotency_key);
    assert_eq!(prompt["input"], "answer from managed");
    let turn_id = prompt["id"].as_str().unwrap();
    state.completed.notify_one();
    send_accepted(&mut socket, turn_id, "answer from managed", 1).await;
    assert!(!state.expect_local_tool);
    send_turn_messages(&mut socket, turn_id, "managed answer", 2, 1).await;
}

async fn send_ready(socket: &mut WebSocket, cursor: &str, restored: bool) {
    socket
        .send(Message::Text(
            serde_json::json!({
                "type": "ready",
                "session_id": AGENT_ID,
                "restored": restored,
                "active_turns": [],
                "capabilities": agent_capabilities(),
                "settings": agent_settings(),
                "latest_event_cursor": cursor
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
}

fn agent_settings() -> serde_json::Value {
    serde_json::json!({
        "model": "gpt-6-astra",
        "thinking": "low",
        "reasoning_mode": "standard",
        "fast_mode": false
    })
}

fn agent_capabilities() -> serde_json::Value {
    serde_json::json!({
        "durable_turns": true,
        "resumable_events": true,
        "workspace": "cloudflare-computer",
        "execution_environments": true,
        "execution_namespace": "cwd-root-v1",
        "native_cross_mounts": false
    })
}

async fn send_accepted(socket: &mut WebSocket, turn_id: &str, input: &str, cursor: u64) {
    socket
        .send(Message::Text(
            serde_json::json!({
                "cursor": cursor.to_string(),
                "turn_id": turn_id,
                "type": "turn_accepted",
                "id": turn_id,
                "input": input,
                "replayed": false
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
}

async fn send_turn_messages(
    socket: &mut WebSocket,
    turn_id: &str,
    answer: &str,
    cursor: u64,
    seq: u64,
) {
    let messages = [
        serde_json::json!({
            "cursor": cursor.to_string(),
            "turn_id": turn_id,
            "type": "event",
            "event": {
                "protocol_version": 1,
                "request_id": format!("request-{turn_id}"),
                "seq": seq,
                "type": "assistant.message",
                "payload": {"message": answer}
            }
        }),
        serde_json::json!({
            "cursor": (cursor + 1).to_string(),
            "turn_id": turn_id,
            "type": "event",
            "event": {
                "protocol_version": 1,
                "request_id": format!("request-{turn_id}"),
                "seq": seq + 1,
                "type": "run.completed",
                "payload": {"status": "completed"}
            }
        }),
        serde_json::json!({
            "cursor": (cursor + 2).to_string(),
            "turn_id": turn_id,
            "type": "turn_completed",
            "id": turn_id,
            "final_message": answer,
            "usage": null,
            "citations": [],
            "usage_error": null
        }),
    ];
    for message in messages {
        socket
            .send(Message::Text(message.to_string().into()))
            .await
            .unwrap();
    }
}

async fn submit_turn(
    State(state): State<TestState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    assert_eq!(
        headers
            .get("idempotency-key")
            .and_then(|value| value.to_str().ok()),
        Some(state.idempotency_key),
    );
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["input"], "answer from managed");
    assert!(
        body.get("id").is_none(),
        "client supplied a turn ID: {body}"
    );
    state.completed.notify_one();
    json_response(
        StatusCode::ACCEPTED,
        serde_json::json!({
            "turn_id": TURN_ID,
            "state": "accepted",
            "input": "answer from managed",
            "accepted_cursor": "1",
            "terminal_cursor": null,
            "created_at": 1,
            "accepted_at": 1,
            "updated_at": 1,
            "attempt_count": 0,
            "retry_at": null,
            "error": null,
            "terminal": null,
        }),
    )
}

async fn events(State(state): State<TestState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    sse_response(async move {
        state.completed.notified().await;
        assert!(!state.expect_local_tool);
        durable_turn_events(TURN_ID, "managed answer", 2, 1)
    })
}

fn authorized(state: &TestState, headers: &HeaderMap) -> bool {
    let matches = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        == Some(state.authorization.as_str());
    if matches {
        state.authorized_requests.fetch_add(1, Ordering::SeqCst);
    }
    matches
}

fn unauthorized() -> Response<Body> {
    json_response(
        StatusCode::UNAUTHORIZED,
        serde_json::json!({ "error": "unauthorized" }),
    )
}

fn json_response(status: StatusCode, body: serde_json::Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

// An inherited login, Keychain reference, provider key, or installed Hand must
// never become authority for a synthetic-account fixture subprocess.
fn fixture_command(home: &std::path::Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(
        std::env::var_os("NANOCODEX2_TEST_BINARY")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_nanocodex").into()),
    );
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("CODEX_HOME", home.join(".codex"))
        .env("NANOCODEX_HOME", home)
        .env("NANOCODEX_DISABLE_HAND", "1")
        .env("NANOCODEX_COMPUTER", "off");
    command
}

fn fixture_model_catalog() -> serde_json::Value {
    // This synthetic account offers a restricted Astra default matching its
    // ready frames, plus Sol for the explicit pinned/settings journeys.
    serde_json::json!({
        "object": "list", "default_model": "gpt-6-astra", "data": [
            {"id": "gpt-6-astra", "name": "Astra", "provider": "openai",
                "thinking": ["low"], "fast_mode": false, "reasoning_modes": ["standard"]},
            {"id": "gpt-6.1-sol", "name": "Sol", "provider": "openai",
                "thinking": ["low", "medium", "high", "xhigh", "max"],
                "fast_mode": true, "reasoning_modes": ["standard", "pro"]}
        ]
    })
}

fn fixture_catalog<S>(authorization: String) -> axum::routing::MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    get(move |headers: HeaderMap| {
        let authorization = authorization.clone();
        async move {
            if headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                != Some(authorization.as_str())
            {
                return unauthorized();
            }
            json_response(StatusCode::OK, fixture_model_catalog())
        }
    })
}

fn configure_workspace(workspace: &std::path::Path) -> (tempfile::TempDir, tempfile::TempDir) {
    let config_home = tempfile::tempdir().unwrap();
    let decoy = tempfile::tempdir().unwrap();
    let workspace = toml::Value::String(workspace.to_string_lossy().into_owned());
    std::fs::write(
        config_home.path().join("config.toml"),
        format!("[agent]\nworkspace = {workspace}\n"),
    )
    .unwrap();
    (config_home, decoy)
}

#[tokio::test]
async fn headless_settings_and_cron_use_the_managed_contract() {
    use axum::{Json, extract::Request};
    use serde_json::{Value, json};

    let key = format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    let authorization = format!("Bearer {key}");
    let app = Router::new().fallback(move |request: Request| {
        let observed = observed.clone();
        let authorization = authorization.clone();
        async move {
            assert_eq!(request.headers()["authorization"], authorization);
            let method = request.method().clone();
            let path = request.uri().path().to_owned();
            let body = axum::body::to_bytes(request.into_body(), 128 * 1024).await.unwrap();
            let body: Value = if body.is_empty() { Value::Null } else { serde_json::from_slice(&body).unwrap() };
            // Catalog reads are authenticated, but are not settings/cron writes.
            if path == "/v1/models" {
                assert_eq!(method, axum::http::Method::GET);
                return Json(fixture_model_catalog()).into_response();
            }
            observed.lock().unwrap().push((method.to_string(), path.clone(), body));
            if method == axum::http::Method::DELETE {
                return StatusCode::NO_CONTENT.into_response();
            }
            if path == "/v1/agents" {
                return Json(json!({
                    "agent_id": AGENT_ID, "session_id": AGENT_ID,
                    "events_url": format!("/v1/agents/{AGENT_ID}/events"),
                    "websocket_url": format!("/v1/agents/{AGENT_ID}/live"),
                })).into_response();
            }
            if path.ends_with("/settings") {
                return Json(json!({"settings": {
                    "model": "gpt-6-astra", "thinking": "high", "reasoning_mode": "standard", "fast_mode": false,
                }})).into_response();
            }
            let mut trigger = json!({
                "id": "daily", "cron": "0 9 * * *", "timezone": "Europe/Athens", "input": "Summarize progress",
                "enabled": true, "session_mode": "new", "last_agent_id": null,
                "next_run_at": 1788768000000_u64, "last_run_at": null, "last_turn_id": null,
                "last_skipped_at": null, "created_at": 1788767000000_u64, "updated_at": 1788767000000_u64,
            });
            if method == axum::http::Method::GET && path.ends_with("/daily") {
                trigger.as_object_mut().unwrap().remove("session_mode");
                trigger.as_object_mut().unwrap().remove("last_agent_id");
            }
            Json(if path.ends_with("/triggers") { json!({"data": [trigger]}) } else { trigger }).into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let cwd = tempfile::tempdir().unwrap();
    for args in [
        vec![
            "new",
            "--model",
            "sol",
            "--thinking",
            "high",
            "--fast-mode",
            "--chatgpt-account",
            "account-a",
        ],
        vec!["settings", AGENT_ID, "thinking", "high"],
        vec![
            "cron",
            "put",
            AGENT_ID,
            "daily",
            "--cron",
            "0 9 * * *",
            "--timezone",
            "Europe/Athens",
            "--prompt",
            "Summarize progress",
        ],
        vec!["cron", "list", AGENT_ID],
        vec!["cron", "get", AGENT_ID, "daily"],
        vec!["cron", "delete", AGENT_ID, "daily"],
    ] {
        let output = tokio::time::timeout(
            PROCESS_TIMEOUT,
            fixture_command(cwd.path())
                .env("NANOCODEX_DISABLE_HAND", "1")
                .args(&args)
                .current_dir(cwd.path())
                .env("NANOCODEX_MANAGED_URL", &origin)
                .env("NANOCODEX_API_KEY", &key)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if args[1] != "delete" {
            let body = serde_json::from_slice::<Value>(&output.stdout).unwrap();
            if args[1] == "get" {
                assert_eq!(body["session_mode"], "continue");
            }
        }
    }
    let before_invalid = requests.lock().unwrap().len();
    for args in [
        vec!["run", "hello", "--agent", AGENT_ID, "--model", "sol"],
        vec!["cron", "get", AGENT_ID, "../escape"],
        vec!["new", "--model", "astra", "--thinking", "none"],
        vec!["new", "--model", "sol", "--thinking", "none"],
        vec!["new", "--model", "gpt-6-sol"],
        vec![
            "run",
            "hello",
            "--agent",
            AGENT_ID,
            "--chatgpt-account",
            "account-a",
        ],
        vec!["new", "--chatgpt-account", ""],
        vec!["new", "--chatgpt-account", "with space"],
    ] {
        let output = tokio::time::timeout(
            PROCESS_TIMEOUT,
            fixture_command(cwd.path())
                .env("NANOCODEX_DISABLE_HAND", "1")
                .args(&args)
                .current_dir(cwd.path())
                .env("NANOCODEX_MANAGED_URL", &origin)
                .env("NANOCODEX_API_KEY", &key)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!output.status.success(), "{args:?}");
    }
    server.abort();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), before_invalid);
    assert_eq!(
        requests[0].2["settings"],
        json!({"model": "gpt-6.1-sol", "thinking": "high", "reasoning_mode": "standard", "fast_mode": true})
    );
    assert_eq!(
        requests[0].2["configuration"],
        json!({"chatgpt_account_id": "account-a"})
    );
    assert_eq!(requests[1].2, json!({"thinking": "high"}));
    assert_eq!(requests[2].0, "PUT");
    assert_eq!(
        requests[2].2,
        json!({"cron": "0 9 * * *", "timezone": "Europe/Athens", "input": "Summarize progress", "enabled": true, "session_mode": "new"})
    );
    assert_eq!(requests[5].0, "DELETE");
}

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod docker_hand_live {
    use super::*;
    use axum::extract::ws::WebSocket;
    use serde_json::{Value, json};
    use tokio::sync::mpsc;

    #[derive(Clone)]
    struct Service {
        authorization: String,
        ready: mpsc::UnboundedSender<&'static str>,
    }

    async fn receive(socket: &mut WebSocket) -> Value {
        loop {
            let Some(Ok(Message::Text(text))) = socket.recv().await else {
                panic!("Hand socket closed early")
            };
            let message: Value = serde_json::from_str(&text).unwrap();
            if message["type"] != "ping" {
                return message;
            }
            send(socket, json!({"type":"pong","nonce":message["nonce"]})).await;
        }
    }
    async fn send(socket: &mut WebSocket, message: Value) {
        socket
            .send(Message::Text(message.to_string().into()))
            .await
            .unwrap();
    }
    async fn tools(
        State(state): State<Service>,
        headers: HeaderMap,
        upgrade: WebSocketUpgrade,
    ) -> Response<Body> {
        assert_eq!(headers["authorization"], state.authorization);
        upgrade.on_upgrade(move |mut socket| async move {
            let catalog = receive(&mut socket).await;
            assert_eq!(catalog["type"], "catalog");
            let machine = &catalog["machines"][0];
            assert_eq!(machine["id"], "docker-cli-test");
            assert_eq!(machine["workspace"], "/app");
            let capabilities = machine["capabilities"].as_array().unwrap();
            assert!(capabilities.iter().any(|v| v == "container"));
            assert!(!capabilities.iter().any(|v| v == "vm" || v == "network"));
            send(&mut socket, json!({"type":"ready"})).await;
            send(&mut socket, json!({
                "type":"call", "session_id":"docker-cli-agent", "call_id":"docker-cli-command",
                "model":"test", "name":"exec_command",
                "input":{"cmd":"test -z \"${NC_API_KEY-}${NANOCODEX_API_KEY-}\" && test ! -e /dev/kvm && printf 'docker-cli-proof\\n' > /app/proof && cat /app/proof", "login":false},
                "output_token_budget":1024, "output_byte_budget":131072,
                "deadline_at":9_000_000_000_000_u64,
            })).await;
            let result = receive(&mut socket).await;
            assert_eq!(result["type"], "result");
            assert_eq!(result["outcome"]["status"], "completed");
            assert_eq!(result["outcome"]["output"]["success"], true, "{result}");
            assert!(result["outcome"]["output"]["output"].as_str().unwrap().contains("docker-cli-proof"), "{result}");
            send(&mut socket, json!({"type":"ack","call_id":"docker-cli-command"})).await;
            state.ready.send("tools").unwrap();
            assert_eq!(receive(&mut socket).await["type"], "drain");
            send(&mut socket, json!({"type":"draining"})).await;
        })
    }
    async fn ice(State(state): State<Service>, headers: HeaderMap) -> axum::Json<Value> {
        assert_eq!(headers["authorization"], state.authorization);
        axum::Json(json!({"iceServers": []}))
    }
    async fn screen(
        State(state): State<Service>,
        headers: HeaderMap,
        upgrade: WebSocketUpgrade,
    ) -> Response<Body> {
        assert_eq!(headers["authorization"], state.authorization);
        upgrade.on_upgrade(move |mut socket| async move {
            send(
                &mut socket,
                json!({"type":"ready","connection_id":"docker-screen"}),
            )
            .await;
            let catalog = receive(&mut socket).await;
            assert_eq!(catalog["type"], "catalog");
            assert_eq!(catalog["machine_id"], "docker-cli-test");
            assert!(
                catalog["surfaces"][0].get("transport").is_none(),
                "{catalog}"
            );
            send(
                &mut socket,
                json!({"type":"published","generation":"docker-screen-generation"}),
            )
            .await;
            send(
                &mut socket,
                json!({"type":"viewer","viewer_id":"test-viewer","surface_id":"desktop"}),
            )
            .await;
            let offer = receive(&mut socket).await;
            assert_eq!(offer["type"], "signal", "{offer}");
            assert_eq!(offer["signal"]["type"], "offer", "{offer}");
            assert!(
                offer["signal"]["sdp"]
                    .as_str()
                    .unwrap()
                    .contains("H264/90000")
            );
            let mut media = webrtc::api::media_engine::MediaEngine::default();
            media.register_default_codecs().unwrap();
            let api = webrtc::api::APIBuilder::new().with_media_engine(media).build();
            let peer = api.new_peer_connection(Default::default()).await.unwrap();
            let (packets, mut received) = mpsc::unbounded_channel();
            peer.on_track(Box::new(move |track, _, _| {
                let packets = packets.clone();
                Box::pin(async move {
                    let (packet, _) = track.read_rtp().await.unwrap();
                    assert!(!packet.payload.is_empty());
                    assert_eq!(track.codec().capability.mime_type.to_lowercase(), "video/h264");
                    packets.send(()).unwrap();
                })
            }));
            let (candidates, mut outgoing) = mpsc::unbounded_channel();
            peer.on_ice_candidate(Box::new(move |candidate| {
                if let Some(candidate) = candidate {
                    let candidate = candidate.to_json().unwrap();
                    candidates.send(json!({"type":"signal","viewer_id":"test-viewer","signal":{
                        "type":"candidate", "candidate":candidate.candidate,
                        "sdpMid":candidate.sdp_mid, "sdpMLineIndex":candidate.sdp_mline_index,
                    }})).unwrap();
                }
                Box::pin(async {})
            }));
            peer.set_remote_description(
                webrtc::peer_connection::sdp::session_description::RTCSessionDescription::offer(
                    offer["signal"]["sdp"].as_str().unwrap().to_owned(),
                ).unwrap(),
            ).await.unwrap();
            let answer = peer.create_answer(None).await.unwrap();
            peer.set_local_description(answer.clone()).await.unwrap();
            send(&mut socket, json!({"type":"signal","viewer_id":"test-viewer","signal":{
                "type":"answer", "sdp":answer.sdp,
            }})).await;
            loop {
                tokio::select! {
                    packet = received.recv() => { packet.unwrap(); break; }
                    candidate = outgoing.recv() => { send(&mut socket, candidate.unwrap()).await; }
                    message = receive(&mut socket) => {
                        assert_eq!(message["type"], "signal", "{message}");
                        let signal = &message["signal"];
                        assert_eq!(signal["type"], "candidate", "{message}");
                        peer.add_ice_candidate(serde_json::from_value(signal.clone()).unwrap()).await.unwrap();
                    }
                }
            }
            state.ready.send("screen").unwrap();
            while socket.recv().await.is_some() {}
            peer.close().await.unwrap();
        })
    }

    struct WorkspaceVolume(String);
    impl Drop for WorkspaceVolume {
        fn drop(&mut self) {
            let output = std::process::Command::new("docker")
                .args(["ps", "-aq", "--filter", &format!("volume={}", self.0)])
                .output()
                .unwrap();
            for id in String::from_utf8_lossy(&output.stdout).lines() {
                let _ = std::process::Command::new("docker")
                    .args(["rm", "-f", id])
                    .output();
            }
            let _ = std::process::Command::new("docker")
                .args(["volume", "rm", &self.0])
                .output();
        }
    }

    #[tokio::test]
    #[ignore = "requires the built Docker Hand image and a Linux Docker daemon"]
    async fn docker_hand_publishes_tools_and_screen_then_drains_on_sigterm() {
        let image =
            std::env::var("NANOCODEX_DOCKER_TEST_IMAGE").expect("set NANOCODEX_DOCKER_TEST_IMAGE");
        let volume = WorkspaceVolume(format!("nanocodex-cli-test-{}", uuid::Uuid::new_v4()));
        let key = format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43));
        let (ready, mut events) = mpsc::unbounded_channel();
        let service = Service {
            authorization: format!("Bearer {key}"),
            ready,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/account/tool-host", get(tools))
            .route("/v1/account/hands/host", get(screen))
            .route("/v1/account/hands/ice", post(ice))
            .with_state(service);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = tempfile::tempdir().unwrap();
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex"))
            .env("NANOCODEX_DISABLE_HAND", "1")
            .args([
                "hand",
                "--docker",
                &image,
                "--volume",
                &volume.0,
                "--network",
                "off",
                "--machine-id",
                "docker-cli-test",
            ])
            .env("NC_API_KEY", &key)
            .env("NANOCODEX_MANAGED_URL", origin)
            .env("NANOCODEX_HOME", config.path())
            .env_remove("NANOCODEX_API_KEY")
            .env_remove("NANOCODEX_VM_GUEST_RUNTIME")
            .env_remove("NANOCODEX_KRUNFW_DIR")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let ready = tokio::time::timeout(std::time::Duration::from_secs(60), async {
            let first = events.recv().await.unwrap();
            let second = events.recv().await.unwrap();
            assert_ne!(first, second);
        })
        .await;
        if ready.is_err() {
            let _ = child.start_kill();
            let output = child.wait_with_output().await.unwrap();
            panic!(
                "Docker Hand did not publish: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(i32::try_from(child.id().unwrap()).unwrap()),
            nix::sys::signal::Signal::SIGTERM,
        )
        .unwrap();
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(30), child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains(&key));
        assert!(!String::from_utf8_lossy(&output.stdout).contains(&key));
        let containers = tokio::process::Command::new("docker")
            .args(["ps", "-aq", "--filter", &format!("volume={}", volume.0)])
            .output()
            .await
            .unwrap();
        assert!(
            containers.stdout.is_empty(),
            "SIGTERM left a container behind"
        );
        let volume_exists = tokio::process::Command::new("docker")
            .args(["volume", "inspect", &volume.0])
            .output()
            .await
            .unwrap();
        assert!(
            volume_exists.status.success(),
            "SIGTERM deleted the workspace volume"
        );
        server.abort();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn docker_preflight_errors_are_actionable_before_account_login() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let docker = dir.path().join("docker");
    for (script, extra, expected) in [
        (None, vec![], "Install the Docker CLI"),
        (Some("exit 1"), vec![], "start a Linux Docker daemon"),
        (
            Some("echo '{\"OSType\":\"windows\"}'"),
            vec![],
            "switch Docker to Linux containers",
        ),
        (
            Some(
                "echo '{\"OSType\":\"linux\",\"Architecture\":\"x86_64\",\"Runtimes\":{\"runc\":{}}}'",
            ),
            vec!["--runtime", "runsc"],
            "not configured on this daemon",
        ),
        (
            Some(
                "if [ \"$1\" = info ]; then echo '{\"OSType\":\"linux\",\"Architecture\":\"x86_64\"}'; else exit 1; fi",
            ),
            vec![],
            "pnpm build:hand-docker",
        ),
        (
            Some(
                "if [ \"$1\" = info ]; then echo '{\"OSType\":\"linux\",\"Architecture\":\"x86_64\"}'; else echo linux/arm64; fi",
            ),
            vec![],
            "rebuild the image",
        ),
    ] {
        if let Some(script) = script {
            std::fs::write(&docker, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex"))
            .args(["hand", "--docker", "image", "--volume", "work"])
            .args(extra)
            .env_clear()
            .env("NANOCODEX_DISABLE_HAND", "1")
            .env("PATH", dir.path())
            .env("NANOCODEX_HOME", dir.path())
            .env("HOME", dir.path())
            .env("CODEX_HOME", dir.path().join(".codex"))
            .env("NANOCODEX_COMPUTER", "off")
            // VM environment defaults must not invalidate Docker selection.
            .env("NANOCODEX_VM_GUEST_RUNTIME", "/missing/guest")
            .env("NANOCODEX_KRUNFW_DIR", "/missing/firmware")
            .current_dir(dir.path())
            .output()
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{stderr}");
    }
}

#[cfg(unix)]
#[path = "native_screen_lifecycle.rs"]
mod native_screen_lifecycle;

// Real executable over HTTP/WS: optional catalog failure must not gate the
// fixed hosted default or an explicit model. Authentication still gates admission.
#[tokio::test]
async fn startup_uses_hosted_defaults_without_reading_catalog() {
    // Since #872 (398726862) an omitted-settings `new` lets the service pick
    // its default without any catalog read, while an omitted-settings `run`
    // resolves the account catalog default before admitting anything. Every
    // explicit or pinned startup still never reads the catalog.
    for catalog_mode in ["held", "unavailable", "available"] {
        for explicit in [true, false] {
            for command in ["run", "new"] {
                startup_catalog_journey(command, catalog_mode, explicit, false, false).await;
            }
        }
    }
    for command in ["run", "new"] {
        startup_catalog_journey(command, "held", true, true, false).await;
        startup_catalog_journey(command, "held", false, false, true).await;
        startup_catalog_journey(command, "held", true, false, true).await;
        startup_catalog_journey(command, "held", false, true, true).await;
    }
}

async fn startup_catalog_journey(
    command_name: &'static str,
    catalog_mode: &'static str,
    explicit: bool,
    revoked: bool,
    pinned: bool,
) {
    let reads = Arc::new(AtomicUsize::new(0));
    let prompts = Arc::new(AtomicUsize::new(0));
    let admissions = Arc::new(AtomicUsize::new(0));
    let key = format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43));
    let authorization = format!("Bearer {key}");
    let catalog_reads = reads.clone();
    let prompt_count = prompts.clone();
    let admission_count = admissions.clone();
    // Only an omitted-settings run consults the account catalog default.
    let reads_catalog = command_name == "run" && !explicit && !pinned;
    let creation = post(
        move |headers: HeaderMap, body: axum::body::Bytes| async move {
            admission_count.fetch_add(1, Ordering::SeqCst);
            // An omitted-settings `new` posts no body at all.
            let body: serde_json::Value = if body.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::from_slice(&body).unwrap()
            };
            assert_eq!(headers["authorization"], authorization);
            if revoked {
                return unauthorized();
            }
            if pinned {
                assert_eq!(body["configuration"]["chatgpt_account_id"], "synthetic-pin");
            }
            assert!(body.get("settings_selection").is_none());
            if command_name == "new" && !explicit && !pinned {
                // The service chooses its own default; nothing is guessed locally.
                assert!(body.get("settings").is_none(), "{body}");
            } else {
                assert_eq!(
                    body["settings"],
                    serde_json::json!({
                        "model": if explicit { "gpt-6.1-sol" } else { "gpt-6-astra" },
                        "thinking": if pinned && !explicit { "high" } else { "low" },
                        "reasoning_mode": "standard", "fast_mode": false,
                    })
                );
            }
            if command_name == "new" {
                assert!(body.get("input").is_none());
                return json_response(
                    StatusCode::CREATED,
                    serde_json::json!({
                        "agent_id": AGENT_ID, "session_id": AGENT_ID,
                        "events_url": format!("/v1/agents/{AGENT_ID}/events"),
                        "websocket_url": format!("/v1/agents/{AGENT_ID}/ws"),
                    }),
                );
            }
            assert_eq!(headers["accept"], "text/event-stream");
            assert_eq!(body["input"], "startup answer");
            prompt_count.fetch_add(1, Ordering::SeqCst);
            combined_stream("startup answer", async {
                durable_turn_events(TURN_ID, "catalog-independent answer", 2, 1)
            })
        },
    );
    let app = Router::new()
        .route(
            "/v1/models",
            get(move || async move {
                catalog_reads.fetch_add(1, Ordering::SeqCst);
                match catalog_mode {
                    "held" => std::future::pending().await,
                    "unavailable" => json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        serde_json::json!({"error": "model_availability_unavailable"}),
                    ),
                    _ => json_response(StatusCode::OK, fixture_model_catalog()),
                }
            }),
        )
        .route("/v1/agent-runs", creation.clone())
        .route("/v1/agents", creation);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = tempfile::tempdir().unwrap();
    let mut command = fixture_command(home.path());
    command
        .env("NANOCODEX_MANAGED_URL", format!("http://{address}"))
        .env("NC_API_KEY", key)
        .arg(command_name)
        .kill_on_drop(true);
    if command_name == "run" {
        command.arg("startup answer");
    }
    if explicit {
        command.args(["--model", "sol"]);
    }
    if pinned {
        command.args(["--chatgpt-account", "synthetic-pin"]);
        if !explicit {
            command.args(["--thinking", "high", "--fast-mode=false"]);
        }
    }
    if reads_catalog && catalog_mode == "held" {
        // The account default is unknown until the catalog answers, so the run
        // must wait rather than admit a locally guessed model.
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(2), command.output()).await;
        assert!(
            result.is_err(),
            "run admitted before the held catalog answered"
        );
    } else {
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(8), command.output()).await;
        let output = result.expect("CLI journey timed out").unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "command={command_name} catalog={catalog_mode} explicit={explicit} revoked={revoked} pinned={pinned}: status={} stdout={stdout} stderr={stderr}",
            output.status
        );
        if revoked {
            assert!(!output.status.success());
            assert!(
                stderr.contains("401") || stderr.contains("Unauthorized"),
                "{stderr}"
            );
        } else if reads_catalog && catalog_mode == "unavailable" {
            assert!(!output.status.success(), "{stderr}");
        } else {
            assert!(output.status.success(), "{stderr}");
            let expected = if command_name == "new" {
                AGENT_ID
            } else {
                "catalog-independent answer"
            };
            assert!(stdout.contains(expected), "{stdout}");
        }
    }
    let catalog_reads = reads.load(Ordering::SeqCst);
    if reads_catalog && catalog_mode == "unavailable" {
        // Managed reads retry a transient 503 before surfacing it.
        assert!(catalog_reads >= 1, "{catalog_reads}");
    } else {
        assert_eq!(catalog_reads, usize::from(reads_catalog));
    }
    let admitted = !reads_catalog || catalog_mode == "available";
    assert_eq!(
        prompts.load(Ordering::SeqCst),
        usize::from(admitted && !revoked && command_name == "run")
    );
    assert_eq!(
        admissions.load(Ordering::SeqCst),
        usize::from(admitted),
        "one startup POST"
    );
    server.abort();
}

#[tokio::test]
async fn explicit_model_rejects_invalid_options_before_network() {
    let home = tempfile::tempdir().unwrap();
    for flags in [
        vec![
            "--model",
            "claude-sonnet-4-6",
            "--chatgpt-account",
            "synthetic-pin",
        ],
        vec!["--model", "claude-sonnet-4-6", "--thinking", "xhigh"],
        vec!["--model", "claude-sonnet-4-6", "--reasoning-mode", "pro"],
        vec!["--model", "claude-sonnet-4-6", "--fast-mode"],
        vec!["--model", "unknown-model"],
    ] {
        let output = fixture_command(home.path())
            .env("NANOCODEX_MANAGED_URL", "http://127.0.0.1:1")
            .env(
                "NC_API_KEY",
                format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)),
            )
            .args(["run", "startup answer"])
            .args(&flags)
            .output()
            .await
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(
            stderr.contains("cannot be pinned")
                || stderr.contains("not offered")
                || stderr.contains("supported managed model")
                || stderr.contains("on the managed service; supported"),
            "{flags:?}: {stderr}"
        );
        eprintln!("invalid flags={flags:?}: {stderr}");
    }
}
