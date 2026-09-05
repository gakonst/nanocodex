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
const LOCAL_TURN_ID: &str = "019fc927-b282-7a11-8445-1b9996ad2fb0";
const CLOUD_TURN_ID: &str = "019fc927-b283-7a11-8445-1b9996ad2fb0";
const PROCESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[tokio::test]
async fn hand_help_exposes_the_vm_screen_and_machine_contract() {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex2"))
        .args(["hand", "--help"])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Usage: nanocodex2 hand [OPTIONS]\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("AGENT_ID"), "{stdout}");
    for expected in [
        "--vm <ROOTFS>",
        "--screen <SCREEN>",
        "--device <DEVICE>",
        "--workspace <LOCAL_WORKSPACE>",
        "[possible values: desktop, android]",
        "--vm-guest-runtime <ELF>",
        "--vm-workspace <PATH>",
        "--vm-cpus <COUNT>",
        "--vm-memory-mib <MIB>",
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
async fn host_help_exposes_the_bounded_vm_pool_contract() {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex2"))
        .args(["host", "--help"])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(
            "Usage: nanocodex2 host [OPTIONS] --factory-name <FACTORY_NAME> --vm-template <ROOTFS> --state-dir <PATH> --vm-guest-runtime <ELF>"
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
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex2"))
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
    assert!(encoded.contains("vm.launch"), "{encoded}");
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
    let missing = tempfile::tempdir()
        .unwrap()
        .path()
        .join("missing-launch-record");
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex2"))
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
    tool_completed: Arc<tokio::sync::Notify>,
    origin: String,
    failed_tool_host_attempts: usize,
    expect_local_tool: bool,
    delay_ready_until_submission: bool,
    disconnect_after_ready: bool,
    catalogs: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[tokio::test]
async fn run_uses_managed_lifecycle_with_the_configured_local_workspace() {
    let api_key = format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = TestState {
        authorization: format!("Bearer {api_key}"),
        idempotency_key: "stable-request",
        authorized_requests: Arc::new(AtomicUsize::new(0)),
        tool_host_attempts: Arc::new(AtomicUsize::new(0)),
        completed: Arc::new(tokio::sync::Notify::new()),
        tool_completed: Arc::new(tokio::sync::Notify::new()),
        origin: format!("http://{address}"),
        failed_tool_host_attempts: 0,
        expect_local_tool: true,
        delay_ready_until_submission: true,
        disconnect_after_ready: false,
        catalogs: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/v1/agents/live", get(create_live_socket))
        .route("/v1/agents/{agent}", get(agent_state))
        .route("/v1/agents/{agent}/tool-host", get(tool_host))
        .route("/v1/agents/{agent}/ws", get(managed_socket))
        .route("/v1/agents/{agent}/turns", post(submit_turn))
        .route("/v1/agents/{agent}/events", get(events))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let workspace = tempfile::tempdir().unwrap();
    let git = |arguments: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(workspace.path())
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success());
    };
    git(&["init", "-q"]);
    std::fs::write(workspace.path().join("fixture.txt"), "managed fixture\n").unwrap();
    git(&["add", "fixture.txt"]);

    let (config_home, decoy) = configure_workspace(workspace.path());
    let output = tokio::time::timeout(
        PROCESS_TIMEOUT,
        tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex2"))
            .args([
                "run",
                "answer from managed",
                "--idempotency-key",
                "stable-request",
            ])
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
    assert!(state.authorized_requests.load(Ordering::SeqCst) >= 2);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("hosted-proof.txt")).unwrap(),
        "private-host\n",
    );
    assert!(!decoy.path().join("hosted-proof.txt").exists());
    assert!(!config_home.path().join("host-id").exists());
    assert!(!config_home.path().join("attachment-id").exists());
    let catalogs = state.catalogs.lock().unwrap();
    assert_eq!(catalogs.len(), 1);
    let catalog = &catalogs[0];
    let attachment_id = catalog["attachment_id"].as_str().unwrap();
    assert!(attachment_id.len() <= 123);
    assert!(uuid::Uuid::parse_str(attachment_id).is_ok());
    assert_eq!(catalog["machines"].as_array().unwrap().len(), 1);
    assert_eq!(catalog["machines"][0]["id"], attachment_id);
    assert_eq!(
        catalog["machines"][0]["workspace"],
        workspace.path().to_string_lossy().as_ref()
    );
    assert!(
        !catalog["machines"][0]["name"]
            .as_str()
            .unwrap()
            .trim()
            .is_empty()
    );
    assert_eq!(
        catalog["machines"][0]["capabilities"],
        serde_json::json!(["native", "filesystem", "process", "package", "server"])
    );
    drop(catalogs);
    server.abort();
}

#[tokio::test]
async fn run_rejects_a_malformed_create_live_ready_frame() {
    let api_key = format!("ncx_live_{}_{}", "1".repeat(12), "2".repeat(43));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = TestState {
        authorization: format!("Bearer {api_key}"),
        idempotency_key: "unused",
        authorized_requests: Arc::new(AtomicUsize::new(0)),
        tool_host_attempts: Arc::new(AtomicUsize::new(0)),
        completed: Arc::new(tokio::sync::Notify::new()),
        tool_completed: Arc::new(tokio::sync::Notify::new()),
        origin: format!("http://{address}"),
        failed_tool_host_attempts: 0,
        expect_local_tool: false,
        delay_ready_until_submission: false,
        disconnect_after_ready: false,
        catalogs: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/v1/agents/live", get(failed_create_live_socket))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let workspace = tempfile::tempdir().unwrap();
    let (config_home, decoy) = configure_workspace(workspace.path());

    let output = tokio::time::timeout(
        PROCESS_TIMEOUT,
        tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex2"))
            .args(["run", "this turn must not submit"])
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
async fn run_keeps_the_durable_agent_when_local_tools_are_initially_unavailable() {
    let api_key = format!("ncx_live_{}_{}", "c".repeat(12), "d".repeat(43));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = TestState {
        authorization: format!("Bearer {api_key}"),
        idempotency_key: "stable-request-without-local-tools",
        authorized_requests: Arc::new(AtomicUsize::new(0)),
        tool_host_attempts: Arc::new(AtomicUsize::new(0)),
        completed: Arc::new(tokio::sync::Notify::new()),
        tool_completed: Arc::new(tokio::sync::Notify::new()),
        origin: format!("http://{address}"),
        failed_tool_host_attempts: usize::MAX,
        expect_local_tool: false,
        delay_ready_until_submission: false,
        disconnect_after_ready: false,
        catalogs: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/v1/agents/live", get(create_live_socket))
        .route("/v1/agents/{agent}", get(agent_state))
        .route("/v1/agents/{agent}/tool-host", get(tool_host))
        .route("/v1/agents/{agent}/ws", get(managed_socket))
        .route("/v1/agents/{agent}/turns", post(submit_turn))
        .route("/v1/agents/{agent}/events", get(events))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let workspace = tempfile::tempdir().unwrap();
    let (config_home, decoy) = configure_workspace(workspace.path());

    let output = tokio::time::timeout(
        PROCESS_TIMEOUT,
        tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex2"))
            .args([
                "run",
                "answer from managed",
                "--idempotency-key",
                "stable-request-without-local-tools",
            ])
            .env("NANOCODEX_MANAGED_URL", &state.origin)
            .env("NC_API_KEY", &api_key)
            .env_remove("NANOCODEX_API_KEY")
            .env("NANOCODEX_HOME", config_home.path())
            .env_remove("OPENAI_API_KEY")
            .current_dir(decoy.path())
            .output(),
    )
    .await
    .expect("nanocodex2 cloud fallback lifecycle timed out")
    .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("assistant.message"));
    assert_eq!(
        stderr,
        format!("Managed agent: {AGENT_ID}\nmanaged answer\n")
    );
    assert!(state.tool_host_attempts.load(Ordering::SeqCst) >= 1);
    assert!(!workspace.path().join("hosted-proof.txt").exists());
    assert!(!decoy.path().join("hosted-proof.txt").exists());
    server.abort();
}

#[tokio::test]
async fn run_reconnects_the_same_local_host_after_a_ready_socket_disconnect() {
    let api_key = format!("ncx_live_{}_{}", "3".repeat(12), "4".repeat(43));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = TestState {
        authorization: format!("Bearer {api_key}"),
        idempotency_key: "disconnect-then-cloud",
        authorized_requests: Arc::new(AtomicUsize::new(0)),
        tool_host_attempts: Arc::new(AtomicUsize::new(0)),
        completed: Arc::new(tokio::sync::Notify::new()),
        tool_completed: Arc::new(tokio::sync::Notify::new()),
        origin: format!("http://{address}"),
        failed_tool_host_attempts: 0,
        expect_local_tool: false,
        delay_ready_until_submission: false,
        disconnect_after_ready: true,
        catalogs: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/v1/agents/live", get(create_live_socket))
        .route("/v1/agents/{agent}", get(agent_state))
        .route("/v1/agents/{agent}/tool-host", get(tool_host))
        .route("/v1/agents/{agent}/ws", get(managed_socket))
        .route("/v1/agents/{agent}/turns", post(submit_turn))
        .route("/v1/agents/{agent}/events", get(events))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let workspace = tempfile::tempdir().unwrap();
    let (config_home, decoy) = configure_workspace(workspace.path());

    let output = tokio::time::timeout(
        PROCESS_TIMEOUT,
        tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex2"))
            .args([
                "run",
                "answer from managed",
                "--idempotency-key",
                "disconnect-then-cloud",
            ])
            .env("NANOCODEX_MANAGED_URL", &state.origin)
            .env("NC_API_KEY", &api_key)
            .env_remove("NANOCODEX_API_KEY")
            .env("NANOCODEX_HOME", config_home.path())
            .env_remove("OPENAI_API_KEY")
            .current_dir(decoy.path())
            .output(),
    )
    .await
    .expect("nanocodex2 reconnect lifecycle timed out")
    .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("run.completed"));
    assert_eq!(state.tool_host_attempts.load(Ordering::SeqCst), 2);
    let catalogs = state.catalogs.lock().unwrap();
    assert_eq!(catalogs.len(), 2);
    assert_eq!(catalogs[0], catalogs[1]);
    assert_eq!(
        catalogs[0]["attachment_id"],
        catalogs[0]["machines"][0]["id"]
    );
    assert!(!workspace.path().join("hosted-proof.txt").exists());
    assert!(!String::from_utf8_lossy(&output.stderr).contains(&api_key));
    server.abort();
}

#[tokio::test]
async fn run_reopens_one_durable_agent_and_falls_back_when_local_tools_are_absent() {
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
        accepted_attachments: Arc::new(AtomicUsize::new(0)),
        local_calls: Arc::new(AtomicUsize::new(0)),
        detaches: Arc::new(AtomicUsize::new(0)),
        changed: Arc::new(tokio::sync::Notify::new()),
        first_submitted: Arc::new(tokio::sync::Notify::new()),
        tool_completed: Arc::new(tokio::sync::Notify::new()),
    };
    let app = Router::new()
        .route("/v1/agents/live", get(durable_create_live_socket))
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
        tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex2"))
            .args([
                "run",
                "first durable turn",
                "--idempotency-key",
                "durable-turn-one",
            ])
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
        format!("Managed agent: {AGENT_ID}\nlocal turn answer\n")
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("hosted-proof.txt")).unwrap(),
        "private-host\n",
    );
    assert_eq!(state.accepted_attachments.load(Ordering::SeqCst), 1);
    assert_eq!(state.local_calls.load(Ordering::SeqCst), 1);
    assert_eq!(state.detaches.load(Ordering::SeqCst), 1);

    let second = tokio::time::timeout(
        PROCESS_TIMEOUT,
        tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex2"))
            .args([
                "run",
                "second durable turn",
                "--agent",
                AGENT_ID,
                "--idempotency-key",
                "durable-turn-two",
            ])
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
        "cloud fallback answer\n"
    );

    assert_eq!(state.creates.load(Ordering::SeqCst), 1);
    assert_eq!(state.state_reads.lock().unwrap().as_slice(), [AGENT_ID]);
    let cursors = state.event_cursors.lock().unwrap();
    assert_eq!(cursors.first().map(String::as_str), Some("0"));
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
    assert!(state.tool_host_attempts.load(Ordering::SeqCst) >= 2);
    assert_eq!(state.accepted_attachments.load(Ordering::SeqCst), 1);
    assert_eq!(state.local_calls.load(Ordering::SeqCst), 1);
    assert_eq!(state.detaches.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("hosted-proof.txt")).unwrap(),
        "private-host\n",
    );
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
    accepted_attachments: Arc<AtomicUsize>,
    local_calls: Arc<AtomicUsize>,
    detaches: Arc<AtomicUsize>,
    changed: Arc<tokio::sync::Notify>,
    first_submitted: Arc<tokio::sync::Notify>,
    tool_completed: Arc<tokio::sync::Notify>,
}

async fn durable_create_live_socket(
    State(state): State<DurableState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    if !durable_authorized(&state, &headers) {
        return unauthorized();
    }
    state.creates.fetch_add(1, Ordering::SeqCst);
    state.event_cursors.lock().unwrap().push("0".to_owned());
    upgrade
        .on_upgrade(move |socket| serve_durable_socket(socket, state, "0".to_owned()))
        .into_response()
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
        "active_turn_details": [],
        "agent_loaded": latest_event_cursor != "0",
        "connected_clients": 0,
        "capabilities": {
            "durable_turns": true,
            "resumable_events": true,
            "live_steer": true,
            "live_cancel": true,
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

async fn durable_tool_host(
    State(state): State<DurableState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    if !durable_authorized(&state, &headers) {
        return unauthorized();
    }
    assert_eq!(agent, AGENT_ID);
    let attempt = state.tool_host_attempts.fetch_add(1, Ordering::SeqCst) + 1;
    state.changed.notify_waiters();
    if attempt > 1 {
        return Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .body(Body::from("local attachment is absent"))
            .unwrap();
    }
    state.accepted_attachments.fetch_add(1, Ordering::SeqCst);
    upgrade
        .on_upgrade(move |socket| serve_durable_tool_host(socket, state))
        .into_response()
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
        (1, "durable-turn-one", "first durable turn") => ("local turn answer", 1, 1),
        (2, "durable-turn-two", "second durable turn") => ("cloud fallback answer", 4, 3),
        unexpected => panic!("unexpected durable socket submission: {unexpected:?}"),
    };
    if index == 1 {
        state.first_submitted.notify_one();
    }
    state.changed.notify_waiters();
    send_accepted(&mut socket, &key, &input, first_cursor).await;
    if index == 1 {
        wait_for_durable_state(&state, || state.local_calls.load(Ordering::SeqCst) == 1).await;
    } else {
        wait_for_durable_state(&state, || {
            state.tool_host_attempts.load(Ordering::SeqCst) >= 2
        })
        .await;
    }
    send_turn_messages(&mut socket, &key, answer, first_cursor + 1, sequence).await;
}

async fn serve_durable_tool_host(socket: WebSocket, state: DurableState) {
    let observer_state = state.clone();
    let observer = tokio::spawn(async move {
        observer_state.tool_completed.notified().await;
        observer_state.local_calls.fetch_add(1, Ordering::SeqCst);
        observer_state.changed.notify_waiters();
    });
    let compatible_state = TestState {
        authorization: state.authorization.clone(),
        idempotency_key: "unused",
        authorized_requests: Arc::new(AtomicUsize::new(0)),
        tool_host_attempts: Arc::new(AtomicUsize::new(0)),
        completed: state.first_submitted.clone(),
        tool_completed: state.tool_completed.clone(),
        origin: state.origin.clone(),
        failed_tool_host_attempts: 0,
        expect_local_tool: true,
        delay_ready_until_submission: false,
        disconnect_after_ready: false,
        catalogs: Arc::new(Mutex::new(Vec::new())),
    };
    serve_tool_host(socket, compatible_state, false).await;
    observer.await.unwrap();
    state.detaches.fetch_add(1, Ordering::SeqCst);
    state.changed.notify_waiters();
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
        (1, "durable-turn-one", "first durable turn") => (LOCAL_TURN_ID, "1"),
        (2, "durable-turn-two", "second durable turn") => (CLOUD_TURN_ID, "4"),
        unexpected => panic!("unexpected durable submission: {unexpected:?}"),
    };
    if index == 1 {
        state.first_submitted.notify_one();
    }
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
            wait_for_durable_state(&state, || {
                !state.submissions.lock().unwrap().is_empty()
                    && state.local_calls.load(Ordering::SeqCst) == 1
            })
            .await;
            durable_turn_events(LOCAL_TURN_ID, "local turn answer", 1, 1)
        }),
        "3" => sse_response(async move {
            wait_for_durable_state(&state, || {
                state.submissions.lock().unwrap().len() >= 2
                    && state.tool_host_attempts.load(Ordering::SeqCst) >= 2
            })
            .await;
            durable_turn_events(CLOUD_TURN_ID, "cloud fallback answer", 4, 3)
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
            "active_turn_details": [],
            "agent_loaded": false,
            "connected_clients": 0,
            "capabilities": {
                "durable_turns": true,
                "resumable_events": true,
                "live_steer": true,
                "live_cancel": true,
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

async fn tool_host(
    State(state): State<TestState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let attempt = state.tool_host_attempts.fetch_add(1, Ordering::SeqCst) + 1;
    if attempt <= state.failed_tool_host_attempts {
        return Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .body(Body::from("local tools unavailable"))
            .unwrap();
    }
    upgrade
        .on_upgrade(move |socket| {
            let disconnect = state.disconnect_after_ready && attempt == 1;
            serve_tool_host(socket, state, disconnect)
        })
        .into_response()
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

async fn create_live_socket(
    State(state): State<TestState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    upgrade
        .on_upgrade(move |mut socket| async move {
            if state.disconnect_after_ready {
                send_ready(&mut socket, "0", false).await;
                drop(socket.send(Message::Close(None)).await);
            } else {
                serve_managed_socket(socket, state).await;
            }
        })
        .into_response()
}

async fn failed_create_live_socket(
    State(state): State<TestState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    upgrade
        .on_upgrade(|mut socket| async move {
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "type": "ready",
                        "session_id": "wrong-agent",
                        "restored": false,
                        "active_turns": [],
                        "active_turn_details": [],
                        "capabilities": agent_capabilities(),
                        "settings": agent_settings(),
                        "latest_event_cursor": "not-a-cursor"
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        })
        .into_response()
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
    if state.disconnect_after_ready {
        state.tool_completed.notified().await;
        while state.tool_host_attempts.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    } else if state.expect_local_tool {
        state.tool_completed.notified().await;
    } else {
        while state.tool_host_attempts.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    }
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
                "active_turn_details": [],
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
        "model": "gpt-5.6-sol",
        "thinking": "high",
        "reasoning_mode": "standard",
        "fast_mode": false
    })
}

fn agent_capabilities() -> serde_json::Value {
    serde_json::json!({
        "durable_turns": true,
        "resumable_events": true,
        "live_steer": true,
        "live_cancel": true,
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

async fn serve_tool_host(mut socket: WebSocket, state: TestState, disconnect_after_ready: bool) {
    let Some(Ok(Message::Text(catalog))) = socket.recv().await else {
        return;
    };
    let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
    assert_eq!(catalog["type"], "catalog");
    assert_eq!(catalog.as_object().unwrap().len(), 4);
    let names = catalog["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["definition"]["name"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        names,
        ["apply_patch", "exec_command", "view_image", "write_stdin"]
            .into_iter()
            .collect(),
    );
    state.catalogs.lock().unwrap().push(catalog);
    if state.delay_ready_until_submission {
        state.completed.notified().await;
    }
    socket
        .send(Message::Text(
            serde_json::json!({"type":"ready"}).to_string().into(),
        ))
        .await
        .unwrap();

    if disconnect_after_ready {
        state.tool_completed.notify_one();
        drop(socket.send(Message::Close(None)).await);
        return;
    }

    if !state.expect_local_tool {
        serve_until_drain(&mut socket).await;
        return;
    }

    if !state.delay_ready_until_submission {
        state.completed.notified().await;
    }
    socket
        .send(Message::Text(
            serde_json::json!({
                "type": "call",
                "session_id": AGENT_ID,
                "call_id": "call-managed",
                "model": "gpt-5.6-sol",
                "name": "exec_command",
                "input": {"cmd":"printf 'private-host\\n' > hosted-proof.txt && cat hosted-proof.txt"},
                "output_token_budget": 1024,
                "output_byte_budget": 131072,
                "deadline_at": 9_000_000_000_000_u64
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let Some(Ok(Message::Text(result))) = socket.recv().await else {
        return;
    };
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["type"], "result");
    assert_eq!(result["call_id"], "call-managed");
    assert_eq!(result["outcome"]["status"], "completed");
    assert_eq!(result["outcome"]["output"]["success"], true, "{result}");
    assert_eq!(
        result["outcome"]["output"]["success"], true,
        "hosted tool failed: {result}",
    );
    assert!(
        result["outcome"]["output"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("private-host")),
        "hosted tool output omitted proof: {result}",
    );
    socket
        .send(Message::Text(
            serde_json::json!({
                "type": "ack",
                "call_id": "call-managed"
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    state.tool_completed.notify_one();
    serve_until_drain(&mut socket).await;
}

async fn serve_until_drain(socket: &mut WebSocket) {
    while let Some(Ok(Message::Text(frame))) = socket.recv().await {
        let frame: serde_json::Value = serde_json::from_str(&frame).unwrap();
        match frame["type"].as_str() {
            Some("ping") => socket
                .send(Message::Text(
                    serde_json::json!({"type":"pong","nonce":frame["nonce"]})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap(),
            Some("drain") => {
                socket
                    .send(Message::Text(
                        serde_json::json!({"type":"draining"}).to_string().into(),
                    ))
                    .await
                    .unwrap();
                return;
            }
            kind => panic!("unexpected executor frame after result: {kind:?}"),
        }
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
        if state.disconnect_after_ready {
            // The reconnect may finish before the CLI submits its prompt. Do
            // not publish events for the fixture's turn until that turn
            // exists; otherwise the driver correctly treats them as retained
            // session events instead of routing them to the pending turn.
            state.completed.notified().await;
            state.tool_completed.notified().await;
            while state.tool_host_attempts.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        } else if state.expect_local_tool {
            state.tool_completed.notified().await;
        } else {
            state.completed.notified().await;
            while state.tool_host_attempts.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        }
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
