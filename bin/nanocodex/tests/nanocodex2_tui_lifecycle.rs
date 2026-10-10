//! Exercise real terminal input against a controlled managed service.

use std::{
    collections::HashMap,
    io::{Read, Write},
    path::Path,
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
    routing::{get, patch, post, put},
};
use base64::Engine as _;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

const AGENT: &str = "019fc927-b280-79a7-8445-1b9996ad2fb0";
const REMOTE_TURN: &str = "019fc927-b281-79a7-8445-1b9996ad2fb0";
const VAULT_ID: &str = "abcdefghijklmnopqrstuv";
const VAULT_ORIGIN: &str = "https://vault-approval.example:8443";
const SECURE_INPUT_ID: &str = "cbbfa5ef-2e4b-45f7-9c98-3913f8ca87cf";
const TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_empty_idle_stops_redrawing_and_still_accepts_input_and_live_updates() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.wait_no_text("Connecting").await;
    // Let presentation setup and the final ready frame reach the PTY reader.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = fixture.terminal.output.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        fixture.terminal.output.lock().unwrap().len(),
        before,
        "a ready empty terminal must not emit decorative animation frames"
    );

    fixture.terminal.input("IDLE_WAKE_INPUT");
    fixture.terminal.wait_text("IDLE_WAKE_INPUT").await;
    fixture.terminal.input("\r");
    let turn = fixture.submission("IDLE_WAKE_INPUT").await;
    fixture.nested(&turn, "assistant.delta", json!({"model_call_index": 1, "item_id": "idle-answer", "phase": "final_answer", "text": "LIVE_AFTER_IDLE"}));
    fixture.terminal.wait_text("LIVE_AFTER_IDLE").await;
    fixture.complete(&turn);
    fixture.terminal.wait_text("Enter send").await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_discovery_omits_sigkill_orphans_even_with_a_recycled_pid() {
    use std::{os::unix::fs::FileTypeExt, process::Stdio};
    use tokio::io::AsyncWriteExt;

    async fn cli(home: &Path, args: &[&str], input: &[u8]) -> std::process::Output {
        let started = std::time::Instant::now();
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex"))
            .args(args)
            .env("CODEX_HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let output = tokio::time::timeout(TIMEOUT, async {
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(input).await.unwrap();
            drop(stdin);
            child.wait_with_output().await.unwrap()
        })
        .await
        .expect("TUI discovery/control CLI did not finish within its deadline");
        eprintln!(
            "CODEX_HOME={} nanocodex2 {} ({:?}): status={}\ninput={}\nstdout={}\nstderr={}",
            home.display(),
            args.join(" "),
            started.elapsed(),
            output.status,
            String::from_utf8_lossy(input),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        output
    }

    async fn list(home: &Path) -> Vec<Value> {
        let output = cli(home, &["tui", "list", "--json"], b"").await;
        assert!(output.status.success());
        let registrations: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            registrations
                .iter()
                .all(|registration| registration.get("auth_token").is_none()),
            "discovery must never expose authentication tokens"
        );
        registrations
    }

    async fn only_survivor(home: &Path, survivor: &Value) {
        let registrations = list(home).await;
        assert_eq!(
            registrations.len(),
            1,
            "only the healthy TUI must be listed"
        );
        assert_eq!(registrations[0]["instance_id"], survivor["instance_id"]);
        assert_eq!(registrations[0]["pid"], survivor["pid"]);
    }

    let mut fixture = Fixture::start().await;
    let home = fixture.terminal._workspace.path().join(".codex");
    let registry = home.join("nanocodex/tui/instances");
    let before = list(&home).await;
    assert_eq!(before.len(), 1, "the healthy TUI must be discoverable");
    let survivor = &before[0];
    fixture.terminal.input("DISCOVERY_SURVIVOR_DRAFT");
    fixture.terminal.wait_text("DISCOVERY_SURVIVOR_DRAFT").await;

    let mut orphan = Terminal::start_with_command(&fixture.origin, false, None, |command| {
        command.env("CODEX_HOME", &home);
        command.env("NANOCODEX_TUI_CONTROL", "on");
    });
    orphan.wait_text("actions").await;
    let both = list(&home).await;
    assert_eq!(both.len(), 2, "both running TUIs must be discoverable");
    let mut registration = both
        .into_iter()
        .find(|value| value["instance_id"] != survivor["instance_id"])
        .unwrap();
    let instance = registration["instance_id"].as_str().unwrap().to_owned();
    let socket = std::path::PathBuf::from(registration["socket_path"].as_str().unwrap());
    let path = registry.join(format!("{instance}.json"));

    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(registration["pid"].as_u64().unwrap() as i32),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            if orphan.child.try_wait().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("SIGKILL fixture TUI did not exit");
    assert!(
        std::fs::symlink_metadata(&socket)
            .unwrap()
            .file_type()
            .is_socket()
    );
    assert!(
        path.is_file(),
        "SIGKILL must leave a real orphan registration"
    );
    eprintln!(
        "SIGKILL left registration={} and socket={}",
        path.display(),
        socket.display()
    );

    let refused = cli(&home, &["tui", "connect", &instance, "--stdio"], b"").await;
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("Connection refused"));
    only_survivor(&home, survivor).await;

    // Simulate PID recycling only in this fixture's registry, without waiting
    // for OS PID churn. Preserve the server-generated private token and mode.
    registration = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    registration["pid"] = survivor["pid"].clone();
    let recycled = serde_json::to_vec(&registration).unwrap();
    std::fs::write(&path, &recycled).unwrap();
    eprintln!(
        "simulated recycled PID={} for orphan instance={instance}",
        survivor["pid"]
    );
    only_survivor(&home, survivor).await;
    assert_eq!(
        std::fs::read(&path).unwrap(),
        recycled,
        "discovery must not delete or rewrite the orphan"
    );

    std::fs::remove_file(&socket).unwrap();
    std::fs::remove_dir(socket.parent().unwrap()).unwrap();
    eprintln!("removed orphan socket; discovery must still preserve the healthy TUI");
    only_survivor(&home, survivor).await;
    assert_eq!(std::fs::read(&path).unwrap(), recycled);

    let controlled = cli(
        &home,
        &[
            "tui",
            "connect",
            survivor["instance_id"].as_str().unwrap(),
            "--stdio",
        ],
        b"{\"id\":\"survivor\",\"method\":\"state.get\"}\n",
    )
    .await;
    assert!(
        controlled.status.success(),
        "the surviving TUI must remain controllable"
    );
    let state = String::from_utf8(controlled.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|response| response["id"] == "survivor")
        .expect("public connect did not return the survivor state");
    assert_eq!(state["result"]["instance_id"], survivor["instance_id"]);
    assert_eq!(
        state["result"]["state"]["composer"]["text"],
        "DISCOVERY_SURVIVOR_DRAFT"
    );

    fixture.terminal.input("\x03");
    fixture
        .terminal
        .wait_no_text("DISCOVERY_SURVIVOR_DRAFT")
        .await;
    fixture.terminal.input("\x03\x03");
    fixture.terminal.wait_output("\x1b[?1049l").await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_control_keeps_local_root_discoverable_and_stops_it_after_runtime_restart() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

    async fn snapshot(
        write: &mut OwnedWriteHalf,
        lines: &mut Lines<BufReader<OwnedReadHalf>>,
        cursor: u64,
    ) -> Value {
        let expected_cursor = cursor.to_string();
        tokio::time::timeout(TIMEOUT, async {
            loop {
                write
                    .write_all(b"{\"id\":\"observe\",\"method\":\"state.get\"}\n")
                    .await
                    .unwrap();
                let response: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                let snapshot = response["result"].clone();
                if snapshot["state"]["managed_cursor"].as_str() == Some(expected_cursor.as_str()) {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("control state did not reach the durable event cursor")
    }

    for keyboard_stop in [true, false] {
        let mut fixture = Fixture::start().await;
        fixture.terminal.prompt("LOCAL_ROOT_RESTART_JOURNEY", "\r");
        let turn = fixture.submission("LOCAL_ROOT_RESTART_JOURNEY").await;
        fixture.nested(
            &turn,
            "run.started",
            json!({"turn_id":"runtime-before-restart"}),
        );
        fixture.terminal.wait_text("Enter steer").await;

        let registry = fixture
            .terminal
            ._workspace
            .path()
            .join(".codex/nanocodex/tui/instances");
        let registration_path = std::fs::read_dir(registry)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .unwrap();
        let registration: Value = tokio::time::timeout(TIMEOUT, async {
            loop {
                if let Ok(bytes) = std::fs::read(&registration_path)
                    && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("control registration did not finish writing");
        let socket = tokio::net::UnixStream::connect(registration["socket_path"].as_str().unwrap())
            .await
            .unwrap();
        let (read, mut write) = socket.into_split();
        let mut lines = BufReader::new(read).lines();
        write
            .write_all(
                format!(
                    "{}\n",
                    json!({"protocol_version":1,
        "instance_id":registration["instance_id"],"auth_token":registration["auth_token"]})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let _: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let accepted = snapshot(&mut write, &mut lines, fixture.cursor).await;
        eprintln!("local acceptance: {accepted}");
        assert_eq!(accepted["state"]["execution"], "running");
        assert_eq!(accepted["state"]["active_turn_ids"], json!([turn]));
        assert_eq!(accepted["active_turns"][AGENT], json!([turn]));

        let child = "019fc927-b282-79a7-8445-1b9996ad2fb0";
        for (seq, kind, payload) in [
            (1, "run.started", json!({"turn_id":"child-runtime"})),
            (
                2,
                "run.failed",
                json!({"turn_id":"child-runtime","status":"failed","error":"synthetic child failure"}),
            ),
        ] {
            fixture.emit(
                &turn,
                json!({"type":"event","agent_id":1,"event":{
            "protocol_version":1,"request_id":child,"seq":seq,"type":kind,"payload":payload}}),
            );
        }
        // A service runtime reconstruction changes nested IDs and resets its sequence,
        // but preserves the accepted durable turn and the terminal's connection.
        fixture.emit(
            &turn,
            json!({"type":"event","event":{
        "protocol_version":1,"request_id":AGENT,"seq":1,"type":"input.accepted",
        "payload":{"session_id":AGENT,"turn_id":"runtime-after-restart",
            "item_id":"runtime-after-restart:prompt","kind":"prompt","request_id":turn,
            "input":"LOCAL_ROOT_RESTART_JOURNEY"}}}),
        );
        fixture.emit(
            &turn,
            json!({"type":"event","event":{
        "protocol_version":1,"request_id":AGENT,"seq":2,"type":"run.started",
        "payload":{"turn_id":"runtime-after-restart"}}}),
        );
        fixture.nested(&turn, "tool.call", json!({"turn_id":"runtime-after-restart",
        "call_id":"held-root-command","tool":"exec_command","arguments":{"cmd":"printf synthetic"}}));
        let restarted = snapshot(&mut write, &mut lines, fixture.cursor).await;
        eprintln!("after child failure and root reconstruction: {restarted}");
        assert_eq!(restarted["state"]["connection"], "ready");
        assert_eq!(restarted["state"]["execution"], "running");
        assert_eq!(restarted["state"]["active_turn_ids"], json!([turn]));
        assert_eq!(restarted["active_turns"][AGENT], json!([turn]));
        assert_eq!(restarted["active_turns"][child], json!([]));

        if keyboard_stop {
            fixture.terminal.input("\x1b");
            fixture.terminal.wait_text("Interrupt").await;
            fixture.terminal.input("\x1b");
        } else {
            let request = json!({"id":"cancel-discovered-local-root","method":"cancel","params":{
            "expected_instance_id":restarted["instance_id"],
            "expected_session_id":restarted["active_session_id"],
            "expected_active_generation":restarted["active_generation"],
            "expected_turn_id":restarted["state"]["active_turn_ids"][0]}});
            write
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            let response: Value = serde_json::from_str(
                &tokio::time::timeout(TIMEOUT, lines.next_line())
                    .await
                    .expect("control cancel never returned a receipt")
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            eprintln!("control cancellation request: {request}; receipt: {response}");
            assert_eq!(response["result"]["status"], "accepted");
            assert_eq!(response["result"]["result"]["turn_id"], turn);
            assert_eq!(response["result"]["result"]["state"], "cancelling");
        }
        let cancelled = tokio::time::timeout(TIMEOUT, fixture.cancellations.recv())
            .await
            .expect("confirmed Stop never reached the service")
            .unwrap();
        eprintln!(
            "Stop HTTP cancellation target: {cancelled}; expected durable root: {turn}; keyboard={keyboard_stop}"
        );
        assert_eq!(cancelled, turn);
        if keyboard_stop {
            fixture.terminal.wait_text("Interrupted response").await;
        }
        let cancelling = snapshot(&mut write, &mut lines, fixture.cursor).await;
        assert_eq!(cancelling["state"]["active_turn_ids"], json!([turn]));
        fixture.nested(&turn, "tool.result", json!({"turn_id":"runtime-after-restart",
        "call_id":"held-root-command","tool":"exec_command","status":"cancelled","duration_ns":1,"result":null}));
        fixture.nested(
            &turn,
            "run.failed",
            json!({"turn_id":"runtime-after-restart","status":"cancelled"}),
        );
        fixture.emit(&turn, json!({"type":"turn_cancelled","id":turn}));
        fixture.terminal.wait_text("Enter send").await;
        let finished = snapshot(&mut write, &mut lines, fixture.cursor).await;
        eprintln!(
            "durable cancellation: {finished}; history: {}",
            json!(*fixture.history.lock().unwrap())
        );
        assert_eq!(finished["state"]["execution"], "idle");
        assert_eq!(finished["state"]["active_turn_ids"], json!([]));
        assert_eq!(finished["active_turns"][AGENT], json!([]));
        assert!(
            fixture.cancellations.try_recv().is_err(),
            "Stop must cancel the root only once"
        );
        fixture.terminal.prompt("RECOVERY_AFTER_ROOT_STOP", "\r");
        let next = fixture.submission("RECOVERY_AFTER_ROOT_STOP").await;
        fixture.complete(&next);
        fixture.terminal.wait_text("Enter send").await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_control_discovers_preserves_draft_and_deduplicates_prompt() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut fixture = Fixture::start().await;
    fixture.terminal.input("unfinished local draft");
    fixture.terminal.wait_text("unfinished local draft").await;
    let registry = fixture
        .terminal
        ._workspace
        .path()
        .join(".codex/nanocodex/tui/instances");
    let path = std::fs::read_dir(registry)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let registration: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let socket = tokio::net::UnixStream::connect(registration["socket_path"].as_str().unwrap())
        .await
        .unwrap();
    let (read, mut write) = socket.into_split();
    let mut lines = BufReader::new(read).lines();
    write.write_all(format!("{}\n",json!({"protocol_version":1,"instance_id":registration["instance_id"],"auth_token":registration["auth_token"]})).as_bytes()).await.unwrap();
    let hello: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    let snapshot = &hello["snapshot"];
    assert_eq!(
        snapshot["state"]["composer"]["text"],
        "unfinished local draft"
    );
    let request = json!({"id":"external-prompt","method":"prompt","params":{
        "expected_instance_id":registration["instance_id"],"expected_session_id":AGENT,
        "expected_active_generation":snapshot["active_generation"],"input":{"text":"external literal prompt"}}});
    write
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let turn = fixture.submission("external literal prompt").await;
    let reply: Value = serde_json::from_str(
        &tokio::time::timeout(TIMEOUT, lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(reply["result"]["status"], "accepted");
    write
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let replay: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(replay, reply);
    assert!(fixture.submissions.try_recv().is_err());
    write
        .write_all(b"{\"id\":\"state\",\"method\":\"state.get\"}\n")
        .await
        .unwrap();
    let state: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(
        state["result"]["state"]["composer"]["text"],
        "unfinished local draft"
    );
    fixture.terminal.wait_text("external literal prompt").await;
    let mut steer = request.clone();
    steer["id"] = json!("external-steer");
    steer["method"] = json!("steer");
    steer["params"]["expected_turn_id"] = json!(turn);
    steer["params"]["input"]["text"] = json!("external correction");
    write
        .write_all(format!("{steer}\n").as_bytes())
        .await
        .unwrap();
    let (input, ack) = tokio::time::timeout(TIMEOUT, async {
        tokio::select! {
            command = fixture.steers.recv() => command.unwrap(),
            response = lines.next_line() => panic!("steer was resolved before backend admission: {response:?}"),
        }
    }).await.unwrap();
    assert_eq!(input["message_id"], "external-steer");
    assert_eq!(input["turn_id"], turn);
    ack.send(true).unwrap();
    let reply: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(reply["result"]["status"], "accepted");
    write
        .write_all(format!("{steer}\n").as_bytes())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&lines.next_line().await.unwrap().unwrap()).unwrap(),
        reply
    );
    assert!(fixture.steers.try_recv().is_err());
    let mut cancel = steer.clone();
    cancel["id"] = json!("external-cancel");
    cancel["method"] = json!("cancel");
    write
        .write_all(format!("{cancel}\n").as_bytes())
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(TIMEOUT, async {
            tokio::select! {
                command = fixture.cancellations.recv() => command.unwrap(),
                response = lines.next_line() => panic!("cancel was resolved before backend admission: {response:?}"),
            }
        }).await.unwrap(),
        turn
    );
    let reply: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(reply["result"]["status"], "accepted");
    write
        .write_all(format!("{cancel}\n").as_bytes())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&lines.next_line().await.unwrap().unwrap()).unwrap(),
        reply
    );
    assert!(fixture.cancellations.try_recv().is_err());
    fixture.complete(&turn);
    fixture.terminal.wait_text("unfinished local draft").await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_tmux_hint_keeps_terminal_usable_and_reaps_helper() {
    use std::os::unix::fs::PermissionsExt;
    let helper = tempfile::tempdir().unwrap();
    let executable = helper.path().join("tmux");
    let pid_file = helper.path().join("tmux.pids");
    std::fs::write(
        &executable,
        "#!/bin/sh\nprintf '%s %s\\n' \"$$\" \"$1\" >> \"$NANOCODEX_TEST_TMUX_PID\"\nexec /bin/sleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(helper.path().to_path_buf()).chain(std::env::split_paths(&inherited_path)),
    )
    .unwrap();
    let started = std::time::Instant::now();
    let mut terminal = Terminal::start_with_command("http://127.0.0.1:9", false, None, |command| {
        command.env("TMUX", "nanocodex-test-stalled-tmux");
        command.env("TMUX_PANE", "%0");
        command.env("PATH", path);
        command.env("NANOCODEX_TEST_TMUX_PID", &pid_file);
    });
    tokio::time::timeout(Duration::from_secs(3), terminal.wait_text("actions"))
        .await
        .expect("stalled tmux must not block the first frame");
    let first_frame = started.elapsed();
    let helpers = || {
        std::fs::read_to_string(&pid_file)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let (pid, command) = line.split_once(' ')?;
                Some((pid.to_owned(), command.to_owned()))
            })
            .collect::<Vec<_>>()
    };
    // Keep editing through the initial and subsequent two-second publication
    // ticks. Slow process launch can consume the publisher's 250ms budget before
    // the shell writes its PID, so that log cannot be a publication barrier.
    let editing = std::time::Instant::now();
    let mut sample = 0;
    let mut max_input_echo = Duration::ZERO;
    while editing.elapsed() < Duration::from_millis(2500) {
        let draft = format!("TMUX_STARTUP_DRAFT_{sample:03}");
        let input = std::time::Instant::now();
        terminal.input(&format!("\x15{draft}"));
        tokio::time::timeout(Duration::from_millis(150), terminal.wait_text(&draft))
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "stalled tmux publication delayed editable input on sample {sample} after {:?}",
                    editing.elapsed()
                )
            });
        max_input_echo = max_input_echo.max(input.elapsed());
        sample += 1;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    terminal.input("\x03");
    terminal.wait_no_text("TMUX_STARTUP_DRAFT").await;
    let closing = std::time::Instant::now();
    terminal.input("\x03\x03");
    let status = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(status) = terminal.child.try_wait().unwrap() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("terminal must close without a lingering tmux helper");
    assert!(status.success());
    terminal.wait_output("\x1b[?1049l").await;
    let close = closing.elapsed();
    let helpers = helpers();
    assert!(
        helpers
            .iter()
            .any(|(_, command)| command == "display-message")
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let all_reaped = helpers.iter().all(|(pid, _)| {
                !std::process::Command::new("/bin/kill")
                    .args(["-0", pid])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            });
            if all_reaped {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("every timed-out or cancelled tmux helper must be killed and reaped");
    eprintln!(
        "stalled tmux: first frame={first_frame:?}, max input echo={max_input_echo:?} ({sample} samples), close={close:?}"
    );
}

/// Accountless `nanocodex --local` drives the unified TUI with only the external
/// Responses socket stubbed: `--prompt` is admitted exactly once and streams its
/// reply, the native tui-control session is discoverable and idle, and the
/// managed-only /share command explains the account requirement without model
/// input or managed HTTP. Local /thinking and /fast then reach the next
/// Responses request, Esc Esc cancels a held generation, and the session stays
/// usable for another prompt. Evidence: output/local-tui-journey/<run>/.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_local_prompt_replies_once_and_rejects_account_commands_without_http() {
    use futures_util::{SinkExt as _, StreamExt as _};
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
    use tokio_tungstenite::tungstenite::Message as Frame;

    const PROMPT: &str = "hello from the local journey";
    const REPLY: &str = "LOCAL_PROMPT_REPLY";
    const LIMIT: Duration = Duration::from_secs(30);
    const SETTINGS_PROMPT: &str = "prompt after local settings";
    const SETTINGS_REPLY: &str = "SETTINGS_APPLIED_REPLY";
    const HELD_PROMPT: &str = "prompt that the provider holds";
    const HELD_PARTIAL: &str = "HELD_PARTIAL_OUTPUT";
    const AFTER_PROMPT: &str = "prompt after cancelling";
    const AFTER_REPLY: &str = "AFTER_CANCEL_REPLY";
    const STEER_KEYS: &str = "steer typed while the reply is held";
    const STEER_CONTROL: &str = "steer sent over native control";
    const STEER_REPLY: &str = "STEERED_FOLLOWUP_REPLY";
    // Generation 2 is held until steering arrives and the fixture releases it;
    // generation 4 is held until the client cancels or sends again.
    const STEER_HELD: usize = 2;
    const CANCEL_HELD: usize = 4;
    let artifact = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/local-tui-journey")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&artifact).unwrap();

    // The only external dependency: a loopback Responses websocket that
    // answers warmups empty and every generation with one streamed reply.
    let responses = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", responses.local_addr().unwrap());
    let generations = Arc::new(Mutex::new(Vec::<Value>::new()));
    let recorded = generations.clone();
    let released = Arc::new(Mutex::new(Vec::<Value>::new()));
    let release_record = released.clone();
    let release_steer = Arc::new(tokio::sync::Notify::new());
    let steer_release = release_steer.clone();
    // Every connection and frame, so a silent stall shows whether the agent
    // dialed the provider, warmed up, or never generated.
    let observed = Arc::new(Mutex::new(json!({"connections":0,"warmups":0,"frames":[]})));
    let observe = observed.clone();
    let provider = tokio::spawn(async move {
        while let Ok((stream, _)) = responses.accept().await {
            let recorded = recorded.clone();
            let release_record = release_record.clone();
            let steer_release = steer_release.clone();
            let observe = observe.clone();
            {
                let mut observed = observe.lock().unwrap();
                let connections = observed["connections"].as_u64().unwrap_or(0) + 1;
                observed["connections"] = connections.into();
            }
            tokio::spawn(async move {
                let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                let mut pending = None;
                loop {
                    let frame = match pending.take() {
                        Some(frame) => frame,
                        None => match socket.next().await {
                            Some(Ok(frame)) => frame,
                            _ => return,
                        },
                    };
                    let Frame::Text(text) = frame else {
                        continue;
                    };
                    let request: Value = serde_json::from_str(text.as_str()).unwrap();
                    {
                        let mut observed = observe.lock().unwrap();
                        if request["generate"] == false {
                            let warmups = observed["warmups"].as_u64().unwrap_or(0) + 1;
                            observed["warmups"] = warmups.into();
                        }
                        observed["frames"].as_array_mut().unwrap().push(json!({
                            "type":request["type"],"generate":request["generate"],"model":request["model"]}));
                    }
                    let generation = (request["generate"] != false).then(|| {
                        let mut recorded = recorded.lock().unwrap();
                        recorded.push(request.clone());
                        recorded.len() - 1
                    });
                    if let Some(held @ (STEER_HELD | CANCEL_HELD)) = generation {
                        for event in [
                            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg_held","role":"assistant","content":[]}}),
                            json!({"type":"response.output_text.delta","output_index":0,"delta":HELD_PARTIAL}),
                        ] {
                            if socket
                                .send(Frame::Text(event.to_string().into()))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        // Only the test releases the steering hold; otherwise
                        // record how the client released the stream.
                        let next = tokio::select! {
                            () = steer_release.notified(), if held == STEER_HELD => None,
                            next = socket.next() => Some(next),
                        };
                        let Some(next) = next else {
                            release_record
                                .lock()
                                .unwrap()
                                .push(json!({"generation":held,"released":"fixture"}));
                            let item = json!({"type":"message","id":"msg_held","role":"assistant","status":"completed",
                                "content":[{"type":"output_text","text":HELD_PARTIAL,"annotations":[]}]});
                            for event in [
                                json!({"type":"response.output_item.done","output_index":0,"item":item}),
                                json!({"type":"response.completed","response":{"id":"resp_held","status":"completed","output":[item],
                                    "usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,
                                        "output_tokens_details":{"reasoning_tokens":0},"total_tokens":2}}}),
                            ] {
                                if socket
                                    .send(Frame::Text(event.to_string().into()))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            continue;
                        };
                        let closed = !matches!(next, Some(Ok(Frame::Text(_))));
                        release_record
                            .lock()
                            .unwrap()
                            .push(json!({"generation":held,
                            "released":if closed { "closed" } else { "next_request" }}));
                        if closed {
                            return;
                        }
                        pending = next.and_then(Result::ok);
                        continue;
                    }
                    let reply = match generation {
                        Some(0) => REPLY,
                        Some(1) => SETTINGS_REPLY,
                        Some(3) => STEER_REPLY,
                        _ => AFTER_REPLY,
                    };
                    let (head, tail) = reply.split_at(reply.len() / 2);
                    let item = json!({"type":"message","id":"msg_local","role":"assistant","status":"completed",
                        "content":[{"type":"output_text","text":reply,"annotations":[]}]});
                    let output = if generation.is_none() {
                        vec![]
                    } else {
                        for event in [
                            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg_local","role":"assistant","content":[]}}),
                            json!({"type":"response.output_text.delta","output_index":0,"delta":head}),
                            json!({"type":"response.output_text.delta","output_index":0,"delta":tail}),
                            json!({"type":"response.output_item.done","output_index":0,"item":item}),
                        ] {
                            if socket
                                .send(Frame::Text(event.to_string().into()))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        vec![item]
                    };
                    let completed = json!({"type":"response.completed","response":{"id":"resp_local","status":"completed","output":output,
                        "usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,
                            "output_tokens_details":{"reasoning_tokens":0},"total_tokens":2}}});
                    if socket
                        .send(Frame::Text(completed.to_string().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    // Any managed HTTP from the accountless TUI would connect here.
    let managed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    managed.set_nonblocking(true).unwrap();
    let managed_origin = format!("http://{}", managed.local_addr().unwrap());

    let mut terminal = Terminal::start_with_command(&managed_origin, false, None, |command| {
        command.env_remove("NANOCODEX_API_KEY");
        command.args([
            "--local",
            "--prompt",
            PROMPT,
            "--model",
            "gpt-6.1-sol",
            "--api-key",
            "synthetic-openai-key",
            "--websocket-url",
            endpoint.as_str(),
            "--browser=none",
            "--mcp-defaults",
            "false",
            "--mcp-codex-config",
            "false",
            "--web-search",
            "false",
            "--image-generation",
            "false",
            "--memory",
            "false",
            "--subagents",
            "false",
        ]);
    });
    let workspace = terminal._workspace.path().to_path_buf();
    let screen = |terminal: &Terminal| terminal.screen.lock().unwrap().screen().contents();
    let evidence = |terminal: &Terminal, step: &str| {
        std::fs::write(
            artifact.join(format!("{step}.screen.txt")),
            screen(terminal),
        )
        .unwrap();
        std::fs::write(
            artifact.join("terminal.log"),
            terminal.output.lock().unwrap().as_slice(),
        )
        .unwrap();
        std::fs::write(
            artifact.join("provider.json"),
            serde_json::to_vec_pretty(&*generations.lock().unwrap()).unwrap(),
        )
        .unwrap();
        std::fs::write(
            artifact.join("provider-connections.json"),
            serde_json::to_vec_pretty(&*observed.lock().unwrap()).unwrap(),
        )
        .unwrap();
        // The CLI's own TUI logs and rollouts live under the temporary HOME,
        // which is removed with the terminal; keep them beside the evidence.
        let logs = artifact.join("logs");
        let mut directories = vec![workspace.clone()];
        while let Some(directory) = directories.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    directories.push(path);
                } else if path
                    .extension()
                    .is_some_and(|extension| extension == "log" || extension == "jsonl")
                {
                    let name = path
                        .strip_prefix(&workspace)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('/', "__");
                    std::fs::create_dir_all(&logs).unwrap();
                    let _ = std::fs::copy(&path, logs.join(name));
                }
            }
        }
        std::fs::write(
            artifact.join("held-release.json"),
            serde_json::to_vec_pretty(&*released.lock().unwrap()).unwrap(),
        )
        .unwrap();
    };
    std::fs::write(
        artifact.join("scenario.json"),
        serde_json::to_vec_pretty(&json!({
            "reproduce":"cargo test --locked -p nanocodex-bin --test nanocodex2_tui_lifecycle terminal_local_prompt_replies_once_and_rejects_account_commands_without_http -- --nocapture",
            "command":["nanocodex","--local","--prompt",PROMPT,"--websocket-url",&endpoint],
            "expected":["one generation containing the --prompt text","streamed reply visible","native control session idle","/share rejected as needing an account","no model input or managed HTTP for /share","/thinking high and /fast on reach control state and the next Responses request","Enter and native control steer a held generation; its follow-up request carries both","Esc Esc cancels a held generation and returns to idle","a later prompt completes","Ctrl+C Ctrl+C exits successfully"],
            "boundary":"shipped nanocodex executable in a 160x32 PTY without an account; only the external Responses websocket is a loopback fixture"
        }))
        .unwrap(),
    )
    .unwrap();
    eprintln!("journey evidence: {}", artifact.display());

    let deadline = std::time::Instant::now() + LIMIT;
    while !screen(&terminal).contains(REPLY) {
        if std::time::Instant::now() > deadline {
            evidence(&terminal, "reply-timeout");
            panic!(
                "--prompt reply never rendered; evidence {}",
                artifact.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // Give a duplicated startup submission time to reach the provider.
    tokio::time::sleep(Duration::from_millis(500)).await;
    evidence(&terminal, "reply");
    {
        let generations = generations.lock().unwrap();
        assert_eq!(
            generations.len(),
            1,
            "--prompt must be admitted exactly once; evidence {}",
            artifact.display()
        );
        assert!(
            generations[0].to_string().contains(PROMPT),
            "{}",
            generations[0]
        );
    }
    assert_eq!(
        screen(&terminal).matches(PROMPT).count(),
        1,
        "{}",
        screen(&terminal)
    );
    assert_eq!(
        screen(&terminal).matches(REPLY).count(),
        1,
        "{}",
        screen(&terminal)
    );

    // A second client finds the native control registration for this session.
    let instances = workspace.join(".codex/nanocodex/tui/instances");
    let registration = loop {
        let found = std::fs::read_dir(&instances).ok().and_then(|mut entries| {
            let path = entries.next()?.ok()?.path();
            serde_json::from_slice::<Value>(&std::fs::read(path).ok()?).ok()
        });
        if let Some(value) = found.filter(|value| value["active_session_id"].is_string()) {
            break value;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no native control registration in {}",
            instances.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    std::fs::write(artifact.join("registration.json"), serde_json::to_vec_pretty(&json!({
        "instance_id":registration["instance_id"],"active_session_id":registration["active_session_id"]})).unwrap()).unwrap();
    let socket = tokio::net::UnixStream::connect(registration["socket_path"].as_str().unwrap())
        .await
        .unwrap();
    let (read, mut write) = socket.into_split();
    let mut lines = tokio::io::BufReader::new(read).lines();
    let auth = json!({"protocol_version":1,"instance_id":registration["instance_id"],"auth_token":registration["auth_token"]});
    write
        .write_all(format!("{auth}\n").as_bytes())
        .await
        .unwrap();
    let hello: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert!(hello.get("snapshot").is_some(), "{hello}");
    let mut requests = 0_u32;
    let state = control_state(&mut lines, &mut write, &mut requests).await;
    std::fs::write(
        artifact.join("control-state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();
    assert_eq!(
        state["active_session_id"], registration["active_session_id"],
        "{state}"
    );
    assert_eq!(state["state"]["execution"], "idle", "{state}");

    // Managed-only commands explain the account requirement and stay local.
    terminal.prompt("/share", "\r");
    let deadline = std::time::Instant::now() + LIMIT;
    while !screen(&terminal).contains("/share needs a Nanocodex account session") {
        if std::time::Instant::now() > deadline {
            evidence(&terminal, "share-timeout");
            panic!(
                "/share did not explain the account requirement; evidence {}",
                artifact.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    evidence(&terminal, "share");
    assert_eq!(
        generations.lock().unwrap().len(),
        1,
        "/share must not become model input"
    );
    assert!(
        matches!(managed.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "the accountless TUI contacted the managed service"
    );

    // Local settings: the control session observes them and the next
    // generation carries them to the provider.
    // Change both settings away from the session's starting values so the
    // next request can only carry them if the commands reached the agent.
    let initial_effort = state["state"]["settings"]["effort"].clone();
    let fast = state["state"]["settings"]["fast_mode"] != true;
    assert_ne!(
        initial_effort, "high",
        "the journey needs a non-high starting effort: {state}"
    );
    assert_eq!(
        generations.lock().unwrap()[0]["service_tier"] == "priority",
        !fast,
        "the first request must reflect the starting fast mode; evidence {}",
        artifact.display()
    );
    terminal.prompt("/thinking high", "\r");
    tokio::time::sleep(Duration::from_millis(200)).await;
    terminal.prompt(if fast { "/fast on" } else { "/fast off" }, "\r");
    let deadline = std::time::Instant::now() + LIMIT;
    let settings = loop {
        let state = control_state(&mut lines, &mut write, &mut requests).await;
        let settings = &state["state"]["settings"];
        if settings["effort"] == "high" && settings["fast_mode"] == fast {
            break state;
        }
        if std::time::Instant::now() > deadline {
            evidence(&terminal, "settings-timeout");
            panic!(
                "local /thinking high and /fast {fast} never reached control state: {state}; evidence {}",
                artifact.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    std::fs::write(
        artifact.join("control-settings.json"),
        serde_json::to_vec_pretty(&settings).unwrap(),
    )
    .unwrap();
    assert_eq!(
        generations.lock().unwrap().len(),
        1,
        "settings commands must not become model input"
    );
    terminal.prompt(SETTINGS_PROMPT, "\r");
    let deadline = std::time::Instant::now() + LIMIT;
    while !screen(&terminal).contains(SETTINGS_REPLY) {
        if std::time::Instant::now() > deadline {
            evidence(&terminal, "settings-reply-timeout");
            panic!(
                "prompt after settings never answered; evidence {}",
                artifact.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    evidence(&terminal, "settings-reply");
    {
        let generations = generations.lock().unwrap();
        assert_eq!(generations.len(), 2, "evidence {}", artifact.display());
        assert!(
            generations[1].to_string().contains(SETTINGS_PROMPT),
            "{}",
            generations[1]
        );
        assert_eq!(
            effective_effort(&generations[1]),
            "high",
            "/thinking high must reach Responses (control state already reported high; first request {}); evidence {}",
            effective_effort(&generations[0]),
            artifact.display()
        );
        assert_eq!(
            generations[1]["service_tier"] == "priority",
            fast,
            "/fast {} must reach Responses (first request {}, next {}); evidence {}",
            if fast { "on" } else { "off" },
            generations[0]["service_tier"],
            generations[1]["service_tier"],
            artifact.display()
        );
    }

    // Steer a held generation from the keyboard and over native control; once
    // the provider completes it, the follow-up request carries both inputs.
    let deadline = std::time::Instant::now() + LIMIT;
    while control_state(&mut lines, &mut write, &mut requests).await["state"]["execution"] != "idle"
    {
        assert!(
            std::time::Instant::now() < deadline,
            "settings turn never settled; evidence {}",
            artifact.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    terminal.prompt(HELD_PROMPT, "\r");
    let deadline = std::time::Instant::now() + LIMIT;
    while !screen(&terminal).contains(HELD_PARTIAL) {
        if std::time::Instant::now() > deadline {
            evidence(&terminal, "steer-held-timeout");
            panic!(
                "held generation for steering never streamed; evidence {}",
                artifact.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let held = control_state(&mut lines, &mut write, &mut requests).await;
    evidence(&terminal, "steer-held");
    assert_ne!(
        held["state"]["execution"], "idle",
        "held generation must be active: {held}"
    );
    terminal.prompt(STEER_KEYS, "\r");
    let deadline = std::time::Instant::now() + LIMIT;
    while !screen(&terminal).contains(STEER_KEYS) {
        if std::time::Instant::now() > deadline {
            evidence(&terminal, "steer-keys-timeout");
            panic!(
                "Enter did not record the typed steer; evidence {}",
                artifact.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let turn = held["state"]["active_turn_ids"][0].clone();
    assert!(
        turn.is_string(),
        "active turn missing from native control: {held}"
    );
    let steered = control_request(
        &mut lines,
        &mut write,
        &mut requests,
        "steer",
        json!({"expected_instance_id":registration["instance_id"],"expected_session_id":held["active_session_id"],
            "expected_active_generation":held["active_generation"],"expected_turn_id":turn,
            "input":{"text":STEER_CONTROL}}),
    )
    .await;
    std::fs::write(
        artifact.join("control-steer.json"),
        serde_json::to_vec_pretty(&steered).unwrap(),
    )
    .unwrap();
    assert_eq!(
        steered["status"],
        "accepted",
        "native control steer: {steered}; evidence {}",
        artifact.display()
    );
    assert_eq!(
        generations.lock().unwrap().len(),
        3,
        "steering must wait for the held generation; evidence {}",
        artifact.display()
    );
    release_steer.notify_one();
    let deadline = std::time::Instant::now() + LIMIT;
    while !screen(&terminal).contains(STEER_REPLY) {
        if std::time::Instant::now() > deadline {
            evidence(&terminal, "steer-reply-timeout");
            panic!(
                "steered follow-up never answered; evidence {}",
                artifact.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    evidence(&terminal, "steer-reply");
    {
        let generations = generations.lock().unwrap();
        assert_eq!(
            generations.len(),
            4,
            "steering must produce one follow-up request; evidence {}",
            artifact.display()
        );
        let followup = generations[3].to_string();
        assert!(
            followup.contains(STEER_KEYS),
            "typed steer missing from follow-up: {followup}"
        );
        assert!(
            followup.contains(STEER_CONTROL),
            "native control steer missing from follow-up: {followup}"
        );
        assert_eq!(
            followup.matches(STEER_KEYS).count(),
            1,
            "typed steer duplicated: {followup}"
        );
        assert_eq!(
            followup.matches(STEER_CONTROL).count(),
            1,
            "control steer duplicated: {followup}"
        );
    }

    // Cancel an active generation the provider never completes.
    let deadline = std::time::Instant::now() + LIMIT;
    while control_state(&mut lines, &mut write, &mut requests).await["state"]["execution"] != "idle"
    {
        assert!(
            std::time::Instant::now() < deadline,
            "steered turn never settled; evidence {}",
            artifact.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    terminal.prompt(HELD_PROMPT, "\r");
    let deadline = std::time::Instant::now() + LIMIT;
    while generations.lock().unwrap().len() < CANCEL_HELD + 1 {
        if std::time::Instant::now() > deadline {
            evidence(&terminal, "held-timeout");
            panic!(
                "held generation never streamed; evidence {}",
                artifact.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let active = control_state(&mut lines, &mut write, &mut requests).await;
    evidence(&terminal, "held");
    assert_ne!(
        active["state"]["execution"], "idle",
        "held generation must be active: {active}"
    );
    terminal.input("\x1b");
    tokio::time::sleep(Duration::from_millis(80)).await;
    terminal.input("\x1b");
    let deadline = std::time::Instant::now() + LIMIT;
    let cancelled = loop {
        let state = control_state(&mut lines, &mut write, &mut requests).await;
        if state["state"]["execution"] == "idle" {
            break state;
        }
        if std::time::Instant::now() > deadline {
            evidence(&terminal, "cancel-timeout");
            panic!(
                "Esc Esc did not cancel the active generation: {state}; evidence {}",
                artifact.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    std::fs::write(
        artifact.join("control-cancelled.json"),
        serde_json::to_vec_pretty(&cancelled).unwrap(),
    )
    .unwrap();
    evidence(&terminal, "cancelled");
    assert_eq!(
        generations.lock().unwrap().len(),
        5,
        "cancel must not resubmit; evidence {}",
        artifact.display()
    );

    // The cancelled session still answers the next prompt exactly once.
    terminal.prompt(AFTER_PROMPT, "\r");
    let deadline = std::time::Instant::now() + LIMIT;
    while !screen(&terminal).contains(AFTER_REPLY) {
        if std::time::Instant::now() > deadline {
            evidence(&terminal, "after-cancel-timeout");
            panic!(
                "prompt after cancel never answered; evidence {}",
                artifact.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    evidence(&terminal, "after-cancel");
    {
        let generations = generations.lock().unwrap();
        assert_eq!(generations.len(), 6, "evidence {}", artifact.display());
        assert!(
            generations[5].to_string().contains(AFTER_PROMPT),
            "{}",
            generations[5]
        );
    }
    assert!(
        released
            .lock()
            .unwrap()
            .iter()
            .any(|release| release["generation"] == CANCEL_HELD),
        "the cancelled provider stream was never released: {:?}",
        released.lock().unwrap()
    );
    assert!(
        matches!(managed.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "the accountless TUI contacted the managed service"
    );

    terminal.input("\x03");
    tokio::time::sleep(Duration::from_millis(100)).await;
    terminal.input("\x03");
    let deadline = std::time::Instant::now() + LIMIT;
    let status = loop {
        if let Some(status) = terminal.child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            evidence(&terminal, "exit-timeout");
            panic!(
                "Ctrl+C Ctrl+C did not exit; evidence {}",
                artifact.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    evidence(&terminal, "exit");
    assert!(
        status.success(),
        "{status:?}; evidence {}",
        artifact.display()
    );
    provider.abort();
}

/// Reads the native tui-control state, skipping interleaved notifications.
async fn control_state(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>,
    write: &mut tokio::net::unix::OwnedWriteHalf,
    requests: &mut u32,
) -> Value {
    control_request(lines, write, requests, "state.get", json!({})).await
}

/// Sends one native tui-control request and returns its result.
async fn control_request(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>,
    write: &mut tokio::net::unix::OwnedWriteHalf,
    requests: &mut u32,
    method: &str,
    params: Value,
) -> Value {
    use tokio::io::AsyncWriteExt as _;
    *requests += 1;
    let id = format!("local-journey-{requests}");
    let request = json!({"id":id,"method":method,"params":params});
    write
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    loop {
        let line = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
            .await
            .expect("control state request stalled")
            .unwrap()
            .expect("control socket closed");
        let frame: Value = serde_json::from_str(&line).unwrap();
        if frame["id"] == id.as_str() {
            return frame["result"].clone();
        }
    }
}

/// The reasoning effort a Responses request asks for. A cache-pinned request
/// keeps its top-level reasoning and appends a newer configuration_update input
/// item instead, so the latest update wins.
fn effective_effort(request: &Value) -> Value {
    request["input"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|item| {
            item["type"] == "configuration_update" && !item["reasoning"]["effort"].is_null()
        })
        .map_or_else(
            || request["reasoning"]["effort"].clone(),
            |item| item["reasoning"]["effort"].clone(),
        )
}

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
        Self::start_with_reload_dir(origin, attach, None)
    }

    fn start_with_reload_dir(origin: &str, attach: bool, reload_dir: Option<&Path>) -> Self {
        Self::start_with_command(origin, attach, reload_dir, |_| {})
    }

    fn start_with_command(
        origin: &str,
        attach: bool,
        reload_dir: Option<&Path>,
        configure: impl FnOnce(&mut CommandBuilder),
    ) -> Self {
        let workspace = tempfile::tempdir().unwrap();
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 32,
                cols: 160,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(
            std::env::var_os("NANOCODEX2_TEST_BINARY")
                .unwrap_or_else(|| env!("CARGO_BIN_EXE_nanocodex").into()),
        );
        command.env_clear();
        command.env("PATH", std::env::var_os("PATH").unwrap_or_default());
        command.env("HOME", workspace.path());
        command.env("NANOCODEX_HOME", workspace.path().join(".nanocodex"));
        if attach {
            command.args(["attach", AGENT]);
        }
        command.cwd(workspace.path());
        command.env("CODEX_HOME", workspace.path().join(".codex"));
        command.env("NANOCODEX_DISABLE_HAND", "1");
        // This PTY is not a tmux client, regardless of the developer's shell.
        // Inheriting TMUX used to skip a broken terminal capability probe.
        command.env_remove("TMUX");
        command.env_remove("TMUX_PANE");
        command.env_remove("TERM_PROGRAM");
        command.env_remove("NANOCODEX2_RELOAD_EXECUTABLE");
        command.env("TERM", "xterm-256color");
        // Exercise terminal clipboard output without changing the developer's
        // native clipboard. This fixture emulates a remote terminal.
        command.env("SSH_TTY", "/dev/nanocodex-test-pty");
        command.env("NANOCODEX_MANAGED_URL", origin);
        // Terminal/API tests must not discover or install the developer’s CUA runtime.
        command.env("NANOCODEX_COMPUTER", "off");
        // Every test terminal gets an isolated registry, even when the caller
        // inherited a real user's reload directory. Only explicit peers share it.
        command.env(
            "NANOCODEX_RELOAD_DIR",
            reload_dir
                .map(Path::to_path_buf)
                .unwrap_or_else(|| workspace.path().join(".reload")),
        );
        command.env(
            "NANOCODEX_API_KEY",
            format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)),
        );
        configure(&mut command);
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

    async fn unlock_private(&mut self) {
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.wait_text("Type safety token (keys only):").await;
        let token = {
            let parser = self.screen.lock().unwrap();
            parser
                .screen()
                .contents()
                .lines()
                .find_map(|line| {
                    line.split_once("Type safety token (keys only): ")
                        .map(|(_, rest)| rest[..32].to_owned())
                })
                .expect("fresh private token")
        };
        self.input(&token);
        self.wait_text("Safety token verified").await;
    }

    async fn wait_output(&self, text: &str) {
        tokio::time::timeout(TIMEOUT, async {
            loop {
                if String::from_utf8_lossy(&self.output.lock().unwrap()).contains(text) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("terminal never emitted {text:?}"));
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
    socket_paths: Arc<Mutex<Vec<String>>>,
    receipts: Arc<Mutex<std::collections::HashMap<(String, String), Value>>>,
    receipts_enabled: Arc<AtomicBool>,
    vault_writes: Arc<Mutex<Vec<Value>>>,
    native_writes: Arc<Mutex<Vec<Value>>>,
    native_key: Arc<p256::SecretKey>,
    native_expiry: u64,
    routing_requests: Arc<Mutex<Vec<String>>>,
    routing_bodies: Arc<Mutex<Vec<Value>>>,
    settings_requests: Arc<Mutex<Vec<Value>>>,
    model_route: Arc<Mutex<Option<Value>>>,
    listed_agent: Arc<Mutex<String>>,
    listed_title: Arc<Mutex<String>>,
    resume_gate: Arc<tokio::sync::Semaphore>,
    routing_gate: Arc<tokio::sync::Semaphore>,
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
    fn routing_enabled(&self) -> bool {
        self.routing_bodies
            .lock()
            .unwrap()
            .last()
            .is_some_and(|body| {
                body.get("model").is_none()
                    || matches!(
                        body["model"].as_str(),
                        Some("@cf/zai-org/glm-5.3" | "kimi-k3" | "mimo-v2.6-pro")
                    )
            })
    }

    fn routing_automatic(&self) -> bool {
        self.routing_bodies
            .lock()
            .unwrap()
            .last()
            .is_some_and(|body| body.get("model").is_none())
    }

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
    let title = service.listed_title.lock().unwrap().clone();
    Json(
        json!({"data": [agent], "summaries": {agent: {"title": title, "created_at": 1, "updated_at": 1, "turn_count": 1}}}),
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
    if uri.path() == "/v1/agents/live" {
        let mut settings = service.settings.lock().unwrap();
        for field in ["model", "thinking", "reasoning_mode"] {
            settings[field] = json!(query[field]);
        }
        settings["fast_mode"] = json!(query["fast_mode"].parse::<bool>().unwrap());
    }
    service
        .socket_paths
        .lock()
        .unwrap()
        .push(uri.path().to_owned());
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
            "workspace": "cloudflare-computer",
            "execution_environments": true, "execution_namespace": "cwd-root-v1", "native_cross_mounts": false},
        "settings": service.settings.lock().unwrap().clone()
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
            "workspace": "cloudflare-computer",
            "execution_environments": true, "execution_namespace": "cwd-root-v1", "native_cross_mounts": false},
        "settings": service.settings.lock().unwrap().clone(),
        "model_routing_enabled": service.routing_enabled(),
        "model_routing_automatic": service.routing_automatic(),
        "model_route": service.model_route.lock().unwrap().clone(),
        "latest_event_cursor": service.latest_cursor(), "stream_error": null
    })))
}

// Model selection crosses the same HTTP boundary as the managed service:
// gateway settings require POST /routing; PATCH /settings rejects them.
async fn enable_routing(
    State(service): State<Service>,
    axum::extract::Path(agent): axum::extract::Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let _permit = service.routing_gate.acquire().await.unwrap();
    let body: Value = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    service.routing_requests.lock().unwrap().push(agent);
    service.routing_bodies.lock().unwrap().push(body.clone());
    if !service.history.lock().unwrap().is_empty() {
        return Err((
            axum::http::StatusCode::CONFLICT,
            Json(
                json!({"error": "routing_requires_new_thread", "message": "routing requires an empty thread"}),
            ),
        ));
    }
    assert!(
        body.as_object()
            .unwrap()
            .keys()
            .all(|key| matches!(key.as_str(), "model" | "thinking"))
    );
    if let Some(model) = body.get("model") {
        let thinking = body["thinking"]
            .as_str()
            .expect("manual routing requires thinking");
        let supported = match model.as_str().unwrap() {
            "kimi-k3" => ["low", "high"].contains(&thinking),
            "@cf/zai-org/glm-5.3" | "mimo-v2.6-pro" => {
                ["low", "medium", "high"].contains(&thinking)
            }
            "gpt-6-astra" => ["low", "medium", "high", "xhigh", "max"].contains(&thinking),
            other => panic!("unexpected routing model: {other}"),
        };
        assert!(
            supported,
            "unsupported effort reached the routing API: {body}"
        );
        *service.settings.lock().unwrap() = json!({"model": model, "thinking": thinking,
            "reasoning_mode": "standard", "fast_mode": false});
    }
    Ok(Json(json!({
        "enabled": service.routing_enabled(),
        "automatic": service.routing_automatic(),
        "model_routing": service.routing_enabled().then(|| json!({"strategy": "direct", "preferences": {}})),
        "settings": service.settings.lock().unwrap().clone()
    })))
}

async fn patch_settings(
    State(service): State<Service>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    service.settings_requests.lock().unwrap().push(body.clone());
    let gateway = matches!(
        body["model"].as_str(),
        Some("@cf/zai-org/glm-5.3" | "kimi-k3" | "mimo-v2.6-pro")
    );
    if gateway || service.routing_enabled() {
        return Err((
            axum::http::StatusCode::CONFLICT,
            Json(
                json!({"error": "invalid_request", "message": "model_routing owns model and thinking; omit settings"}),
            ),
        ));
    }
    let mut settings = service.settings.lock().unwrap();
    for (key, value) in body.as_object().unwrap() {
        settings[key] = value.clone();
    }
    Ok(Json(json!({"settings": settings.clone()})))
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

fn accepted_receipt(input: &Value) -> Value {
    use sha2::{Digest as _, Sha256};
    let prompt: nanocodex::agent::input::Prompt =
        serde_json::from_value(json!({"instruction": input})).unwrap();
    json!({"state": "accepted", "input_key": Sha256::digest(serde_json::to_vec(&prompt).unwrap()).iter().map(|byte| format!("{byte:02x}")).collect::<String>()})
}

async fn steer_receipt(
    State(service): State<Service>,
    axum::extract::Path((_, turn)): axum::extract::Path<(String, String)>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, axum::http::StatusCode> {
    if !service.receipts_enabled.load(Ordering::Acquire) {
        return Err(axum::http::StatusCode::NOT_FOUND);
    }
    let id = query.get("message_id").unwrap();
    let receipt = service
        .receipts
        .lock()
        .unwrap()
        .get(&(turn.clone(), id.clone()))
        .cloned()
        .unwrap_or_else(|| json!({"state": "unknown"}));
    Ok(Json(
        json!({"protocol": 1, "turn_id": turn, "message_id": id, "state": receipt["state"], "input_key": receipt["input_key"]}),
    ))
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
    socket_paths: Arc<Mutex<Vec<String>>>,
    receipts: Arc<Mutex<std::collections::HashMap<(String, String), Value>>>,
    receipts_enabled: Arc<AtomicBool>,
    vault_writes: Arc<Mutex<Vec<Value>>>,
    native_writes: Arc<Mutex<Vec<Value>>>,
    native_key: Arc<p256::SecretKey>,
    routing_requests: Arc<Mutex<Vec<String>>>,
    routing_bodies: Arc<Mutex<Vec<Value>>>,
    settings_requests: Arc<Mutex<Vec<Value>>>,
    model_route: Arc<Mutex<Option<Value>>>,
    listed_agent: Arc<Mutex<String>>,
    listed_title: Arc<Mutex<String>>,
    resume_gate: Arc<tokio::sync::Semaphore>,
    routing_gate: Arc<tokio::sync::Semaphore>,
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
            .wait_text(if active {
                "Enter steer"
            } else if attach {
                "Enter send"
            } else {
                "actions"
            })
            .await;
        fixture
    }

    async fn launch_with_history(
        active: bool,
        attach: bool,
        initial_history: Vec<Value>,
        history_gate: Arc<tokio::sync::Semaphore>,
    ) -> Self {
        Self::launch_with_reload_dir(active, attach, initial_history, history_gate, None).await
    }

    async fn start_with_reload_dir(reload_dir: &Path) -> Self {
        let fixture = Self::launch_with_reload_dir(
            true,
            true,
            Vec::new(),
            Arc::new(tokio::sync::Semaphore::new(1)),
            Some(reload_dir),
        )
        .await;
        fixture.terminal.wait_text("Enter steer").await;
        fixture
    }

    async fn launch_with_reload_dir(
        active: bool,
        attach: bool,
        initial_history: Vec<Value>,
        history_gate: Arc<tokio::sync::Semaphore>,
        reload_dir: Option<&Path>,
    ) -> Self {
        Self::launch_with_catalog(
            active,
            attach,
            initial_history,
            history_gate,
            reload_dir,
            "available",
            None,
        )
        .await
    }

    async fn launch_with_catalog(
        active: bool,
        attach: bool,
        initial_history: Vec<Value>,
        history_gate: Arc<tokio::sync::Semaphore>,
        reload_dir: Option<&Path>,
        catalog_mode: &'static str,
        startup_prompt: Option<&str>,
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
        let listed_title = Arc::new(Mutex::new("RETAINED_REMOTE_WORK".to_owned()));
        let resume_gate = Arc::new(tokio::sync::Semaphore::new(1));
        let routing_gate = Arc::new(tokio::sync::Semaphore::new(1));
        let socket_paths = Arc::new(Mutex::new(Vec::new()));
        let vault_writes = Arc::new(Mutex::new(Vec::new()));
        let native_writes = Arc::new(Mutex::new(Vec::new()));
        let native_key = Arc::new(p256::SecretKey::random(
            &mut p256::elliptic_curve::rand_core::OsRng,
        ));
        let native_expiry = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 240_000;
        let receipts = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let receipts_enabled = Arc::new(AtomicBool::new(false));
        let routing_requests = Arc::new(Mutex::new(Vec::new()));
        let routing_bodies = Arc::new(Mutex::new(Vec::new()));
        let settings_requests = Arc::new(Mutex::new(Vec::new()));
        let model_route = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route("/v1/models", get(move |headers: axum::http::HeaderMap| async move {
                let authorization = format!("Bearer ncx_live_{}_{}", "a".repeat(12), "b".repeat(43));
                let isolated_login = format!("Bearer ncx_live_{}_{}", "a".repeat(12), "c".repeat(43));
                let supplied = headers.get("authorization").and_then(|value| value.to_str().ok());
                assert!(supplied == Some(authorization.as_str()) || supplied == Some(isolated_login.as_str()));
                if catalog_mode == "held" {
                    std::future::pending::<()>().await;
                }
                if catalog_mode == "unavailable" {
                    return Err(axum::http::StatusCode::SERVICE_UNAVAILABLE);
                }
                Ok(Json(json!({
                    "object": "list", "default_model": "gpt-6-astra",
                    "data": [
                        {"id": "gpt-6-astra", "name": "Astra", "provider": "openai",
                            "thinking": ["low", "medium", "high", "xhigh", "max"],
                            "fast_mode": true, "reasoning_modes": ["standard"]},
                        {"id": "@cf/zai-org/glm-5.3", "name": "GLM 5.3", "provider": "workers_ai",
                            "thinking": ["low", "medium", "high"], "fast_mode": false, "reasoning_modes": ["standard"]},
                        {"id": "kimi-k3", "name": "Kimi K3", "provider": "gateway",
                            "thinking": ["low", "high"], "fast_mode": false, "reasoning_modes": ["standard"]},
                        {"id": "mimo-v2.6-pro", "name": "MiMo V2.6 Pro", "provider": "gateway",
                            "thinking": ["low", "medium", "high"], "fast_mode": false, "reasoning_modes": ["standard"]}
                    ]
                })))
            }))
            .route("/v1/me", get(|headers: axum::http::HeaderMap| async move {
                assert!(headers["authorization"].to_str().unwrap().starts_with("Bearer ncx_live_"));
                Json(json!({"user":{"id":"aabbccdd-1122-4455-8899-aabbccddeeff","persistent":true},"organization":{"id":"fixture-org"},"team":{"id":"fixture-team"},"role":"owner","authentication":"api_key"}))
            }))
            .route("/v1/credentials", get(vault_metadata))
            .route("/v1/agents/{agent}/native-secure-input", post(native_secure_input_fixture))
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
            .route("/v1/agents/{agent}/routing", post(enable_routing))
            .route("/v1/agents/{agent}/settings", patch(patch_settings))
            .route("/v1/agents/{agent}/turns", post(submit))
            .route("/v1/agents/{agent}/turns/{turn}/steer", post(steer))
            .route("/v1/agents/{agent}/turns/{turn}/steer-receipt", get(steer_receipt))
            .route("/v1/agents/{agent}/turns/{turn}/cancel", post(cancel))
            .with_state(Service {
                socket_paths: socket_paths.clone(),
                receipts: receipts.clone(),
                receipts_enabled: receipts_enabled.clone(),
                vault_writes: vault_writes.clone(),
                native_writes: native_writes.clone(), native_key: native_key.clone(), native_expiry,
                routing_requests: routing_requests.clone(),
                routing_bodies: routing_bodies.clone(),
                settings_requests: settings_requests.clone(),
                model_route: model_route.clone(),
                listed_agent: listed_agent.clone(),
                listed_title: listed_title.clone(),
                resume_gate: resume_gate.clone(),
                routing_gate: routing_gate.clone(),
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
        let mut terminal = Terminal::start_with_reload_dir(&origin, attach, reload_dir);
        if let Some(prompt) = startup_prompt {
            terminal.wait_text("actions").await;
            terminal.prompt(prompt, "\r");
        }
        let events = tokio::time::timeout(TIMEOUT, connections.recv())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "initial connection missing: {}\nRaw output: {:?}",
                    terminal.screen.lock().unwrap().screen().contents(),
                    String::from_utf8_lossy(&terminal.output.lock().unwrap())
                )
            })
            .unwrap();
        Self {
            socket_paths,
            receipts,
            receipts_enabled,
            vault_writes,
            native_writes,
            native_key,
            routing_requests,
            routing_bodies,
            settings_requests,
            model_route,
            listed_agent,
            listed_title,
            resume_gate,
            routing_gate,
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

    /// Start a routing change and wait until the root accepts input again.
    /// While it runs the root is non-interactive and drops typed input, yet a
    /// lagging screen can still show the previous composer, "Enter send" and
    /// a model label the status itself names. Hold the routing request until
    /// the change's status is on screen, then wait for that status to clear.
    async fn routing_change(&mut self, keys: impl FnOnce(&mut Terminal), status: &str) {
        let pause = self.routing_gate.clone().acquire_owned().await.unwrap();
        keys(&mut self.terminal);
        self.terminal.wait_text(status).await;
        drop(pause);
        self.terminal.wait_no_text(status).await;
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

// Invoke the shipped TUI over a real PTY/HTTP/WebSocket boundary. The optional
// catalog never returns (or fails), but the first prompt must still complete.
#[tokio::test]
async fn terminal_hosted_defaults_do_not_wait_for_model_catalog() {
    for catalog_mode in ["held", "unavailable"] {
        let started = std::time::Instant::now();
        let mut fixture = Fixture::launch_with_catalog(
            false,
            false,
            Vec::new(),
            Arc::new(tokio::sync::Semaphore::new(1)),
            None,
            catalog_mode,
            Some("INSTANT_DEFAULT_PROMPT"),
        )
        .await;
        let connected = started.elapsed();
        assert_eq!(
            *fixture.settings.lock().unwrap(),
            json!({
                "model": "gpt-6-astra", "thinking": "low",
                "reasoning_mode": "standard", "fast_mode": false,
            })
        );
        assert_eq!(*fixture.socket_paths.lock().unwrap(), ["/v1/agents/live"]);
        let turn = fixture.submission("INSTANT_DEFAULT_PROMPT").await;
        fixture.nested(
            &turn,
            "assistant.message",
            json!({
                "phase": "final_answer", "text": "HOSTED_DEFAULT_READY",
            }),
        );
        fixture.complete(&turn);
        fixture.terminal.wait_text("HOSTED_DEFAULT_READY").await;
        eprintln!(
            "JOURNEY catalog={catalog_mode}: hosted Astra/low/standard/fast=false; live connection={connected:?}; first answer={:?}",
            started.elapsed()
        );
        fixture.terminal.input("\x03\x03");
        tokio::time::timeout(TIMEOUT, async {
            loop {
                if let Some(status) = fixture.terminal.child.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("TUI did not close while catalog was unavailable");
    }
}

#[tokio::test]
async fn terminal_autoroute_enables_through_api_then_locks_after_first_prompt() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("/autoroute", "\r");
    fixture
        .terminal
        .wait_text("Automatic routing enabled")
        .await;
    assert_eq!(*fixture.routing_requests.lock().unwrap(), [AGENT]);
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());

    fixture.terminal.prompt("FIRST_ROUTED_TASK", "\r");
    let turn = fixture.submission("FIRST_ROUTED_TASK").await;
    // The chooser selects a different model and transport from the startup defaults.
    *fixture.model_route.lock().unwrap() = Some(json!({
        "backend": "vercel", "model": "@cf/zai-org/glm-5.3", "thinking": "high"
    }));
    fixture.complete(&turn);
    fixture.terminal.wait_text("done").await;
    fixture.terminal.wait_text("Vercel").await;
    fixture.terminal.wait_text("glm-5.3").await;

    fixture.terminal.prompt("/autoroute", "\r");
    fixture
        .terminal
        .wait_text("Auto routing can only be enabled before the first prompt")
        .await;
    assert_eq!(*fixture.routing_requests.lock().unwrap(), [AGENT]);
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());

    // A subsequent ordinary prompt is a barrier: no delayed slash command may
    // enter the model stream after the local rejection.
    fixture.terminal.prompt("FOLLOWUP_WITH_PINNED_ROUTE", "\r");
    let followup = fixture.submission("FOLLOWUP_WITH_PINNED_ROUTE").await;
    fixture.complete(&followup);
    assert_eq!(*fixture.routing_requests.lock().unwrap(), [AGENT]);
}

#[tokio::test]
async fn terminal_autoroute_cannot_enable_on_an_attached_session_with_history() {
    let history = vec![
        json!({"cursor": "1", "turn_id": REMOTE_TURN, "type": "turn_accepted", "id": REMOTE_TURN, "input": "PREVIOUS_USER_MESSAGE", "replayed": false}),
        json!({"cursor": "2", "turn_id": REMOTE_TURN, "type": "turn_completed", "id": REMOTE_TURN, "final_message": "PREVIOUS_ANSWER", "usage": null, "citations": [], "usage_error": null}),
    ];
    let mut fixture = Fixture::start_with_history(false, true, history).await;
    fixture.terminal.wait_text("PREVIOUS_USER_MESSAGE").await;
    fixture.terminal.prompt("/autoroute", "\r");
    fixture
        .terminal
        .wait_text("Auto routing can only be enabled before the first prompt")
        .await;
    assert!(fixture.routing_requests.lock().unwrap().is_empty());
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());
}

#[tokio::test]
async fn terminal_autoroute_can_enable_on_an_empty_attached_session() {
    let mut fixture = Fixture::start_with_history(false, true, Vec::new()).await;
    fixture.terminal.prompt("/autoroute", "\r");
    fixture
        .terminal
        .wait_text("Automatic routing enabled")
        .await;
    assert_eq!(*fixture.routing_requests.lock().unwrap(), [AGENT]);
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());
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
        fixture.terminal.wait_output(&copy).await;
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
    fixture.terminal.wait_text("Enter").await;
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
        // This journey inspects tool details; explicitly open them.
        fixture.terminal.input("\x0f");
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
        // This journey inspects tool details; explicitly open them.
        fixture.terminal.input("\x0f");
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
async fn terminal_fresh_session_can_cancel_an_external_turn_without_capabilities() {
    let mut fixture = Fixture::start().await;
    fixture.emit(
        REMOTE_TURN,
        json!({"type": "turn_accepted", "id": REMOTE_TURN,
            "input": "work started by another client", "replayed": false}),
    );
    fixture.terminal.wait_text("Enter steer").await;
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
    fixture.emit(
        REMOTE_TURN,
        json!({"type": "turn_cancelled", "id": REMOTE_TURN}),
    );
    fixture.terminal.wait_text("Enter send").await;
    assert!(fixture.cancellations.try_recv().is_err());
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
async fn terminal_reconciles_unknown_steering_from_its_durable_receipt() {
    for attached in [false, true] {
        let mut fixture = Fixture::start_with_active(attached).await;
        fixture.receipts_enabled.store(true, Ordering::Release);
        let turn = if attached {
            REMOTE_TURN.to_owned()
        } else {
            fixture.terminal.prompt("local task", "\r");
            fixture.submission("local task").await
        };
        fixture.terminal.prompt("ACK_LOST_INSTRUCTION", "\r");
        let (input, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
            .await
            .unwrap()
            .unwrap();
        let message_id = input["message_id"].as_str().unwrap().to_owned();
        ack.send(false).unwrap();
        fixture.terminal.wait_text("[delivery unknown]").await;
        // Foreign callers and other turns cannot confirm our request.
        fixture.receipts.lock().unwrap().insert(
            (turn.clone(), "foreign".into()),
            accepted_receipt(&input["input"]),
        );
        fixture.receipts.lock().unwrap().insert(
            ("other-turn".into(), message_id.clone()),
            accepted_receipt(&input["input"]),
        );
        fixture.terminal.wait_text("[delivery unknown]").await;
        fixture.receipts.lock().unwrap().insert(
            (turn.clone(), message_id),
            accepted_receipt(&input["input"]),
        );
        fixture.terminal.wait_no_text("[delivery unknown]").await;
        fixture.terminal.wait_text("steering accepted").await;
        fixture.terminal.prompt("NEXT_INSTRUCTION", "\r");
        let (next, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(prompt_text(&next["input"]), "NEXT_INSTRUCTION");
        ack.send(true).unwrap();
        fixture.complete(&turn);
        assert!(fixture.submissions.try_recv().is_err());
    }
}

#[tokio::test]
async fn terminal_replays_steer_receipt_after_disconnect_and_completion_without_resubmitting() {
    let mut fixture = Fixture::start().await;
    fixture.receipts_enabled.store(true, Ordering::Release);
    fixture.terminal.prompt("local task", "\r");
    let turn = fixture.submission("local task").await;
    fixture.terminal.prompt("ACK_LOST_BEFORE_RECONNECT", "\r");
    let (input, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    ack.send(false).unwrap();
    fixture.terminal.wait_text("[delivery unknown]").await;
    fixture.break_stream();
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("Reconnected").await;
    fixture.complete(&turn);
    fixture.receipts.lock().unwrap().insert(
        (turn, input["message_id"].as_str().unwrap().into()),
        accepted_receipt(&input["input"]),
    );
    fixture.terminal.wait_no_text("[delivery unknown]").await;
    fixture.terminal.wait_text("steering accepted").await;
    assert!(fixture.steers.try_recv().is_err());
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_durable_steer_receipt_wins_over_late_http_failure() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.receipts_enabled.store(true, Ordering::Release);
    fixture.terminal.prompt("RECEIPT_BEFORE_FAILED_ACK", "\r");
    let (input, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    fixture.receipts.lock().unwrap().insert(
        (
            REMOTE_TURN.into(),
            input["message_id"].as_str().unwrap().into(),
        ),
        accepted_receipt(&input["input"]),
    );
    fixture.terminal.wait_text("steering accepted").await;
    let _ = ack.send(false);
    fixture.terminal.prompt("AFTER_LATE_HTTP_FAILURE", "\r");
    let (next, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prompt_text(&next["input"]), "AFTER_LATE_HTTP_FAILURE");
    ack.send(true).unwrap();
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_no_text("[delivery unknown]").await;
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test]
async fn terminal_receipt_for_a_different_payload_cannot_confirm_unknown_steering() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.receipts_enabled.store(true, Ordering::Release);
    fixture.terminal.prompt("DIFFERENT_PAYLOAD", "\r");
    let (input, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
        .await
        .unwrap()
        .unwrap();
    fixture.receipts.lock().unwrap().insert(
        (
            REMOTE_TURN.into(),
            input["message_id"].as_str().unwrap().into(),
        ),
        accepted_receipt(&json!("earlier payload")),
    );
    ack.send(false).unwrap();
    fixture.terminal.wait_text("[delivery unknown]").await;
    // Let the independent receipt poll finish as well as the HTTP reconciliation.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    fixture.terminal.wait_text("[delivery unknown]").await;
    fixture.terminal.wait_no_text("steering accepted").await;
    assert!(fixture.steers.try_recv().is_err());
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
    // This journey inspects tool details; explicitly open them.
    fixture.terminal.input("\x0f");
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
    // A missing receipt is an unknown outcome, never a failure.
    fixture.terminal.wait_text("outcome unknown").await;
    wait_line(&fixture.terminal, &["?", "MISSING_TOOL_RESULT.txt"]).await;
    fixture.terminal.wait_text("Enter send").await;
    fixture
        .terminal
        .prompt("NEXT_TURN_AFTER_MISSING_TERMINAL", "\r");
    let next = fixture.submission("NEXT_TURN_AFTER_MISSING_TERMINAL").await;
    fixture.complete(&next);
    fixture.terminal.wait_text("Enter send").await;
}

#[tokio::test]
async fn terminal_marks_receiptless_tools_unknown_and_accepts_their_late_result() {
    let mut fixture = Fixture::start().await;
    // This journey inspects tool details; explicitly open them.
    fixture.terminal.input("\x0f");
    fixture
        .terminal
        .prompt("terminate a cell before its nested read settles", "\r");
    let turn = fixture
        .submission("terminate a cell before its nested read settles")
        .await;
    fixture.nested(&turn, "run.started", json!({}));
    fixture.nested(&turn, "tool.call", json!({"call_id": "cell", "tool": "exec", "arguments": {"code": "await tools.read_file({})"}}));
    fixture.nested(&turn, "tool.call", json!({"call_id": "cell/code-0", "tool": "read_file", "arguments": {"path": "ACTUALLY_FAILED_READ.txt"}}));
    fixture.nested(&turn, "tool.result", json!({"call_id": "cell/code-0", "tool": "read_file", "status": "failed", "duration_ns": 1, "result": {"error": "ACTUAL_READ_FAILURE"}}));
    fixture.nested(&turn, "tool.call", json!({"call_id": "cell/code-1", "tool": "read_file", "arguments": {"path": "RECEIPTLESS_READ.txt"}}));
    fixture.nested(&turn, "tool.result", json!({"call_id": "cell", "tool": "exec", "status": "completed", "duration_ns": 1, "result": "Script running with cell ID cell-1\nOutput:\n"}));
    fixture.nested(&turn, "tool.call", json!({"call_id": "stop", "tool": "wait", "arguments": {"cell_id": "cell-1", "terminate": true}}));
    fixture.nested(&turn, "tool.result", json!({"call_id": "stop", "tool": "wait", "status": "completed", "duration_ns": 1, "result": "Script terminated\nOutput:\n"}));
    // The terminated cell's receiptless read is unknown; the real failure stays failed.
    fixture.terminal.wait_text("outcome unknown").await;
    wait_line(&fixture.terminal, &["?", "RECEIPTLESS_READ.txt"]).await;
    wait_line(&fixture.terminal, &["×", "ACTUALLY_FAILED_READ.txt"]).await;
    fixture.terminal.wait_text("ACTUAL_READ_FAILURE").await;
    // The call's later actual receipt settles the same card.
    fixture.nested(&turn, "tool.result", json!({"call_id": "cell/code-1", "tool": "read_file", "status": "completed", "duration_ns": 1, "result": {"text": "LATE_ACTUAL_READ_RESULT"}}));
    fixture.terminal.wait_text("LATE_ACTUAL_READ_RESULT").await;
    wait_line(&fixture.terminal, &["✓", "RECEIPTLESS_READ.txt"]).await;
    fixture
        .terminal
        .wait_no_text("tool call ended without a terminal result")
        .await;
    wait_line(&fixture.terminal, &["×", "ACTUALLY_FAILED_READ.txt"]).await;
    fixture.complete(&turn);
    fixture.terminal.wait_text("Enter send").await;
}

/// Waits until one rendered row contains every needle.
async fn wait_line(terminal: &Terminal, needles: &[&str]) {
    let found = tokio::time::timeout(TIMEOUT, async {
        loop {
            let screen = terminal.screen.lock().unwrap().screen().contents();
            if screen
                .lines()
                .any(|line| needles.iter().all(|needle| line.contains(needle)))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if found.is_err() {
        let screen = terminal.screen.lock().unwrap().screen().contents();
        panic!("no row contains {needles:?}:\n{screen}");
    }
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
    // This journey inspects tool details; explicitly open them.
    fixture.terminal.input("\x0f");
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
        screen
            .lines()
            .filter(|line| line.contains("Shell") && line.contains("BACKGROUND_BUILD_COMMAND"))
            .count(),
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
    // This journey inspects tool details; explicitly open them.
    fixture.terminal.input("\x0f");
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
async fn terminal_resumes_by_generated_title() {
    const OTHER_AGENT: &str = "019fc927-b280-79a7-8445-1b9996ad2fb1";
    const GENERATED_TITLE: &str = "Repair cobalt deployment";
    let mut fixture = Fixture::start().await;
    *fixture.listed_agent.lock().unwrap() = OTHER_AGENT.to_owned();
    // The service supplies the persisted generated title through GET /v1/agents.
    *fixture.listed_title.lock().unwrap() = GENERATED_TITLE.to_owned();
    fixture.terminal.input("/");
    fixture.terminal.wait_text("Resume session").await;
    fixture.terminal.input("restore\r");
    fixture.terminal.wait_text(GENERATED_TITLE).await;
    fixture.terminal.wait_text(OTHER_AGENT).await;
    fixture.terminal.input("unmatchedquartz");
    fixture.terminal.wait_text("No matching threads").await;
    fixture.terminal.wait_no_text(GENERATED_TITLE).await;
    fixture.terminal.input("\x15cobalt");
    fixture.terminal.wait_text(GENERATED_TITLE).await;
    fixture.terminal.wait_no_text("No matching threads").await;
    eprintln!(
        "Resume title search (query=cobalt):\n{}",
        fixture.terminal.screen.lock().unwrap().screen().contents()
    );
    // Hold the switch until its status is on screen. Otherwise a lagging PTY
    // screen can lack "Resuming session" while the switch is still running,
    // and the prompt typed into the non-interactive composer is dropped.
    let pause = fixture.resume_gate.clone().acquire_owned().await.unwrap();
    fixture.terminal.input("\r");
    fixture.terminal.wait_text("Resuming session").await;
    drop(pause);
    fixture.replacement_connection().await;
    fixture.terminal.wait_no_text("Resuming session").await;
    fixture
        .terminal
        .prompt("CONTINUE_GENERATED_TITLE_THREAD", "\r");
    let message = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message["fixture_agent_id"], OTHER_AGENT);
    assert_eq!(
        prompt_text(&message["input"]),
        "CONTINUE_GENERATED_TITLE_THREAD"
    );
    eprintln!(
        "Resume submission reached agent {} with input {}",
        message["fixture_agent_id"], message["input"]
    );
    let turn = message["id"].as_str().unwrap().to_owned();
    fixture.emit(
        &turn,
        json!({"type": "turn_accepted", "id": turn, "input": message["input"], "replayed": false}),
    );
    fixture.complete(&turn);
    fixture.terminal.wait_text("Enter send").await;
    assert!(fixture.submissions.try_recv().is_err());
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
        // This journey inspects tool details; explicitly open them.
        fixture.terminal.input("\x0f");
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
    fixture.terminal.prompt("/voice on", "\r");
    fixture.terminal.wait_text("ctrl+x mute").await;
    fixture.terminal.prompt("DRAFT_WHILE_VOICE_CONNECTS", "");
    fixture.terminal.input("\x18");
    fixture.terminal.wait_text("ctrl+x unmute").await;
    fixture
        .terminal
        .wait_text("DRAFT_WHILE_VOICE_CONNECTS")
        .await;
    fixture.terminal.input("\x15");
    fixture.terminal.prompt("/voice off", "\r");
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
    // This journey inspects tool details; explicitly open them.
    fixture.terminal.input("\x0f");
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
    wait_line(&fixture.terminal, &["?", "UNFINISHED_STOP_READ.txt"]).await;
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
async fn terminal_tool_activity_keeps_wrapper_failures_visible() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.nested(REMOTE_TURN, "tool.call", json!({"call_id": "wrapper", "tool": "exec", "arguments": "await check(); throw new Error('WRAPPER_FAILURE')"}));
    fixture.nested(REMOTE_TURN, "tool.call", json!({"call_id": "wrapper/code-0", "tool": "exec_command", "arguments": {"cmd": "CHILD_COMMAND"}}));
    fixture.nested(REMOTE_TURN, "tool.result", json!({"call_id": "wrapper/code-0", "tool": "exec_command", "status": "completed", "duration_ns": 1, "result": {"output": "child succeeded", "exit_code": 0}}));
    fixture.nested(REMOTE_TURN, "tool.result", json!({"call_id": "wrapper", "tool": "exec", "status": "failed", "duration_ns": 2, "result": {"error": "WRAPPER_FAILURE"}}));
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("WRAPPER_FAILURE").await;
    fixture.terminal.wait_text("CHILD_COMMAND").await;
    eprintln!(
        "WRAPPER FAILURE\n{}",
        fixture.terminal.screen.lock().unwrap().screen().contents()
    );
}

#[tokio::test]
async fn terminal_computer_activity_keeps_observations_in_disclosed_details() {
    let mut fixture = Fixture::start_with_active(true).await;
    let name = "mcp__cua_repl__js";
    let tree =
        "The following is a diff from the previous accessibility tree\n42 button SNAPSHOT_CONTROL";
    fixture.nested(
        REMOTE_TURN,
        "tool.call",
        json!({"call_id":"computer", "tool":"exec", "arguments":"await inspectComputer()"}),
    );
    for (index, title, result) in [
        (
            0,
            "Observe native app",
            json!({"content":[{"type":"text","text":tree}],"isError":false}),
        ),
        (
            1,
            "Read current controls",
            json!({"content":[{"type":"text","text":"SECOND_RAW_OBSERVATION"}],"isError":false}),
        ),
        (
            2,
            "Capture native view",
            json!({"content":[{"type":"image","mimeType":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aS6kAAAAASUVORK5CYII="}],"isError":false}),
        ),
        (
            3,
            "Click observed control",
            json!({"content":[{"type":"text","text":"Script error:\nCONTROL_DISAPPEARED\nLONG_PROVIDER_MANUAL"}],"isError":true}),
        ),
    ] {
        let id = format!("computer/code-{index}");
        fixture.nested(REMOTE_TURN, "tool.call", json!({"call_id":id,"tool":name,"arguments":{"title":title,"code":"await fixture.getAXState()"}}));
        fixture.terminal.wait_text("Using computer").await;
        fixture.nested(REMOTE_TURN, "tool.result", json!({"call_id":id,"tool":name,"status":"completed","duration_ns":1,"structured_result":result}));
    }
    let echo = json!({"content":[{"type":"text","text":tree}],"isError":false}).to_string();
    fixture.nested(REMOTE_TURN, "tool.result", json!({"call_id":"computer","tool":"exec","status":"completed","duration_ns":5,"result":[{"type":"input_text","text":echo},{"type":"input_text","text":"INDEPENDENT_COMPUTER_NOTE"}]}));
    fixture.complete(REMOTE_TURN);
    fixture
        .terminal
        .wait_text("Used computer · 4 actions · 1 failed")
        .await;
    fixture.terminal.wait_text("CONTROL_DISAPPEARED").await;
    fixture.terminal.wait_text("Captured screenshot").await;
    fixture.terminal.wait_no_text("SNAPSHOT_CONTROL").await;
    fixture
        .terminal
        .wait_no_text("previous accessibility tree")
        .await;
    fixture.terminal.wait_no_text("LONG_PROVIDER_MANUAL").await;
    eprintln!(
        "COMPUTER COMPACT\n{}",
        fixture.terminal.screen.lock().unwrap().screen().contents()
    );
    fn click(terminal: &mut Terminal, text: &str) {
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
    click(&mut fixture.terminal, "Used computer");
    fixture
        .terminal
        .wait_text("INDEPENDENT_COMPUTER_NOTE")
        .await;
    fixture.terminal.wait_text("Observe native app").await;
    fixture.terminal.wait_no_text("SNAPSHOT_CONTROL").await;
    click(&mut fixture.terminal, "Observe native app");
    fixture.terminal.wait_text("SNAPSHOT_CONTROL").await;
    fixture
        .terminal
        .wait_text("previous accessibility tree")
        .await;
    eprintln!(
        "COMPUTER EXPANDED\n{}",
        fixture.terminal.screen.lock().unwrap().screen().contents()
    );
}

#[tokio::test]
async fn terminal_tool_activity_is_compact_live_and_expandable() {
    let mut fixture = Fixture::start_with_active(true).await;
    for (id, command) in [
        ("one", "FIRST_HIDDEN_COMMAND"),
        ("two", "SECOND_HIDDEN_COMMAND"),
    ] {
        fixture.nested(
            REMOTE_TURN,
            "tool.call",
            json!({"call_id": id, "tool": "exec_command", "arguments": {"cmd": command}}),
        );
    }
    fixture.terminal.wait_text("Tools  2 calls").await;
    fixture.terminal.wait_text("FIRST_HIDDEN_COMMAND").await;
    fixture.terminal.wait_text("SECOND_HIDDEN_COMMAND").await;
    eprintln!(
        "RUNNING\n{}",
        fixture.terminal.screen.lock().unwrap().screen().contents()
    );
    fixture.nested(REMOTE_TURN, "tool.result", json!({"call_id": "one", "tool": "exec_command", "status": "completed", "duration_ns": 1000000000, "result": {"output": "FIRST_HIDDEN_OUTPUT", "exit_code": 0}}));
    fixture.terminal.wait_text("exit 0").await;
    fixture.nested(REMOTE_TURN, "tool.result", json!({"call_id": "two", "tool": "exec_command", "status": "failed", "duration_ns": 2000000000, "result": {"output": "SECOND_HIDDEN_FAILURE", "exit_code": 1}}));
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("Enter send").await;
    fixture.terminal.wait_text("2 calls · 1 failed").await;
    fixture.terminal.wait_no_text("FIRST_HIDDEN_OUTPUT").await;
    fixture.terminal.wait_text("SECOND_HIDDEN_FAILURE").await;
    eprintln!(
        "SETTLED\n{}",
        fixture.terminal.screen.lock().unwrap().screen().contents()
    );
    fn click(terminal: &mut Terminal, text: &str) {
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
    click(&mut fixture.terminal, "Tools  2 calls");
    fixture.terminal.wait_text("FIRST_HIDDEN_OUTPUT").await;
    fixture.terminal.wait_text("SECOND_HIDDEN_COMMAND").await;
    click(&mut fixture.terminal, "SECOND_HIDDEN_COMMAND");
    fixture.terminal.wait_text("21 B").await;
    fixture.terminal.wait_text("SECOND_HIDDEN_FAILURE").await;
    eprintln!(
        "EXPANDED\n{}",
        fixture.terminal.screen.lock().unwrap().screen().contents()
    );
    click(&mut fixture.terminal, "FIRST_HIDDEN_COMMAND");
    fixture.terminal.wait_text("2 calls · 1 failed").await;
    fixture.terminal.wait_text("FIRST_HIDDEN_COMMAND").await;
    fixture.terminal.wait_text("SECOND_HIDDEN_FAILURE").await;
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
    // Completion appends an answer and moves the batch row; wait before hit testing.
    fixture.terminal.wait_text("Enter send").await;
    fixture.terminal.wait_text("Tools  2 calls").await;
    fixture.terminal.wait_text("check-first").await;
    fixture.terminal.wait_text("check-second").await;

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

    click_row(&mut fixture.terminal, "Tools  2 calls");
    fixture.terminal.wait_text("2 tools").await;
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
    fixture.terminal.wait_text("check-first").await;
    fixture.terminal.wait_text("check-second").await;
    fixture.terminal.wait_no_text("FIRST_CHILD_OUTPUT").await;

    click_row(&mut fixture.terminal, "Tools  2 calls");
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

#[tokio::test]
async fn terminal_voice_clone_recording_panel_cancels_without_model_input() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture
        .terminal
        .prompt("/voice clone \"Sample speaker\"", "\r");
    fixture.terminal.wait_text("Sample speaker").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_no_text("Sample speaker").await;
    fixture.terminal.wait_text("Enter steer").await;
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());
    // Opening/canceling the local panel must leave the normal composer usable.
    fixture.terminal.prompt("/voice voices chatgpt", "\r");
    fixture.terminal.wait_text("ChatGPT voices").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_no_text("ChatGPT voices").await;
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());
}

#[tokio::test]
async fn terminal_voice_menu_exposes_clone_and_chatgpt_picker_without_model_input() {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal.prompt("/voice", "\r");
    fixture.terminal.wait_text("Record a voice clone").await;
    fixture.terminal.wait_text("ChatGPT voices").await;
    fixture.terminal.wait_text("ElevenLabs voices").await;
    fixture.terminal.input("\x1b[B\x1b[B\x1b[B\r");
    fixture.terminal.wait_text("Voice clone: My voice").await;
    fixture.terminal.wait_text("R: record/re-record").await;
    fixture.terminal.wait_text("H: read-aloud script").await;
    fixture.terminal.input("h");
    fixture.terminal.wait_text("Read naturally").await;
    fixture.terminal.wait_text("This morning").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_no_text("Voice clone:").await;
    fixture.terminal.prompt("/voice", "\r");
    fixture.terminal.wait_text("Record a voice clone").await;
    fixture.terminal.input("\x1b[B\r");
    fixture.terminal.wait_text("ChatGPT voices").await;
    fixture.terminal.wait_text("cove").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_no_text("ChatGPT voices").await;
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_reload_restarts_local_peers_without_stopping_durable_work() {
    let registry_parent = tempfile::tempdir().unwrap();
    let registry = registry_parent.path().join("reload");
    let mut first = Fixture::start_with_reload_dir(&registry).await;
    let mut second = Fixture::start_with_reload_dir(&registry).await;
    let expected_path = format!("/v1/agents/{AGENT}/ws");
    for fixture in [&first, &second] {
        assert_eq!(
            fixture.socket_paths.lock().unwrap().as_slice(),
            std::slice::from_ref(&expected_path)
        );
    }

    first.terminal.prompt("/reload", "\r");
    first.replacement_connection().await;
    second.replacement_connection().await;

    for fixture in [&mut first, &mut second] {
        fixture.terminal.wait_text("Enter steer").await;
        assert_eq!(
            *fixture.socket_paths.lock().unwrap(),
            [expected_path.clone(), expected_path.clone()],
            "reload must reattach to the existing agent on each original service"
        );
        assert!(fixture.terminal.child.try_wait().unwrap().is_none());
        fixture.terminal.prompt("/id", "\r");
        fixture.terminal.wait_text("Agent ID").await;
        fixture.terminal.wait_text(AGENT).await;
        fixture.terminal.input("\x1b");
        fixture.terminal.wait_no_text("Agent ID").await;
        fixture.terminal.wait_text("Enter steer").await;
        assert!(fixture.submissions.try_recv().is_err());
        assert!(fixture.steers.try_recv().is_err());
        assert!(fixture.cancellations.try_recv().is_err());
    }
}

// Synthetic-only private route. No administrator password or installed helper.
async fn native_secure_input_fixture(
    State(service): State<Service>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    use base64::engine::general_purpose::STANDARD;
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    use sha2_hkdf::{Digest, Sha256};
    assert!(
        headers["authorization"]
            .to_str()
            .unwrap()
            .starts_with("Bearer ncx_live_")
    );
    assert_eq!(body["request_id"], SECURE_INPUT_ID);
    assert!(!body.to_string().contains("PTY_FIXTURE_SECRET"));
    service.native_writes.lock().unwrap().push(body.clone());
    match body["action"].as_str() {
        Some("describe") => {
            // Delay to exercise key/paste interception while Loading.
            tokio::time::sleep(Duration::from_millis(150)).await;
            let binding = json!({"arguments":["--fixture", "literal\u{202e}arg"],"cwd":"/fixture", "executable":"/usr/bin/id", "uid":1000});
            Json(
                json!({"request_id":SECURE_INPUT_ID,"machine_id":"fixture-machine","executable":"/usr/bin/id","arguments":binding["arguments"],"cwd":"/fixture","uid":1000,
                "expires_at":service.native_expiry,"command_digest":STANDARD.encode(Sha256::digest(binding.to_string().as_bytes())),"public_key":STANDARD.encode(service.native_key.public_key().to_encoded_point(false).as_bytes())}),
            )
        }
        Some("cancel") => Json(
            json!({"type":"secure_input_receipt","request_id":SECURE_INPUT_ID,"status":"cancelled"}),
        ),
        _ => Json(
            json!({"type":"secure_input_receipt","request_id":SECURE_INPUT_ID,"status":"completed"}),
        ),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_native_secure_input_private_pty_roundtrip_and_no_plaintext_paths() {
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
    use base64::engine::general_purpose::STANDARD;
    use p256::{PublicKey, ecdh::diffie_hellman};
    let mut fixture = Fixture::start_with_history(false, true, Vec::new()).await;
    fixture
        .terminal
        .prompt(&format!("/secure-input {AGENT} {SECURE_INPUT_ID}"), "\r");
    fixture
        .terminal
        .wait_text("Fetching command privately")
        .await;
    fixture.terminal.prompt("PTY_FIXTURE_SECRET_LOADING", "");
    fixture.terminal.wait_text("Review EVERY argument").await;
    fixture.terminal.wait_text("Command digest").await;
    fixture.terminal.wait_text("argv[1]").await;
    fixture.terminal.unlock_private().await;
    // Prequeued Ctrl+Enter+paste must not approve or retain the pasted tail.
    fixture
        .terminal
        .input("\x1b[13;5u\x1b[200~PTY_FIXTURE_SECRET_STALE\x1b[201~");
    fixture.terminal.wait_text("Password: ********").await;
    fixture.terminal.unlock_private().await;
    fixture.terminal.input("\r");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fixture.native_writes.lock().unwrap().len(), 1);
    // Actual raw keys and bracketed paste both route only into the private field.
    fixture.terminal.input("PTY_FIXTURE_");
    fixture.terminal.prompt("SECRET", "");
    assert_private_control_export(&fixture).await;
    fixture.terminal.input("\r");
    fixture
        .terminal
        .wait_text("Protected command completed successfully")
        .await;
    let submitted = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        submitted["input"][0]["text"],
        json!({"type":"secure_input_receipt","request_id":SECURE_INPUT_ID,"status":"completed"})
            .to_string()
    );
    assert!(!submitted.to_string().contains("PTY_FIXTURE_SECRET"));
    let writes = fixture.native_writes.lock().unwrap().clone();
    assert_eq!(writes.len(), 2);
    let submit = &writes[1];
    assert_eq!(submit.as_object().unwrap().len(), 3);
    let ephemeral = PublicKey::from_sec1_bytes(
        &STANDARD
            .decode(submit["ephemeral_public_key"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    let shared = diffie_hellman(
        fixture.native_key.to_nonzero_scalar(),
        ephemeral.as_affine(),
    );
    let mut key = [0u8; 32];
    hkdf::Hkdf::<sha2_hkdf::Sha256>::new(Some(&[]), shared.raw_secret_bytes())
        .expand(SECURE_INPUT_ID.as_bytes(), &mut key)
        .unwrap();
    let encrypted = STANDARD
        .decode(submit["ciphertext"].as_str().unwrap())
        .unwrap();
    let plaintext = Aes256Gcm::new_from_slice(&key)
        .unwrap()
        .decrypt(Nonce::from_slice(&encrypted[..12]), &encrypted[12..])
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&plaintext).unwrap()["value"],
        "PTY_FIXTURE_SECRET"
    );
    assert!(
        !String::from_utf8_lossy(&fixture.terminal.output.lock().unwrap())
            .contains("PTY_FIXTURE_SECRET")
    );
    assert!(
        !fixture
            .history
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.to_string().contains("PTY_FIXTURE_SECRET"))
    );
    assert!(fixture.steers.try_recv().is_err());
    assert!(fixture.submissions.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_native_secure_input_focus_cancel_quarantines_queued_paste() {
    let mut fixture = Fixture::start_with_history(false, true, Vec::new()).await;
    fixture
        .terminal
        .prompt(&format!("/secure-input {AGENT} {SECURE_INPUT_ID}"), "\r");
    fixture.terminal.wait_text("Review EVERY argument").await;
    fixture.terminal.unlock_private().await;
    fixture.terminal.input("\x1b[13;5u");
    fixture.terminal.wait_text("Password: ********").await;
    fixture
        .terminal
        .input("\x1b[O\x1b[200~PTY_FIXTURE_SECRET_TAIL\x1b[201~\r\x1b");
    fixture.terminal.wait_text("Secure input cancelled").await;
    fixture.terminal.input("\x1b[I");
    fixture.terminal.unlock_private().await;
    fixture.terminal.input("\x1b");
    fixture
        .terminal
        .wait_no_text("Private protected sudo approval")
        .await;
    assert!(
        !String::from_utf8_lossy(&fixture.terminal.output.lock().unwrap())
            .contains("PTY_FIXTURE_SECRET")
    );
    let writes = fixture.native_writes.lock().unwrap().clone();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[1]["action"], "cancel");
    assert!(
        writes
            .iter()
            .all(|wire| !wire.to_string().contains("PTY_FIXTURE_SECRET"))
    );
    if let Ok(receipt) = fixture.submissions.try_recv() {
        assert!(!receipt.to_string().contains("PTY_FIXTURE_SECRET"));
    }
    assert!(fixture.steers.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_native_secure_input_incomplete_paste_cannot_cross_freshness_barrier() {
    let mut fixture = Fixture::start_with_history(false, true, Vec::new()).await;
    fixture
        .terminal
        .prompt(&format!("/secure-input {AGENT} {SECURE_INPUT_ID}"), "\r");
    fixture
        .terminal
        .wait_text("Fetching command privately")
        .await;
    fixture
        .terminal
        .input("\x1b[200~PTY_FIXTURE_SECRET_PARTIAL_REVIEW");
    fixture.terminal.wait_text("Review EVERY argument").await;
    let token = private_token(&fixture.terminal);
    // Safety token falls INSIDE the incomplete old paste and cannot unlock.
    fixture
        .terminal
        .input(&format!("{token}\x1b[201~\x1b[13;5u"));
    tokio::time::sleep(Duration::from_millis(150)).await;
    fixture.terminal.wait_no_text("Password: ********").await;
    assert_eq!(fixture.native_writes.lock().unwrap().len(), 1);
    fixture.terminal.unlock_private().await;
    fixture.terminal.input("\x1b[13;5u");
    fixture.terminal.wait_text("Password: ********").await;
    fixture
        .terminal
        .input("\x1b[200~PTY_FIXTURE_SECRET_PARTIAL_PASSWORD");
    let token = private_token(&fixture.terminal);
    fixture.terminal.input(&format!("{token}\x1b[201~\r"));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(fixture.native_writes.lock().unwrap().len(), 1);
    fixture
        .terminal
        .wait_text("Type safety token (keys only):")
        .await;
    // Even Esc followed by a partial paste cannot leak out through dismissal.
    fixture
        .terminal
        .input("\x1b\x1b[200~PTY_FIXTURE_SECRET_PARTIAL_EXIT");
    fixture.terminal.wait_text("Secure input cancelled").await;
    let token = private_token(&fixture.terminal);
    fixture.terminal.input(&format!("{token}\x1b[201~\x1b"));
    tokio::time::sleep(Duration::from_millis(150)).await;
    fixture
        .terminal
        .wait_text("Private protected sudo approval")
        .await;
    fixture.terminal.unlock_private().await;
    fixture.terminal.input("\x1b");
    fixture
        .terminal
        .wait_no_text("Private protected sudo approval")
        .await;
    assert!(
        !String::from_utf8_lossy(&fixture.terminal.output.lock().unwrap())
            .contains("PTY_FIXTURE_SECRET")
    );
    assert!(
        !fixture
            .history
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.to_string().contains("PTY_FIXTURE_SECRET"))
    );
    assert!(
        fixture
            .native_writes
            .lock()
            .unwrap()
            .iter()
            .all(|wire| !wire.to_string().contains("PTY_FIXTURE_SECRET"))
    );
    if let Ok(receipt) = fixture.submissions.try_recv() {
        assert!(!receipt.to_string().contains("PTY_FIXTURE_SECRET"));
    }
}

fn private_token(terminal: &Terminal) -> String {
    let parser = terminal.screen.lock().unwrap();
    parser
        .screen()
        .contents()
        .lines()
        .find_map(|line| {
            line.split_once("Type safety token (keys only): ")
                .map(|(_, rest)| rest[..32].to_owned())
        })
        .expect("fresh private token")
}

#[cfg(unix)]
async fn assert_private_control_export(fixture: &Fixture) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    // This registration/token belongs only to the synthetic disposable PTY.
    let registry = fixture
        .terminal
        ._workspace
        .path()
        .join(".codex/nanocodex/tui/instances");
    let path = std::fs::read_dir(registry)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let registration: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let socket = tokio::net::UnixStream::connect(registration["socket_path"].as_str().unwrap())
        .await
        .unwrap();
    let (read, mut write) = socket.into_split();
    let mut lines = BufReader::new(read).lines();
    write.write_all(format!("{}\n",json!({"protocol_version":1,"instance_id":registration["instance_id"],"auth_token":registration["auth_token"]})).as_bytes()).await.unwrap();
    let hello: Value = serde_json::from_str(
        &tokio::time::timeout(TIMEOUT, lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(!hello.to_string().contains("PTY_FIXTURE_SECRET"));
    assert_eq!(hello["snapshot"]["state"]["ui_blocked"], true);
    write
        .write_all(b"{\"id\":\"private-export\",\"method\":\"state.get\"}\n")
        .await
        .unwrap();
    let exported: Value = serde_json::from_str(
        &tokio::time::timeout(TIMEOUT, lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(!exported.to_string().contains("PTY_FIXTURE_SECRET"));
    assert_eq!(exported["result"]["state"]["composer"]["text"], "");
    let snapshot = &hello["snapshot"];
    let mutation = json!({"id":"private-reject","method":"prompt","params":{"expected_instance_id":registration["instance_id"],"expected_session_id":AGENT,
        "expected_active_generation":snapshot["active_generation"],"input":{"text":"NORMAL_CONTROL_MUTATION"}}});
    write
        .write_all(format!("{mutation}\n").as_bytes())
        .await
        .unwrap();
    let rejected: Value = serde_json::from_str(
        &tokio::time::timeout(TIMEOUT, lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(rejected["id"], "private-reject");
    assert_eq!(rejected["result"]["status"], "rejected");
    // The bridge rejects mutation against the published private-ui block
    // before it can reach the driver's defense-in-depth approval guard.
    assert_eq!(rejected["result"]["code"], "ui_blocked");
    assert!(!rejected.to_string().contains("PTY_FIXTURE_SECRET"));
}

// The OS opener is the external boundary: exercise the shipped binary's actual
// markdown rendering, hit testing, mouse decoder and asynchronous open effect.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_link_clicks_open_once_and_drag_still_copies() {
    use std::os::unix::fs::PermissionsExt;
    let opener = tempfile::tempdir().unwrap();
    let log = opener.path().join("opened.txt");
    for name in ["open", "xdg-open"] {
        let script = opener.path().join(name);
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$NANOCODEX_TEST_LINK_LOG\"\n",
        )
        .unwrap();
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::join_paths(std::iter::once(opener.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal = Terminal::start_with_command(&fixture.origin, true, None, |command| {
        command.env("PATH", path);
        command.env("NANOCODEX_TEST_LINK_LOG", &log);
    });
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("Enter steer").await;
    let reply = "[Release notes](https://example.test/release)\n\n[Unicode 界 label](https://example.test/unicode)\n\nAutolink <https://example.test/plain>";
    fixture.nested(
        REMOTE_TURN,
        "assistant.message",
        json!({"model_call_index":1,"item_id":"links","phase":"final_answer","text":reply}),
    );
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("Release notes").await;
    fixture.terminal.wait_text("Enter send").await;
    fn location(terminal: &Terminal, needle: &str) -> (u16, u16) {
        let parser = terminal.screen.lock().unwrap();
        for row in 0..32 {
            let line = parser.screen().rows(0, 160).nth(row).unwrap();
            if let Some(offset) = line.find(needle) {
                return (
                    unicode_width::UnicodeWidthStr::width(&line[..offset]) as u16 + 1,
                    row as u16 + 1,
                );
            }
        }
        panic!(
            "missing click label {needle:?}: {}",
            parser.screen().contents()
        );
    }
    for (needle, destination, motion) in [
        ("Release notes", "https://example.test/release", false),
        ("界 label", "https://example.test/unicode", true),
        (
            "https://example.test/plain",
            "https://example.test/plain",
            false,
        ),
    ] {
        let before = std::fs::read_to_string(&log).unwrap_or_default();
        let (col, row) = location(&fixture.terminal, needle);
        fixture.terminal.input(&format!("\x1b[<0;{col};{row}M"));
        tokio::time::sleep(Duration::from_millis(80)).await;
        if motion {
            // Terminals can report sub-cell movement as a drag at the same cell.
            fixture.terminal.input(&format!("\x1b[<32;{col};{row}M"));
        }
        fixture.terminal.input(&format!("\x1b[<0;{col};{row}m"));
        tokio::time::timeout(TIMEOUT, async {
            while std::fs::read_to_string(&log).unwrap_or_default() == before {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "click on {needle:?} did not open: {}",
                fixture.terminal.screen.lock().unwrap().screen().contents()
            )
        });
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            format!("{before}{destination}\n")
        );
        eprintln!("CLICK {needle:?} -> {destination} (same-cell motion={motion})");
    }
    let before = std::fs::read_to_string(&log).unwrap();
    let (col, row) = location(&fixture.terminal, "Release notes");
    for return_to_start in [false, true] {
        let output_start = fixture.terminal.output.lock().unwrap().len();
        let end = if return_to_start { col } else { col + 6 };
        fixture.terminal.input(&format!(
            "\x1b[<0;{col};{row}M\x1b[<32;{col};{row}M\x1b[<32;{};{row}M\x1b[<32;{end};{row}M\x1b[<0;{end};{row}m",
            col + 6
        ));
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let copied = String::from_utf8_lossy(
                    &fixture.terminal.output.lock().unwrap()[output_start..],
                )
                .contains("\x1b]52;");
                if copied {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("drag must copy through terminal clipboard");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            before,
            "dragging a link selects text without opening, even when returning to its start"
        );
    }
    eprintln!(
        "DRAG selects via OSC52 without launching a URL\n{}",
        fixture.terminal.screen.lock().unwrap().screen().contents()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_gateway_model_picker_routes_manual_selection_and_keeps_prompt_usable() {
    // Use the service's explicit fast=false state rather than fresh-CLI defaults.
    let mut fixture = Fixture::start_with_history(false, true, Vec::new()).await;
    fixture.terminal.wait_text("Enter send").await;
    fixture.terminal.input("/fast");
    fixture.terminal.wait_text("Enable fast mode").await;
    fixture.terminal.input("\r");
    tokio::time::timeout(TIMEOUT, async {
        while fixture.settings.lock().unwrap()["fast_mode"] != true {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    fixture.terminal.prompt("/thinking xhigh", "\r");
    fixture.terminal.wait_text("xhigh").await;
    for (index, model) in ["@cf/zai-org/glm-5.3", "kimi-k3", "mimo-v2.6-pro"]
        .into_iter()
        .enumerate()
    {
        fixture.terminal.prompt("/model", "\r");
        fixture.terminal.wait_text("Select model").await;
        fixture
            .routing_change(
                |terminal| terminal.input("\x1b[B\r"),
                &format!("Starting {model} session"),
            )
            .await;
        fixture.terminal.wait_no_text("Select model").await;
        tokio::time::timeout(TIMEOUT, async {
            while fixture.routing_bodies.lock().unwrap().len() <= index {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        fixture.terminal.wait_text(model).await;
        fixture.terminal.wait_text("Enter send").await;
        let bodies = fixture.routing_bodies.lock().unwrap().clone();
        assert_eq!(bodies[index]["model"], model);
        assert!(["low", "medium", "high"].contains(&bodies[index]["thinking"].as_str().unwrap()));
        assert_eq!(fixture.settings.lock().unwrap()["fast_mode"], false);
        fixture.terminal.wait_no_text("Auto · choosing").await;
        fixture
            .terminal
            .wait_no_text("Could not select model")
            .await;
        println!("Manual selection HTTP: {}", bodies[index]);
    }
    assert!(
        fixture
            .settings_requests
            .lock()
            .unwrap()
            .iter()
            .all(|body| body.get("model").is_none() || body["model"] == "gpt-6-astra")
    );
    fixture.terminal.prompt("/thinking high", "\r");
    tokio::time::timeout(TIMEOUT, async {
        while fixture.settings.lock().unwrap()["thinking"] != "high" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "Effort did not reach service; routes={:?}; terminal=\n{}",
            fixture.routing_bodies.lock().unwrap(),
            fixture.terminal.screen.lock().unwrap().screen().contents()
        )
    });
    fixture.terminal.wait_text("high").await;
    assert_eq!(
        fixture.routing_bodies.lock().unwrap().last().unwrap(),
        &json!({"model": "mimo-v2.6-pro", "thinking": "high"})
    );
    fixture
        .routing_change(
            |terminal| terminal.prompt("/autoroute", "\r"),
            "Enabling automatic routing",
        )
        .await;
    fixture.terminal.wait_text("Auto · choosing").await;
    fixture.terminal.prompt("/thinking high", "\r");
    fixture
        .terminal
        .wait_text("Automatic routing controls the model and effort")
        .await;
    fixture
        .routing_change(
            |terminal| terminal.prompt("/model gpt-6-astra", "\r"),
            "Starting Astra session",
        )
        .await;
    fixture.terminal.wait_no_text("Auto · choosing").await;
    fixture.terminal.wait_text("gpt-6-astra").await;
    fixture.terminal.wait_text("Enter send").await;
    tokio::time::timeout(TIMEOUT, async {
        while fixture.settings.lock().unwrap()["model"] != "gpt-6-astra" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    fixture
        .routing_change(
            |terminal| terminal.prompt("/model kimi-k3", "\r"),
            "Starting kimi-k3 session",
        )
        .await;
    fixture.terminal.wait_text("kimi-k3").await;
    fixture.terminal.wait_text("Enter send").await;
    fixture
        .terminal
        .prompt("MANUALLY_SELECTED_MODEL_TASK", "\r");
    let turn = fixture.submission("MANUALLY_SELECTED_MODEL_TASK").await;
    assert_eq!(fixture.settings.lock().unwrap()["model"], "kimi-k3");
    *fixture.model_route.lock().unwrap() =
        Some(json!({"backend": "vercel", "model": "kimi-k3", "thinking": "high"}));
    fixture.complete(&turn);
    fixture.terminal.wait_text("done").await;
    fixture.terminal.wait_text("Vercel").await;
    fixture.terminal.wait_no_text("Auto · choosing").await;
    let requests = fixture.routing_bodies.lock().unwrap().len();
    fixture.terminal.prompt("/model gpt-6-astra", "\r");
    fixture
        .terminal
        .wait_text("The model can only be changed before the first prompt")
        .await;
    fixture.terminal.prompt("/thinking low", "\r");
    fixture
        .terminal
        .wait_text("effort is fixed after the first prompt")
        .await;
    fixture.terminal.prompt("MANUAL_MODEL_FOLLOWUP", "\r");
    let followup = fixture.submission("MANUAL_MODEL_FOLLOWUP").await;
    fixture.complete(&followup);
    fixture.terminal.wait_text("done").await;
    assert_eq!(fixture.routing_bodies.lock().unwrap().len(), requests);
    println!(
        "Verified terminal after manual routing and follow-up:\n{}",
        fixture.terminal.screen.lock().unwrap().screen().contents()
    );
}

// Exercises local reuse through two real terminal processes without server history.
#[tokio::test]
async fn terminal_prompt_cache_survives_restart_and_scopes_sessions() {
    const OTHER: &str = "019fc927-b280-79a7-8445-1b9996ad2fc1";
    let mut fixture = Fixture::start().await;
    let original = "CACHE_EXACT_短\n  preserve indentation\n\nlast line";
    fixture.terminal.prompt(original, "\r");
    let turn = fixture.submission(original).await;
    fixture.complete(&turn);
    fixture.terminal.wait_text("Enter send").await;
    // The newer loose match must rank below the older exact match.
    let distractor = "C A C H E E X A C T distractor";
    fixture.terminal.prompt(distractor, "\r");
    let turn = fixture.submission(distractor).await;
    fixture.complete(&turn);
    fixture.terminal.wait_text("Enter send").await;
    // Exit directly; no picker lookup may mask a missed background save.
    let account_home = fixture.terminal._workspace.path().join(".codex");
    fixture.terminal.input("\x03\x03");
    fixture.terminal.wait_output("\x1b[?1049l").await;
    tokio::time::timeout(TIMEOUT, async {
        while fixture.terminal.child.try_wait().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("prompt cache flush must finish on exit");
    fixture.history.lock().unwrap().clear();

    let mut reopened = Terminal::start_with_command(&fixture.origin, false, None, |command| {
        command.args(["attach", OTHER]);
        command.env("CODEX_HOME", &account_home);
    });
    let events = tokio::time::timeout(TIMEOUT, fixture.connections.recv())
        .await
        .unwrap()
        .unwrap();
    reopened.wait_text("Enter send").await;
    reopened.prompt("UNSENT_DRAFT_短", "");
    reopened.input("\x12");
    reopened.wait_text("Recent prompts").await;
    reopened.wait_text("CACHE_EXACT_短").await;
    reopened.input("\x06");
    reopened.wait_text("Current session").await;
    reopened.wait_text("No prompts in this scope").await;
    reopened.input("\x1b");
    reopened.wait_no_text("Recent prompts").await;
    reopened.wait_text("UNSENT_DRAFT_短").await;
    assert!(fixture.submissions.try_recv().is_err());
    reopened.input("\x12");
    reopened.wait_text("Recent prompts").await;
    reopened.prompt("cacheexact", "");
    reopened.wait_text("cacheexact").await;
    reopened.wait_text("CACHE_EXACT_短").await;
    eprintln!(
        "prompt cache after process restart, empty remote history, and fuzzy lookup:\n{}",
        reopened.screen.lock().unwrap().screen().contents()
    );
    reopened.input("\r");
    reopened.wait_no_text("Recent prompts").await;
    reopened.wait_text("preserve indentation").await;
    assert!(
        fixture.submissions.try_recv().is_err(),
        "picker selection must only edit the draft"
    );
    reopened.input("\r");
    let sent = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prompt_text(&sent["input"]), original);
    let turn = sent["id"].as_str().unwrap();
    events.send(json!({"type":"turn_accepted","id":turn,"turn_id":turn,"cursor":"1","input":sent["input"],"replayed":false})).unwrap();
    events.send(json!({"type":"turn_completed","id":turn,"turn_id":turn,"cursor":"2","final_message":"CACHE_REUSE_CONFIRMED","usage":null,"citations":[],"usage_error":null})).unwrap();
    reopened.wait_text("CACHE_REUSE_CONFIRMED").await;

    // Same service, different login credential: no prompts from the prior login.
    let mut different_login =
        Terminal::start_with_command(&fixture.origin, true, None, |command| {
            command.env("CODEX_HOME", &account_home);
            command.env(
                "NANOCODEX_API_KEY",
                format!("ncx_live_{}_{}", "a".repeat(12), "c".repeat(43)),
            );
        });
    different_login.wait_text("Enter send").await;
    different_login.input("\x12");
    different_login.wait_text("Recent prompts").await;
    different_login.wait_text("No prompts in this scope").await;
    eprintln!("same service, different login: cached prompts isolated");

    // A separate service using the same local storage must not see the first cache.
    let other_service = Fixture::start().await;
    let mut isolated = Terminal::start_with_command(&other_service.origin, true, None, |command| {
        command.env("CODEX_HOME", &account_home);
    });
    isolated.wait_text("Enter send").await;
    isolated.input("\x12");
    isolated.wait_text("Recent prompts").await;
    isolated.wait_text("No prompts in this scope").await;
    eprintln!(
        "different service with shared local home:\n{}",
        isolated.screen.lock().unwrap().screen().contents()
    );
}

#[tokio::test]
async fn terminal_prompt_cache_merges_concurrent_terminals_and_preserves_corruption() {
    const OTHER: &str = "019fc927-b280-79a7-8445-1b9996ad2fc2";
    let mut fixture = Fixture::start().await;
    let account_home = fixture.terminal._workspace.path().join(".codex");
    let mut peer = Terminal::start_with_command(&fixture.origin, false, None, |command| {
        command.args(["attach", OTHER]);
        command.env("CODEX_HOME", &account_home);
    });
    let _peer_events = tokio::time::timeout(TIMEOUT, fixture.connections.recv())
        .await
        .unwrap()
        .unwrap();
    peer.wait_text("Enter send").await;
    fixture.terminal.prompt("CONCURRENT_PROMPT_ALPHA", "\r");
    peer.prompt("CONCURRENT_PROMPT_BETA", "\r");
    let mut sent = Vec::new();
    for _ in 0..2 {
        let input = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
            .await
            .unwrap()
            .unwrap();
        sent.push(prompt_text(&input["input"]));
    }
    sent.sort();
    assert_eq!(sent, ["CONCURRENT_PROMPT_ALPHA", "CONCURRENT_PROMPT_BETA"]);
    // Each terminal waits for its own merge; reopening then sees both writers.
    for terminal in [&mut fixture.terminal, &mut peer] {
        terminal.input("\x12");
        terminal.wait_text("Recent prompts").await;
        terminal.input("\x1b");
        terminal.wait_no_text("Recent prompts").await;
    }
    peer.input("\x12");
    peer.wait_text("Recent prompts").await;
    peer.wait_text("CONCURRENT_PROMPT_ALPHA").await;
    peer.wait_text("CONCURRENT_PROMPT_BETA").await;
    eprintln!(
        "concurrent writers preserved:\n{}",
        peer.screen.lock().unwrap().screen().contents()
    );
    peer.input("\x06");
    peer.wait_text("Current session").await;
    peer.wait_text("CONCURRENT_PROMPT_BETA").await;
    peer.wait_no_text("CONCURRENT_PROMPT_ALPHA").await;
    peer.input("\x1b");
    peer.wait_no_text("Recent prompts").await;
    peer.prompt("DRAFT_SURVIVES_CORRUPTION", "");
    let entries = std::fs::read_dir(account_home.join("prompt-history")).unwrap();
    let data = entries
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&data).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    // A blocked cache read must not prevent cancellation or replace a later overlay.
    peer.input("\x03");
    peer.wait_no_text("DRAFT_SURVIVES_CORRUPTION").await;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(data.with_extension("json.lock"))
        .unwrap();
    lock.lock().unwrap();
    peer.input("\x12");
    peer.wait_text("Loading recent prompts").await;
    peer.input("\x1b");
    peer.wait_no_text("Loading recent prompts").await;
    peer.prompt("/id", "\r");
    peer.wait_text("Agent ID").await;
    // The file operation has a one-second timeout; observe after that response.
    tokio::time::sleep(Duration::from_millis(1300)).await;
    peer.wait_text("Agent ID").await;
    peer.wait_no_text("Recent prompts").await;
    drop(lock);
    peer.input("\x1b");
    peer.wait_no_text("Agent ID").await;
    peer.prompt("DRAFT_SURVIVES_CORRUPTION", "");
    std::fs::write(&data, b"corrupt-history-for-recovery-journey").unwrap();
    peer.input("\x12");
    peer.wait_text("Recent prompts").await;
    peer.wait_text("CONCURRENT_PROMPT_BETA").await;
    peer.input("\x1b");
    peer.wait_no_text("Recent prompts").await;
    peer.wait_text("DRAFT_SURVIVES_CORRUPTION").await;
    peer.wait_text("Saved prompt history unavailable").await;
    assert_eq!(
        std::fs::read(&data).unwrap(),
        b"corrupt-history-for-recovery-journey"
    );
    assert!(fixture.submissions.try_recv().is_err());
    eprintln!(
        "corrupt cache retained with editable local draft:\n{}",
        peer.screen.lock().unwrap().screen().contents()
    );
}

async fn copy_journey_expect(fixture: &mut Fixture, command: &str, key: &str, expected: &str) {
    let start = fixture.terminal.output.lock().unwrap().len();
    fixture.terminal.prompt(command, key);
    let actual = tokio::time::timeout(TIMEOUT, async {
        loop {
            let payload = {
                let bytes = fixture.terminal.output.lock().unwrap();
                let output = String::from_utf8_lossy(&bytes[start..]);
                output.split_once("\x1b]52;c;").and_then(|(_, rest)| {
                    rest.split_once('\x07')
                        .map(|(encoded, _)| encoded.to_owned())
                })
            };
            if let Some(encoded) = payload {
                break String::from_utf8(
                    base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .unwrap(),
                )
                .unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "no clipboard output for {command:?}: {}",
            fixture.terminal.screen.lock().unwrap().screen().contents()
        )
    });
    assert_eq!(actual, expected, "raw Markdown for {command:?}");
    eprintln!("PTY command={command:?} key={key:?}; decoded OSC52={actual:?}");
}

async fn copy_journey_error(fixture: &mut Fixture, command: &str, expected: &str) {
    fixture.terminal.prompt(command, "\r");
    fixture.terminal.wait_text(expected).await;
    eprintln!(
        "PTY rejected {command:?}: {}",
        fixture.terminal.screen.lock().unwrap().screen().contents()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_copy_keeps_raw_markdown_and_skips_unfinished_messages() {
    let mut fixture = Fixture::start().await;
    copy_journey_error(&mut fixture, "/copy", "No completed assistant response").await;
    fixture.terminal.prompt("COPY_STREAM_JOURNEY", "\r");
    let turn = fixture.submission("COPY_STREAM_JOURNEY").await;
    fixture.nested(&turn, "assistant.delta", json!({"model_call_index": 1, "item_id": "first-copy", "phase": "final_answer", "text": "COPY_PARTIAL_ONLY"}));
    fixture.terminal.wait_text("COPY_PARTIAL_ONLY").await;
    copy_journey_error(&mut fixture, "/copy", "No completed assistant response").await;

    let first = "# COPY_FIRST\n\n**bold** and [source](https://example.test/copy)\n\n```rust\nlet answer = 42;\n```\n";
    fixture.nested(&turn, "assistant.message", json!({"model_call_index": 1, "item_id": "first-copy", "phase": "final_answer", "text": first}));
    fixture.terminal.wait_text("COPY_FIRST").await;
    // A completed message is available while its turn is still running.
    copy_journey_expect(&mut fixture, "/copy", "\r", first).await;
    fixture.emit(
        &turn,
        json!({"type":"event", "agent_id":1, "event": {
            "protocol_version":1, "request_id":"copy-child", "seq":fixture.cursor+1,
            "type":"assistant.message", "payload":{"model_call_index":1,"item_id":"child-answer",
            "phase":"final_answer","text":"COPY_CHILD_MUST_NOT_REPLACE_PARENT"}
        }}),
    );
    fixture.nested(&turn, "assistant.delta", json!({"model_call_index": 2, "item_id": "second-copy", "phase": "final_answer", "text": "COPY_SECOND_STREAM"}));
    fixture.terminal.wait_text("COPY_SECOND_STREAM").await;
    // Tab normally queues a live-turn prompt; /copy must remain local there too.
    copy_journey_expect(&mut fixture, "/copy 1", "\t", first).await;
    copy_journey_error(
        &mut fixture,
        "/copy 2",
        "No completed assistant response at position 2",
    )
    .await;

    let second = "## COPY_SECOND_COMPLETE\n\n- café 界\n- `raw_markdown`\n";
    fixture.nested(&turn, "assistant.message", json!({"model_call_index": 2, "item_id": "second-copy", "phase": "final_answer", "text": second}));
    fixture.terminal.wait_text("COPY_SECOND_COMPLETE").await;
    copy_journey_expect(&mut fixture, "/copy", "\r", second).await;
    copy_journey_expect(&mut fixture, "/copy 2", "\r", first).await;
    fixture.complete(&turn);
    fixture.terminal.wait_text("Enter send").await;
    copy_journey_expect(&mut fixture, "/copy 2", "\r", first).await;
    copy_journey_error(
        &mut fixture,
        "/copy 3",
        "No completed assistant response at position 3",
    )
    .await;

    for command in [
        "/copy 0",
        "/copy -1",
        "/copy nope",
        "/copy 1 2",
        "/copy 999999999999999999999999999999999999",
    ] {
        copy_journey_error(&mut fixture, command, "Usage: /copy [N]").await;
    }
    // Exercise typed action-menu arguments as well as bracketed paste commands.
    fixture.terminal.input("/copy response");
    fixture.terminal.wait_text("copy response").await;
    fixture.terminal.input("\r");
    fixture.terminal.wait_text("Usage: /copy [N]").await;

    // A successful copy is a terminal-input barrier after all rejected commands.
    copy_journey_expect(&mut fixture, "/copy", "\r", second).await;
    let output = fixture.terminal.output.lock().unwrap().clone();
    assert_eq!(
        String::from_utf8_lossy(&output)
            .matches("\x1b]52;c;")
            .count(),
        6,
        "errors must not copy and a streamed item must not count"
    );
    // The next real prompt must be the next submission: no copy command may have
    // escaped as input, a queued follow-up, or a live steering request.
    fixture.terminal.prompt("COPY_SUBMISSION_BARRIER", "\r");
    let barrier = fixture.submission("COPY_SUBMISSION_BARRIER").await;
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());
    fixture.complete(&barrier);
    fixture.terminal.wait_text("Enter send").await;
    assert!(fixture.submissions.try_recv().is_err());
    eprintln!(
        "COPY journey: six exact clipboard payloads; no copy submission or steer\n{}",
        fixture.terminal.screen.lock().unwrap().screen().contents()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_copy_reads_restored_history_before_live_completion() {
    let older = "# COPY_HISTORY_OLDER\n\n**original Markdown**\n";
    let newer = "# COPY_HISTORY_NEWER\n\n[source](https://example.test/history)\n";
    let second_turn = "019fc927-b282-79a7-8445-1b9996ad2fb0";
    let history = vec![
        json!({"cursor": "1", "turn_id": REMOTE_TURN, "type": "turn_accepted", "id": REMOTE_TURN, "input": "COPY_HISTORY_FIRST_PROMPT", "replayed": false}),
        json!({"cursor": "2", "turn_id": REMOTE_TURN, "type": "turn_completed", "id": REMOTE_TURN, "final_message": older, "usage": null, "citations": [], "usage_error": null}),
        json!({"cursor": "3", "turn_id": second_turn, "type": "turn_accepted", "id": second_turn, "input": "COPY_HISTORY_SECOND_PROMPT", "replayed": false}),
        json!({"cursor": "4", "turn_id": second_turn, "type": "turn_completed", "id": second_turn, "final_message": newer, "usage": null, "citations": [], "usage_error": null}),
    ];
    let mut fixture = Fixture::start_with_history(false, true, history).await;
    fixture.terminal.wait_text("COPY_HISTORY_NEWER").await;
    copy_journey_expect(&mut fixture, "/copy", "\r", newer).await;
    copy_journey_expect(&mut fixture, "/copy 2", "\r", older).await;
    fixture.terminal.prompt("COPY_AFTER_RESTORE", "\r");
    let turn = fixture.submission("COPY_AFTER_RESTORE").await;
    fixture.nested(&turn, "assistant.delta", json!({"model_call_index": 1, "item_id": "restored-live", "phase": "final_answer", "text": "COPY_RESTORED_LIVE_STREAM"}));
    fixture
        .terminal
        .wait_text("COPY_RESTORED_LIVE_STREAM")
        .await;
    copy_journey_expect(&mut fixture, "/copy", "\r", newer).await;
    copy_journey_expect(&mut fixture, "/copy 2", "\r", older).await;
    copy_journey_error(
        &mut fixture,
        "/copy 3",
        "No completed assistant response at position 3",
    )
    .await;
    let live = "# COPY_RESTORED_LIVE_FINAL\n\n`unchanged bytes`\n";
    fixture.nested(&turn, "assistant.message", json!({"model_call_index": 1, "item_id": "restored-live", "phase": "final_answer", "text": live}));
    fixture.complete(&turn);
    fixture.terminal.wait_text("Enter send").await;
    copy_journey_expect(&mut fixture, "/copy", "\r", live).await;
    copy_journey_expect(&mut fixture, "/copy 3", "\r", older).await;
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());
    let output = fixture.terminal.output.lock().unwrap().clone();
    assert_eq!(
        String::from_utf8_lossy(&output)
            .matches("\x1b]52;c;")
            .count(),
        6
    );
    eprintln!(
        "COPY history journey: restored indexes stay stable during streaming and shift once on completion\n{}",
        fixture.terminal.screen.lock().unwrap().screen().contents()
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_prompt_cache_flushes_failed_and_coalesced_writes_on_exit() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("CACHE_RETRY_SEED", "\r");
    let seed = fixture.submission("CACHE_RETRY_SEED").await;
    fixture.complete(&seed);
    fixture.terminal.wait_text("Enter send").await;
    // Establish the data and lock files before introducing contention. This is
    // the last picker read before exit, so a later lookup cannot repair a save.
    fixture.terminal.input("\x12");
    fixture.terminal.wait_text("Recent prompts").await;
    fixture.terminal.wait_text("CACHE_RETRY_SEED").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_no_text("Recent prompts").await;
    let account_home = fixture.terminal._workspace.path().join(".codex");
    let data = std::fs::read_dir(account_home.join("prompt-history"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(data.with_extension("json.lock"))
        .unwrap();
    lock.lock().unwrap();
    let locked_at = std::time::Instant::now();
    for prompt in ["CACHE_RETRY_FIRST_短", "CACHE_RETRY_COALESCED_SECOND"] {
        fixture.terminal.prompt(prompt, "\r");
        let turn = fixture.submission(prompt).await;
        fixture.complete(&turn);
        fixture.terminal.wait_text("Enter send").await;
    }
    // The second submission arrives while the first write is blocked. Keep
    // the lock through the one-second attempt and the delayed one-second retry.
    fixture
        .terminal
        .wait_text("Could not save recent prompts")
        .await;
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let blocked_data = std::fs::read_to_string(&data).unwrap();
    assert!(!blocked_data.contains("CACHE_RETRY_FIRST_短"));
    assert!(!blocked_data.contains("CACHE_RETRY_COALESCED_SECOND"));
    eprintln!(
        "cache lock held {:?}; both real submissions completed but background save failed; before unlock:\n{}",
        locked_at.elapsed(),
        fixture.terminal.screen.lock().unwrap().screen().contents()
    );
    drop(lock);
    // Exit immediately after releasing the lock. Do not use Ctrl+R: shutdown
    // must retain and flush the failed batch together with coalesced prompts.
    fixture.terminal.input("\x03\x03");
    fixture.terminal.wait_output("\x1b[?1049l").await;
    tokio::time::timeout(TIMEOUT, async {
        while fixture.terminal.child.try_wait().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("failed prompt cache writes must flush before process exit");
    fixture.history.lock().unwrap().clear();
    let mut reopened = Terminal::start_with_command(&fixture.origin, true, None, |command| {
        command.env("CODEX_HOME", &account_home);
    });
    let _events = tokio::time::timeout(TIMEOUT, fixture.connections.recv())
        .await
        .unwrap()
        .unwrap();
    reopened.wait_text("Enter send").await;
    reopened.input("\x12");
    reopened.wait_text("Recent prompts").await;
    reopened.wait_text("CACHE_RETRY_FIRST_短").await;
    reopened.wait_text("CACHE_RETRY_COALESCED_SECOND").await;
    assert!(fixture.submissions.try_recv().is_err());
    eprintln!(
        "restart with empty server history recovered both failed/coalesced writes from CODEX_HOME={} without a pre-exit picker read:\n{}",
        account_home.display(),
        reopened.screen.lock().unwrap().screen().contents()
    );
}

// Linux permits arbitrary filename bytes; APFS rejects this cwd with EILSEQ.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_prompt_cache_persists_from_a_non_utf8_workspace() {
    use std::os::unix::ffi::OsStringExt;

    let mut fixture = Fixture::start().await;
    let local = tempfile::tempdir().unwrap();
    let workspace = local.path().join(std::ffi::OsString::from_vec(
        b"prompt-cache-workspace-\xff".to_vec(),
    ));
    std::fs::create_dir(&workspace).unwrap();
    assert!(workspace.to_str().is_none());
    let account_home = local.path().join("account-home");
    // Start the shipped executable in an actual non-UTF-8 cwd, rather than
    // manufacturing JSON (which cannot represent an invalid UTF-8 path).
    fixture.terminal = Terminal::start_with_command(&fixture.origin, true, None, |command| {
        command.cwd(&workspace);
        command.env("CODEX_HOME", &account_home);
    });
    fixture.events = tokio::time::timeout(TIMEOUT, fixture.connections.recv())
        .await
        .unwrap()
        .unwrap();
    fixture.terminal.wait_text("Enter send").await;
    let prompt = "CACHE_NON_UTF8_WORKSPACE_短\n  preserve this prompt exactly";
    fixture.terminal.prompt(prompt, "\r");
    let turn = fixture.submission(prompt).await;
    fixture.complete(&turn);
    fixture.terminal.wait_text("Enter send").await;
    fixture.terminal.input("\x03\x03");
    fixture.terminal.wait_output("\x1b[?1049l").await;
    tokio::time::timeout(TIMEOUT, async {
        while fixture.terminal.child.try_wait().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("non-UTF-8 workspace prompt must flush before process exit");
    fixture.history.lock().unwrap().clear();
    let mut reopened = Terminal::start_with_command(&fixture.origin, true, None, |command| {
        command.env("CODEX_HOME", &account_home);
    });
    let _events = tokio::time::timeout(TIMEOUT, fixture.connections.recv())
        .await
        .unwrap()
        .unwrap();
    reopened.wait_text("Enter send").await;
    reopened.input("\x12");
    reopened.wait_text("Recent prompts").await;
    reopened.wait_text("CACHE_NON_UTF8_WORKSPACE_短").await;
    reopened.input("\r");
    reopened.wait_no_text("Recent prompts").await;
    reopened.wait_text("preserve this prompt exactly").await;
    assert!(fixture.submissions.try_recv().is_err());
    reopened.input("\r");
    let sent = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prompt_text(&sent["input"]), prompt);
    eprintln!(
        "non-UTF-8 native cwd={:?}; persisted exact prompt after process exit and cleared server history: {prompt:?}\n{}",
        workspace.as_os_str(),
        reopened.screen.lock().unwrap().screen().contents()
    );
}

// These journeys stub only the managed service. Commands, native overlays,
// terminal input and streamed replies all pass through the shipped executable.
fn review_journey_snapshot(fixture: &Fixture, step: &str) {
    eprintln!(
        "REVIEW PTY {step}\n{}",
        fixture.terminal.screen.lock().unwrap().screen().contents()
    );
}

// Use actual refs in the terminal's cwd; Git discovery is part of the journey.
fn review_journey_git(fixture: &Fixture, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(fixture.terminal._workspace.path())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", fixture.terminal._workspace.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("Git must be available for the branch picker journey");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn review_journey_commit(fixture: &Fixture) {
    review_journey_git(
        fixture,
        &[
            "-c",
            "user.name=Review Fixture",
            "-c",
            "user.email=review-fixture@example.test",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--allow-empty",
            "-m",
            "Branch picker fixture",
        ],
    );
}

async fn review_journey_branches(fixture: &mut Fixture) {
    review_journey_menu(fixture).await;
    fixture.terminal.input("\r");
    fixture.terminal.wait_text("Search branches").await;
}

async fn review_journey_menu(fixture: &mut Fixture) {
    // Exercise the typed slash-action path as well as the pasted inline commands
    // below. Enter must open the native picker rather than send literal /review.
    fixture.terminal.input("/review");
    fixture.terminal.wait_text("Search: review").await;
    fixture.terminal.input("\r");
    fixture.terminal.wait_text("Review").await;
    let labels = ["Base branch", "Uncommitted", "Commit", "Custom"];
    for label in labels {
        fixture.terminal.wait_text(label).await;
    }
    let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
    let positions = labels.map(|label| screen.find(label).unwrap());
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "review choices must be in keyboard navigation order: {screen}"
    );
    review_journey_snapshot(fixture, "typed /review + Enter opens scope menu");
}

async fn review_journey_reply(fixture: &mut Fixture, required: &[&str], reply: &str) {
    let request = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
        .await
        .unwrap_or_else(|_| {
            panic!(
                "review target {required:?} never reached managed transport:\n{}",
                fixture.terminal.screen.lock().unwrap().screen().contents()
            )
        })
        .expect("managed service must remain connected");
    let input = prompt_text(&request["input"]);
    eprintln!("REVIEW HTTP expected target={required:?}; received={request}");
    for target in required {
        assert!(
            input.contains(target),
            "requested review target {target:?} was lost: {input}"
        );
    }
    // Check the review contract, not one frozen rendering of the full prompt.
    let instructions = input.to_lowercase();
    assert!(instructions.contains("review"), "{input}");
    assert!(instructions.contains("finding"), "{input}");
    assert!(
        instructions.contains("read-only")
            || instructions.contains("read only")
            || [
                "do not edit",
                "do not modify",
                "do not change",
                "never edit"
            ]
            .iter()
            .any(|prohibition| instructions.contains(prohibition)),
        "review must explicitly prohibit changing the code: {input}"
    );
    let turn = request["id"].as_str().unwrap().to_owned();
    fixture.emit(
        &turn,
        json!({"type": "turn_accepted", "id": turn, "input": request["input"], "replayed": false}),
    );
    fixture.nested(
        &turn,
        "assistant.message",
        json!({"model_call_index": 1, "item_id": reply, "phase": "final_answer", "text": reply}),
    );
    fixture.complete(&turn);
    fixture.terminal.wait_text(reply).await;
    fixture.terminal.wait_text("Enter send").await;
    review_journey_snapshot(fixture, &format!("service reply visible: {reply}"));
    assert!(fixture.submissions.try_recv().is_err(), "duplicate review");
    assert!(
        fixture.steers.try_recv().is_err(),
        "review escaped as steering"
    );
}

async fn review_journey_normal_turn(fixture: &mut Fixture, prompt: &str) {
    // The exact next transport input is a barrier against delayed or queued
    // commands escaping cancellation, validation, or the busy guard.
    fixture.terminal.prompt(prompt, "\r");
    let turn = fixture.submission(prompt).await;
    let reply = format!("NORMAL_REPLY_{prompt}");
    fixture.nested(
        &turn,
        "assistant.message",
        json!({"model_call_index": 1, "item_id": "normal-after-review", "phase": "final_answer", "text": reply}),
    );
    fixture.complete(&turn);
    fixture.terminal.wait_text(&reply).await;
    fixture.terminal.wait_text("Enter send").await;
    assert!(fixture.submissions.try_recv().is_err());
    assert!(fixture.steers.try_recv().is_err());
    assert!(fixture.cancellations.try_recv().is_err());
    review_journey_snapshot(
        fixture,
        &format!("normal recovery prompt={prompt:?}, reply={reply:?}"),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_review_picker_cancels_and_submits_each_scope() {
    eprintln!(
        "Reproduce: cargo test --locked -p nanocodex-bin --test nanocodex2_tui_lifecycle terminal_review_ -- --nocapture"
    );
    let mut fixture = Fixture::start().await;
    review_journey_git(&fixture, &["init", "--initial-branch=main"]);
    review_journey_commit(&fixture);
    review_journey_git(&fixture, &["branch", "review-target/release-42"]);
    review_journey_menu(&mut fixture).await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_no_text("Base branch").await;
    review_journey_normal_turn(&mut fixture, "AFTER_REVIEW_MENU_CANCEL").await;

    review_journey_menu(&mut fixture).await;
    fixture.terminal.input("\r");
    fixture.terminal.wait_text("Search branches").await;
    fixture.terminal.input("CANCELLED_REVIEW_BASE");
    fixture.terminal.wait_text("CANCELLED_REVIEW_BASE").await;
    review_journey_snapshot(&fixture, "branch search typed; Esc must return to choices");
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_text("Uncommitted").await;
    fixture.terminal.wait_text("Custom").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_no_text("Base branch").await;
    review_journey_normal_turn(&mut fixture, "AFTER_REVIEW_INPUT_CANCEL").await;

    for (index, target, required, reply) in [
        (
            0,
            Some("review-target/release-42"),
            "review-target/release-42",
            "REVIEW_MENU_BASE_RESULT",
        ),
        (1, None, "uncommitted", "REVIEW_MENU_UNCOMMITTED_RESULT"),
        (
            2,
            Some("deadbeef0"),
            "deadbeef0",
            "REVIEW_MENU_COMMIT_RESULT",
        ),
        (
            3,
            Some("Check parser bounds and café handling"),
            "Check parser bounds and café handling",
            "REVIEW_MENU_CUSTOM_RESULT",
        ),
    ] {
        review_journey_menu(&mut fixture).await;
        fixture.terminal.input(&"\x1b[B".repeat(index));
        fixture.terminal.input("\r");
        if let Some(target) = target {
            if index == 0 {
                fixture.terminal.wait_text("Search branches").await;
                fixture.terminal.wait_text("review-target/release-42").await;
            }
            fixture.terminal.input(target);
            fixture.terminal.wait_text(target).await;
            if index == 0 {
                fixture.terminal.wait_no_text("(current)").await;
            }
            review_journey_snapshot(
                &fixture,
                &format!("choice={index}, typed target={target:?}; Enter submits"),
            );
            fixture.terminal.input("\r");
        }
        review_journey_reply(&mut fixture, &[required], reply).await;
    }
    review_journey_normal_turn(&mut fixture, "AFTER_ALL_REVIEW_SCOPES").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_review_branch_picker_navigates_filters_and_refreshes() {
    let mut fixture = Fixture::start().await;
    review_journey_git(&fixture, &["init", "--initial-branch=main"]);
    review_journey_commit(&fixture);
    for branch in ["alpha", "café", "zeta"] {
        review_journey_git(&fixture, &["branch", branch]);
    }
    review_journey_git(
        &fixture,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    review_journey_git(
        &fixture,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );

    // Default selection is main, ahead of alphabetically earlier local refs.
    // Subsequent requests prove that navigation changes the submitted ref.
    for (keys, target, reply) in [
        ("", "main", "REVIEW_BRANCH_DEFAULT_RESULT"),
        (
            "\x1b[F\x1b[H\x1b[B\x1b[A\x1b[B",
            "origin/main",
            "REVIEW_BRANCH_NAVIGATION_RESULT",
        ),
        ("\x1b[F", "zeta", "REVIEW_BRANCH_END_RESULT"),
    ] {
        review_journey_branches(&mut fixture).await;
        fixture.terminal.wait_text("origin/main").await;
        fixture.terminal.wait_text("(current)").await;
        fixture.terminal.wait_no_text("origin/HEAD").await;
        fixture.terminal.input(keys);
        review_journey_snapshot(
            &fixture,
            &format!("navigation keys={keys:?}; expected ref={target}"),
        );
        fixture.terminal.input("\r");
        let selected_scope = format!("\"base_ref\":{}", json!(target));
        review_journey_reply(&mut fixture, &[&selected_scope], reply).await;
    }

    // A same-named tag must not silently change the selected branch target.
    review_journey_git(&fixture, &["tag", "alpha"]);
    review_journey_branches(&mut fixture).await;
    fixture.terminal.wait_text("heads/alpha").await;
    fixture.terminal.input("ALPHA");
    fixture.terminal.wait_text("ALPHA").await;
    fixture.terminal.wait_no_text("(current)").await;
    review_journey_snapshot(
        &fixture,
        "alpha tag collision displays and submits heads/alpha",
    );
    fixture.terminal.input("\r");
    review_journey_reply(
        &mut fixture,
        &["\"base_ref\":\"heads/alpha\""],
        "REVIEW_BRANCH_DISAMBIGUATED_RESULT",
    )
    .await;

    review_journey_branches(&mut fixture).await;
    fixture.terminal.wait_text("origin/main").await;
    fixture.terminal.input("NO_SUCH_BRANCH_937");
    fixture.terminal.wait_text("No matching branches.").await;
    fixture.terminal.input("\r");
    fixture.terminal.wait_text("Search branches").await;
    review_journey_snapshot(&fixture, "Enter on no matches leaves picker open");
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_text("Uncommitted").await;
    fixture.terminal.input("\x1b");
    // "Search branches" is already gone after the first Esc; wait for the scope
    // menu itself to close so the next paste cannot race the second Esc.
    fixture.terminal.wait_no_text("Base branch").await;
    review_journey_normal_turn(&mut fixture, "AFTER_BRANCH_NO_MATCH_ENTER").await;

    review_journey_branches(&mut fixture).await;
    fixture.terminal.wait_text("origin/main").await;
    // A pasted uppercase, noncontiguous query must match the Unicode ref.
    fixture.terminal.prompt("CFÉ", "");
    fixture.terminal.wait_text("CFÉ").await;
    fixture.terminal.wait_text("café").await;
    fixture.terminal.wait_no_text("(current)").await;
    review_journey_snapshot(&fixture, "pasted CFÉ fuzzy-matches café");
    fixture.terminal.input("\r");
    review_journey_reply(&mut fixture, &["café"], "REVIEW_BRANCH_UNICODE_RESULT").await;

    review_journey_branches(&mut fixture).await;
    fixture.terminal.wait_text("origin/main").await;
    fixture.terminal.input("no-match-before-clear");
    fixture.terminal.wait_text("No matching branches.").await;
    fixture.terminal.input("\x15");
    fixture.terminal.wait_text("origin/main").await;
    fixture.terminal.wait_no_text("No matching branches.").await;
    fixture.terminal.input("OGMN");
    fixture.terminal.wait_text("OGMN").await;
    fixture.terminal.wait_no_text("(current)").await;
    fixture.terminal.wait_text("origin/main").await;
    review_journey_snapshot(
        &fixture,
        "Ctrl-U clears search; typed OGMN matches origin/main",
    );
    fixture.terminal.input("\r");
    review_journey_reply(&mut fixture, &["origin/main"], "REVIEW_BRANCH_FUZZY_RESULT").await;

    // Populate once, close it, mutate actual refs, and reopen to catch stale caches.
    review_journey_branches(&mut fixture).await;
    fixture.terminal.wait_text("origin/main").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_text("Uncommitted").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_no_text("Base branch").await;
    review_journey_normal_turn(&mut fixture, "AFTER_BRANCH_ESCAPE_CANCEL").await;
    review_journey_git(&fixture, &["branch", "fresh-after-reopen"]);
    review_journey_branches(&mut fixture).await;
    fixture.terminal.wait_text("fresh-after-reopen").await;
    fixture.terminal.input("FRESH");
    fixture.terminal.wait_text("FRESH").await;
    fixture.terminal.wait_no_text("(current)").await;
    fixture.terminal.input("\r");
    review_journey_reply(
        &mut fixture,
        &["fresh-after-reopen"],
        "REVIEW_BRANCH_REFRESH_RESULT",
    )
    .await;
    review_journey_normal_turn(&mut fixture, "AFTER_BRANCH_PICKER_JOURNEY").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_review_branch_picker_empty_and_nonrepo_recover_without_submitting() {
    let mut fixture = Fixture::start().await;
    review_journey_branches(&mut fixture).await;
    fixture.terminal.wait_text("Could not load branches.").await;
    fixture.terminal.input("\r");
    fixture.terminal.wait_text("Search branches").await;
    review_journey_snapshot(&fixture, "non-repository Enter does not submit");
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_text("Uncommitted").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_no_text("Base branch").await;
    review_journey_normal_turn(&mut fixture, "AFTER_BRANCH_LOAD_ERROR").await;

    review_journey_git(&fixture, &["init", "--initial-branch=main"]);
    review_journey_branches(&mut fixture).await;
    fixture.terminal.wait_text("No branches found.").await;
    fixture.terminal.input("\r");
    fixture.terminal.wait_text("Search branches").await;
    review_journey_snapshot(&fixture, "unborn repository Enter does not submit");
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_text("Uncommitted").await;
    fixture.terminal.input("\x1b");
    // As above: wait for the scope menu, not the already-closed branch search.
    fixture.terminal.wait_no_text("Base branch").await;
    review_journey_normal_turn(&mut fixture, "AFTER_BRANCH_EMPTY_REPO").await;

    review_journey_commit(&fixture);
    review_journey_branches(&mut fixture).await;
    fixture.terminal.wait_text("(current)").await;
    fixture.terminal.input("\r");
    review_journey_reply(
        &mut fixture,
        &["\"base_ref\":\"main\""],
        "REVIEW_BRANCH_RECOVERED_RESULT",
    )
    .await;
    review_journey_normal_turn(&mut fixture, "AFTER_BRANCH_REPOSITORY_RECOVERY").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_review_inline_scopes_validate_locally_and_recover() {
    let mut fixture = Fixture::start().await;
    for (command, required, reply) in [
        (
            "/review --uncommitted",
            "uncommitted",
            "REVIEW_INLINE_UNCOMMITTED_RESULT",
        ),
        (
            "/review --base origin/review-base",
            "origin/review-base",
            "REVIEW_INLINE_BASE_RESULT",
        ),
        (
            "/review --commit HEAD~2",
            "HEAD~2",
            "REVIEW_INLINE_COMMIT_RESULT",
        ),
        (
            "/review Check Unicode bounds in src/parser.rs",
            "Check Unicode bounds in src/parser.rs",
            "REVIEW_INLINE_CUSTOM_RESULT",
        ),
    ] {
        eprintln!("REVIEW PTY inline command={command:?} + Enter");
        fixture.terminal.prompt(command, "\r");
        review_journey_reply(&mut fixture, &[required], reply).await;
    }
    for command in [
        "/review --base",
        "/review --commit",
        "/review --unknown",
        "/review --uncommitted extra",
        "/review --base main --commit HEAD",
    ] {
        fixture.terminal.prompt(command, "");
        fixture.terminal.wait_text(command).await;
        fixture.terminal.input("\r");
        fixture.terminal.wait_text("Usage: /review").await;
        review_journey_snapshot(
            &fixture,
            &format!("invalid command={command:?} shows usage"),
        );
    }
    review_journey_normal_turn(&mut fixture, "AFTER_REVIEW_USAGE_ERRORS").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_review_busy_rejects_without_steering_or_queueing() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("WORK_ACTIVE_DURING_REVIEW", "\r");
    let active = fixture.submission("WORK_ACTIVE_DURING_REVIEW").await;
    fixture.terminal.wait_text("Enter steer").await;
    for command in ["/review", "/review --uncommitted"] {
        fixture.terminal.prompt(command, "");
        fixture.terminal.wait_text(command).await;
        fixture.terminal.input("\r");
        fixture
            .terminal
            .wait_text("Finish active work before starting a review")
            .await;
        review_journey_snapshot(
            &fixture,
            &format!("busy command={command:?} rejected locally"),
        );
        assert!(fixture.submissions.try_recv().is_err());
        assert!(fixture.steers.try_recv().is_err());
        assert!(fixture.cancellations.try_recv().is_err());
    }
    fixture.complete(&active);
    fixture.terminal.wait_text("Enter send").await;
    review_journey_normal_turn(&mut fixture, "AFTER_BUSY_REVIEW").await;
    fixture
        .terminal
        .prompt("/review --base release/after-busy", "\r");
    review_journey_reply(
        &mut fixture,
        &["release/after-busy"],
        "REVIEW_AFTER_BUSY_RESULT",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_review_interrupts_and_returns_to_normal_chat() {
    let mut fixture = Fixture::start().await;
    fixture.terminal.prompt("/review --uncommitted", "\r");
    let request = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
        .await
        .unwrap()
        .unwrap();
    let turn = request["id"].as_str().unwrap().to_owned();
    fixture.emit(
        &turn,
        json!({"type":"turn_accepted","id":turn,"input":request["input"],"replayed":false}),
    );
    fixture.nested(
        &turn,
        "assistant.delta",
        json!({"model_call_index":1,"item_id":"review-progress","phase":"commentary","text":"INSPECTING_REVIEW_DIFF"}),
    );
    fixture.terminal.wait_text("INSPECTING_REVIEW_DIFF").await;
    fixture.terminal.input("\x1b");
    fixture.terminal.wait_text("Interrupt").await;
    fixture.terminal.input("\x1b");
    assert_eq!(
        tokio::time::timeout(TIMEOUT, fixture.cancellations.recv())
            .await
            .unwrap()
            .unwrap(),
        turn
    );
    fixture.emit(&turn, json!({"type":"turn_cancelled","id":turn}));
    fixture.terminal.wait_text("Enter send").await;
    review_journey_snapshot(&fixture, "Esc twice cancels the streamed review turn");
    review_journey_normal_turn(&mut fixture, "AFTER_REVIEW_INTERRUPT").await;
}

// Reproduce with NANOCODEX_INLINE_REVIEW_EVIDENCE=/absolute/output/inline-review
// cargo test --locked -p nanocodex-bin --test nanocodex2_tui_lifecycle terminal_inline_review -- --nocapture
fn inline_review_evidence(terminal: &Terminal, name: &str, markdown: &str) -> String {
    let screen = terminal.screen.lock().unwrap().screen().contents();
    eprintln!("INLINE REVIEW {name}\ninput Markdown:\n{markdown}\nobserved screen:\n{screen}");
    if let Some(directory) = std::env::var_os("NANOCODEX_INLINE_REVIEW_EVIDENCE") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(format!("{name}.screen.txt")), &screen).unwrap();
        std::fs::write(
            directory.join(format!("{name}.ansi")),
            &*terminal.output.lock().unwrap(),
        )
        .unwrap();
        std::fs::write(directory.join(format!("{name}.md")), markdown).unwrap();
    }
    screen
}

fn inline_review_diff(fixture: &Fixture) -> String {
    review_journey_git(fixture, &["init", "--initial-branch=main"]);
    let file = fixture.terminal._workspace.path().join("review limits.rs");
    std::fs::write(
        &file,
        format!("fn limit() {{\n    let opening = 0;\n    let before = 1;\n    let ceiling = 40;\n    let after = 2;\n    let closing = 3;\n}}\n{}fn unrelated() {{ let UNRELATED_CONTEXT = 2; }}\n", "\n".repeat(14)),
    )
    .unwrap();
    review_journey_git(fixture, &["add", "review limits.rs"]);
    review_journey_commit(fixture);
    std::fs::write(
        &file,
        format!("fn limit() {{\n    let opening = 0;\n    let before = 1;\n    let ceiling = 99;\n    let after = 2;\n    let closing = 3;\n}}\n{}fn unrelated() {{ let UNRELATED_CONTEXT = 3; }}\n", "\n".repeat(14)),
    )
    .unwrap();
    let output = std::process::Command::new("git")
        .args([
            "--no-pager",
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--unified=3",
            "--",
            "review limits.rs",
        ])
        .current_dir(fixture.terminal._workspace.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

fn inline_review_markdown(diff: &str, side: &str, line: usize, title: &str, body: &str) -> String {
    format!(
        "```review\n{}\n```\n",
        json!({
            "file":"review limits.rs", "side":side, "line_start":line, "line_end":line,
            "title":title, "body":body, "diff":diff,
        })
    )
}

async fn inline_review_begin(fixture: &mut Fixture) -> String {
    fixture.terminal.prompt("/review --uncommitted", "\r");
    let request = tokio::time::timeout(TIMEOUT, fixture.submissions.recv())
        .await
        .unwrap()
        .unwrap();
    let input = prompt_text(&request["input"]);
    assert!(input.contains("uncommitted"), "{input}");
    let turn = request["id"].as_str().unwrap().to_owned();
    fixture.emit(
        &turn,
        json!({"type":"turn_accepted", "id":turn, "input":request["input"], "replayed":false}),
    );
    turn
}

fn inline_review_finish(fixture: &mut Fixture, turn: &str, markdown: &str) {
    fixture.nested(
        turn,
        "assistant.message",
        json!({"model_call_index":1,
        "item_id":"inline-finding", "phase":"final_answer", "text":markdown}),
    );
    fixture.complete(turn);
}

fn inline_review_order(screen: &str, ordered: &[&str]) {
    let positions: Vec<_> = ordered
        .iter()
        .map(|text| {
            screen
                .find(text)
                .unwrap_or_else(|| panic!("missing {text:?}:\n{screen}"))
        })
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "expected visual order {ordered:?}:\n{screen}"
    );
}

// Inspect emitted terminal cells, including ANSI attributes, rather than private
// renderer data. The old/new signs and Rust token colors must survive the PTY.
async fn inline_review_anchor(terminal: &Terminal, old_side: bool) {
    // The PTY reader can stop between a clear and the next frame. Inspect one
    // complete visible frame, including its color attributes, after repaint.
    let screen = tokio::time::timeout(TIMEOUT, async {
        loop {
            let screen = terminal.screen.lock().unwrap().screen().clone();
            let (rows, cols) = screen.size();
            let lines: Vec<String> = (0..rows)
                .map(|row| {
                    (0..cols)
                        .map(|col| {
                            let cell = screen.cell(row, col).unwrap().contents();
                            if cell.is_empty() {
                                " ".to_owned()
                            } else {
                                cell
                            }
                        })
                        .collect()
                })
                .collect();
            if ["let ceiling = 40;", "let ceiling = 99;", "let before = 1;"]
                .iter()
                .all(|needle| lines.iter().any(|line| line.contains(needle)))
            {
                break screen;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("complete review code frame visible");
    let (rows, cols) = screen.size();
    let code_row = |needle: &str| {
        (0..rows)
            .find(|row| {
                (0..cols)
                    .map(|col| {
                        let cell = screen.cell(*row, col).unwrap().contents();
                        if cell.is_empty() {
                            " ".to_owned()
                        } else {
                            cell
                        }
                    })
                    .collect::<String>()
                    .contains(needle)
            })
            .expect("review code row visible")
    };
    let deleted = code_row("let ceiling = 40;");
    let added = code_row("let ceiling = 99;");
    let before = code_row("let before = 1;");
    let row_text = |row| {
        (0..cols)
            .map(|col| {
                let cell = screen.cell(row, col).unwrap().contents();
                if cell.is_empty() {
                    " ".to_owned()
                } else {
                    cell
                }
            })
            .collect::<String>()
    };
    let removed = row_text(deleted);
    let inserted = row_text(added);
    assert!(removed.contains("4   │-"), "old line gutter: {removed}");
    assert!(inserted.contains("  4 │+"), "new line gutter: {inserted}");
    assert!(
        row_text(before).contains("3 3 │"),
        "context must show both absolute line numbers"
    );
    assert_eq!(removed.contains('┃'), old_side, "{removed}");
    assert_eq!(inserted.contains('┃'), !old_side, "{inserted}");
    let sign_color = |row, sign: &str| {
        let col = (0..cols)
            .find(|col| screen.cell(row, *col).unwrap().contents() == sign)
            .unwrap();
        screen.cell(row, col).unwrap().fgcolor()
    };
    assert_ne!(
        sign_color(deleted, "-"),
        sign_color(added, "+"),
        "addition/deletion signs need distinct colors"
    );
    let text = row_text(added);
    let start = text[..text.find("let ceiling").unwrap()].chars().count() as u16;
    assert_ne!(
        screen.cell(added, start).unwrap().fgcolor(),
        screen.cell(added, start + 4).unwrap().fgcolor(),
        "Rust keyword and identifier should retain syntax highlighting"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_inline_review_streams_wraps_copies_and_restores_snapshot() {
    let mut fixture = Fixture::start().await;
    let diff = inline_review_diff(&fixture);
    let markdown = inline_review_markdown(
        &diff,
        "new",
        4,
        "[P1] Cap the ceiling",
        "WRAP_START This permits requests above the documented safe limit and must retain all of this explanation when the terminal is narrow. WRAP_END",
    );
    let turn = inline_review_begin(&mut fixture).await;
    // Split inside the fence and JSON value: neither network framing nor an
    // incomplete JSON object may make the eventual finding disappear.
    let cuts = [
        0,
        5,
        markdown.find("ceiling").unwrap() + 4,
        markdown.len() - 4,
        markdown.len(),
    ];
    for (index, pair) in cuts.windows(2).enumerate() {
        fixture.nested(&turn, "assistant.delta", json!({"model_call_index":1,
            "item_id":"inline-finding", "phase":"final_answer", "text":&markdown[pair[0]..pair[1]]}));
        tokio::time::sleep(Duration::from_millis(80)).await;
        inline_review_evidence(&fixture.terminal, &format!("new-stream-{index}"), &markdown);
    }
    fixture.terminal.wait_text("Cap the ceiling").await;
    fixture.terminal.wait_text("┃").await;
    fixture.terminal.wait_text("let after = 2;").await;
    let screen = inline_review_evidence(&fixture.terminal, "new-stream-complete", &markdown);
    assert!(
        !screen.contains("UNRELATED_CONTEXT"),
        "only the finding hunk belongs in its card: {screen}"
    );
    inline_review_order(
        &screen,
        &[
            "let before = 1;",
            "let ceiling = 40;",
            "let ceiling = 99;",
            "Cap the ceiling",
            "WRAP_END",
            "let after = 2;",
        ],
    );
    inline_review_anchor(&fixture.terminal, false).await;
    inline_review_finish(&mut fixture, &turn, &markdown);
    fixture.terminal.wait_text("Enter send").await;
    fixture.terminal.resize(68);
    fixture.terminal.wait_text("WRAP_END").await;
    // A resize is asynchronous. Wait until the long explanation actually wraps.
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
            if !screen
                .lines()
                .any(|line| line.contains("WRAP_START") && line.contains("WRAP_END"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let screen = inline_review_evidence(&fixture.terminal, "new-narrow", &markdown);
    inline_review_order(
        &screen,
        &[
            "let ceiling = 99;",
            "Cap the ceiling",
            "WRAP_START",
            "WRAP_END",
            "let after = 2;",
        ],
    );
    copy_journey_expect(&mut fixture, "/copy", "\r", &markdown).await;
    inline_review_evidence(&fixture.terminal, "new-copy", &markdown);

    // Reopen via the real attach command, against recorded service history,
    // after the file no longer contains either version of the reviewed line.
    std::fs::write(
        fixture.terminal._workspace.path().join("review limits.rs"),
        "WORKSPACE_REPLACED\n",
    )
    .unwrap();
    fixture.terminal.input("\x03\x03");
    fixture.terminal.wait_output("\x1b[?1049l").await;
    let mut attached = Terminal::start_with_command(&fixture.origin, true, None, |command| {
        command.cwd(fixture.terminal._workspace.path());
    });
    attached.wait_text("Enter send").await;
    attached.wait_text("Cap the ceiling").await;
    let screen = inline_review_evidence(&attached, "new-attach-after-workspace-change", &markdown);
    inline_review_order(
        &screen,
        &[
            "let before = 1;",
            "let ceiling = 40;",
            "let ceiling = 99;",
            "Cap the ceiling",
            "WRAP_END",
            "let after = 2;",
        ],
    );
    assert!(screen.contains("Review uncommitted changes"), "{screen}");
    assert!(
        !screen.contains("line_start"),
        "internal review instructions must stay out of the transcript: {screen}"
    );
    inline_review_anchor(&attached, false).await;
    assert!(!screen.contains("WORKSPACE_REPLACED"));
    attached.input("\x03\x03");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_inline_review_old_side_and_ordinary_diff_remain_readable() {
    let mut fixture = Fixture::start().await;
    let diff = inline_review_diff(&fixture);
    let markdown = inline_review_markdown(
        &diff,
        "old",
        4,
        "[P2] Preserve the cap",
        "DELETION_COMMENT Keep the existing guard.",
    );
    let turn = inline_review_begin(&mut fixture).await;
    inline_review_finish(&mut fixture, &turn, &markdown);
    fixture.terminal.wait_text("DELETION_COMMENT").await;
    fixture.terminal.wait_text("Enter send").await;
    let screen = inline_review_evidence(&fixture.terminal, "old-anchor", &markdown);
    inline_review_anchor(&fixture.terminal, true).await;
    inline_review_order(
        &screen,
        &[
            "let before = 1;",
            "let ceiling = 40;",
            "Preserve the cap",
            "DELETION_COMMENT",
            "let ceiling = 99;",
            "let after = 2;",
        ],
    );
    copy_journey_expect(&mut fixture, "/copy", "\r", &markdown).await;

    fixture.terminal.prompt("Show the ordinary patch", "\r");
    let turn = fixture.submission("Show the ordinary patch").await;
    let ordinary = format!("ORDINARY_PATCH\n\n```diff\n{diff}```\n");
    inline_review_finish(&mut fixture, &turn, &ordinary);
    fixture.terminal.wait_text("ORDINARY_PATCH").await;
    fixture.terminal.wait_text("Enter send").await;
    let screen = inline_review_evidence(&fixture.terminal, "ordinary-diff", &ordinary);
    let ordinary_screen = screen.split_once("ORDINARY_PATCH").unwrap().1;
    inline_review_order(
        ordinary_screen,
        &[
            "let before = 1;",
            "let ceiling = 40;",
            "let ceiling = 99;",
            "let after = 2;",
        ],
    );
    assert!(!ordinary_screen.contains("DELETION_COMMENT"));
    copy_journey_expect(&mut fixture, "/copy", "\r", &ordinary).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_inline_review_unavailable_context_preserves_findings() {
    let mut fixture = Fixture::start().await;
    let diff = inline_review_diff(&fixture);
    // A missing line, a different file, and malformed context cannot acquire an
    // apparently precise inline anchor from the current workspace.
    let cases = [
        ("unmatched-line", inline_review_markdown(&diff, "new", 999, "[P1] Missing anchor", "UNMATCHED_FINDING")),
        ("wrong-file", inline_review_markdown(&diff.replace("review limits.rs", "other.rs"), "old", 4, "[P1] Wrong file", "WRONG_FILE_FINDING")),
        ("missing-diff", inline_review_markdown("", "new", 4, "[P1] Missing context", "MISSING_CONTEXT_FINDING")),
        ("incomplete-hunk", inline_review_markdown(&diff.replace("@@ -1,7 +1,7 @@", "@@ -1,8 +1,7 @@"), "new", 4, "[P1] Truncated context", "INCOMPLETE_HUNK_FINDING")),
        ("malformed-json", "```review\n{\"title\":\"[P1] Malformed finding\",\"body\":\"MALFORMED_FINDING\"\n```\n".to_owned()),
    ];
    for (name, markdown) in cases {
        let turn = inline_review_begin(&mut fixture).await;
        inline_review_finish(&mut fixture, &turn, &markdown);
        let marker = match name {
            "unmatched-line" => "UNMATCHED_FINDING",
            "wrong-file" => "WRONG_FILE_FINDING",
            "missing-diff" => "MISSING_CONTEXT_FINDING",
            "incomplete-hunk" => "INCOMPLETE_HUNK_FINDING",
            _ => "MALFORMED_FINDING",
        };
        fixture.terminal.wait_text(marker).await;
        fixture.terminal.wait_text("Enter send").await;
        let screen = inline_review_evidence(&fixture.terminal, name, &markdown);
        if name != "malformed-json" {
            // Locate the current card: prior turns may also have fallback labels.
            let current = screen.rsplit_once("Review").expect("review card visible").1;
            assert!(
                current.to_lowercase().contains("context unavailable"),
                "{current}"
            );
            assert!(
                !current.contains('┃'),
                "unavailable context must not mark a code anchor: {current}"
            );
            assert!(
                !current.contains("let ceiling"),
                "unmatched code must not masquerade as an anchored excerpt: {current}"
            );
        }
        copy_journey_expect(&mut fixture, "/copy", "\r", &markdown).await;
    }
}

// Folded tool batches use the classic CLI summary: one "Tools" header with the
// call count and duration, then one terse row per call. Failures stay visible,
// long batches elide quiet rows, and Ctrl+O discloses every call's details.
#[tokio::test]
async fn terminal_tool_batches_fold_into_classic_summaries() {
    let mut fixture = Fixture::start_with_active(true).await;
    let evidence = std::env::var_os("NANOCODEX_TUI_EVIDENCE");
    let dump = |label: &str, fixture: &Fixture| {
        let contents = fixture.terminal.screen.lock().unwrap().screen().contents();
        if let Some(dir) = &evidence {
            std::fs::write(
                Path::new(dir).join(format!("{label}.screen.txt")),
                &contents,
            )
            .unwrap();
        }
        contents
    };
    let comment = |item: &str, text: &str| json!({"model_call_index": 1, "item_id": item, "phase": "commentary", "text": text});
    // Sequential calls are separated by real time so they do not read as parallel.
    async fn call(fixture: &mut Fixture, id: &str, tool: &str, arguments: Value) {
        tokio::time::sleep(Duration::from_millis(20)).await;
        fixture.nested(
            REMOTE_TURN,
            "tool.call",
            json!({"call_id": id, "tool": tool, "arguments": arguments}),
        );
    }
    fn finish(
        fixture: &mut Fixture,
        id: &str,
        tool: &str,
        status: &str,
        duration_ns: u64,
        result: Value,
    ) {
        fixture.nested(REMOTE_TURN, "tool.result", json!({"call_id": id, "tool": tool, "status": status, "duration_ns": duration_ns, "result": result}));
    }

    fixture.nested(
        REMOTE_TURN,
        "assistant.message",
        comment("c-one", "I'll look around the workspace first."),
    );
    call(&mut fixture, "cell1", "exec", json!("const [files, readme] = await Promise.all([tools.Glob({pattern: \"*.md\"}), tools.Read({file_path: \"/ws/README.md\"})]);\nawait tools.Bash({command: \"ls -la\"});\ntext(\"README_FIRST_LINE\");")).await;
    // Glob and Read run in parallel; Bash follows them.
    call(
        &mut fixture,
        "cell1/code-0",
        "Glob",
        json!({"pattern": "GLOB_PATTERN_*.md"}),
    )
    .await;
    fixture.nested(REMOTE_TURN, "tool.call", json!({"call_id": "cell1/code-1", "tool": "Read", "arguments": {"file_path": "/ws/README.md"}}));
    fixture.terminal.wait_text("2 calls").await;
    let running = dump("running", &fixture);
    assert!(
        running.lines().any(|line| line.contains("Tools  2 calls")),
        "{running}"
    );
    assert!(
        running.contains("◌ Glob") && running.contains("◌ Read"),
        "{running}"
    );
    tokio::time::sleep(Duration::from_millis(40)).await;
    finish(
        &mut fixture,
        "cell1/code-0",
        "Glob",
        "completed",
        40_000_000,
        json!({"filenames": ["README.md"], "numFiles": 1}),
    );
    finish(
        &mut fixture,
        "cell1/code-1",
        "Read",
        "completed",
        41_000_000,
        json!({"content": "RAW_README_CONTENT"}),
    );
    call(
        &mut fixture,
        "cell1/code-2",
        "Bash",
        json!({"command": "ls -la", "description": "List workspace"}),
    )
    .await;
    finish(
        &mut fixture,
        "cell1/code-2",
        "Bash",
        "completed",
        10_000_000,
        json!({"stdout": "RAW_LS_OUTPUT", "exit_code": 0}),
    );
    finish(
        &mut fixture,
        "cell1",
        "exec",
        "completed",
        95_000_000,
        json!([{"type": "input_text", "text": "README_FIRST_LINE # Demo project"}]),
    );

    fixture.nested(
        REMOTE_TURN,
        "assistant.message",
        comment("c-two", "Now checking for TODOs and the missing file."),
    );
    call(&mut fixture, "cell2", "exec", json!("await tools.Grep({pattern: \"TODO\"}); text(await tools.Bash({command: \"cat MISSING_FILE.txt\"}));")).await;
    call(
        &mut fixture,
        "cell2/code-0",
        "Grep",
        json!({"pattern": "TODO", "path": "/ws"}),
    )
    .await;
    finish(
        &mut fixture,
        "cell2/code-0",
        "Grep",
        "completed",
        3_000_000,
        json!({"filenames": ["README.md"], "numFiles": 1}),
    );
    call(
        &mut fixture,
        "cell2/code-1",
        "Bash",
        json!({"command": "cat MISSING_FILE.txt", "description": "Read missing file"}),
    )
    .await;
    finish(
        &mut fixture,
        "cell2/code-1",
        "Bash",
        "failed",
        11_000_000,
        json!({"stderr": "MISSING_FILE_ERROR: No such file or directory", "exit_code": 1}),
    );
    finish(
        &mut fixture,
        "cell2",
        "exec",
        "completed",
        24_000_000,
        json!([{"type": "input_text", "text": "exit_code: 1"}]),
    );

    fixture.nested(
        REMOTE_TURN,
        "assistant.message",
        comment("c-three", "Delegating a review, then sweeping files."),
    );
    call(&mut fixture, "cell3", "exec", json!("const a = await tools.spawn_agent({role: \"Reviewer\", task: \"Review README\"}); await tools.wait_agent({agent_ids: [a.agent_id]}); for (const f of files) await tools.Read({file_path: f});")).await;
    call(
        &mut fixture,
        "cell3/code-0",
        "spawn_agent",
        json!({"role": "Reviewer", "task": "Review README"}),
    )
    .await;
    finish(
        &mut fixture,
        "cell3/code-0",
        "spawn_agent",
        "completed",
        5_000_000,
        json!({"agent_id": 7, "status": "running"}),
    );
    call(
        &mut fixture,
        "cell3/code-1",
        "wait_agent",
        json!({"agent_ids": [7]}),
    )
    .await;
    finish(
        &mut fixture,
        "cell3/code-1",
        "wait_agent",
        "completed",
        1_500_000_000,
        json!({"agents": [{"agent_id": 7, "status": "completed", "output": "README is fine"}]}),
    );
    for index in 2..10 {
        let id = format!("cell3/code-{index}");
        call(
            &mut fixture,
            &id,
            "Read",
            json!({"file_path": format!("/ws/SWEEP_FILE_{index}.md")}),
        )
        .await;
        let (status, result) = if index == 4 {
            ("failed", json!({"error": "SWEEP_READ_DENIED"}))
        } else {
            ("completed", json!({"content": "RAW_SWEEP_CONTENT"}))
        };
        finish(&mut fixture, &id, "Read", status, 1_000_000, result);
    }
    finish(
        &mut fixture,
        "cell3",
        "exec",
        "completed",
        1_610_000_000,
        json!([{"type": "input_text", "text": "review done"}]),
    );
    fixture.nested(REMOTE_TURN, "assistant.message", json!({"model_call_index": 2, "item_id": "final", "phase": "final_answer", "text": "FINAL_ANSWER: one TODO."}));
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("FINAL_ANSWER").await;
    fixture.terminal.wait_text("Enter send").await;
    fixture.terminal.wait_text("10 calls").await;

    let folded = dump("folded", &fixture);
    for expected in [
        "✓ Tools  3 calls",
        "README_FIRST_LINE # Demo project",
        "GLOB_PATTERN_*.md",
        "/ws/README.md",
        "$ ls -la",
        "× Tools  2 calls · 1 failed",
        "MISSING_FILE_ERROR",
        "TODO",
        "Tools  10 calls · 1 failed",
        "Spawned",
        "Waited on",
        "SWEEP_READ_DENIED",
        "5 more · Ctrl+O",
    ] {
        assert!(
            folded.contains(expected),
            "folded summary lacks {expected:?}\n{folded}"
        );
    }
    // Glob and Read overlap, so they share a parallel branch; Bash closes the batch.
    assert!(
        folded
            .lines()
            .any(|line| line.contains("├─┬") && line.contains("Glob")),
        "{folded}"
    );
    assert!(
        folded
            .lines()
            .any(|line| line.contains("└──") && line.contains("ls -la")),
        "{folded}"
    );
    for raw in [
        "RAW_README_CONTENT",
        "RAW_LS_OUTPUT",
        "RAW_SWEEP_CONTENT",
        "SWEEP_FILE_2",
        "Local",
    ] {
        assert!(
            !folded.contains(raw),
            "folded summary leaked {raw:?}\n{folded}"
        );
    }

    fixture.terminal.input("\x0f");
    fixture.terminal.wait_text("RAW_SWEEP_CONTENT").await;
    dump("expanded", &fixture);
    // Ctrl+O's third state hides tool rows but keeps the conversation.
    fixture.terminal.input("\x0f");
    fixture.terminal.wait_no_text("RAW_SWEEP_CONTENT").await;
    fixture.terminal.wait_no_text("Tools  3 calls").await;
    let hidden = dump("hidden", &fixture);
    for kept in [
        "I’ll look around the workspace first.",
        "Now checking for TODOs",
        "FINAL_ANSWER",
    ] {
        assert!(hidden.contains(kept), "hidden mode lost {kept:?}\n{hidden}");
    }
    for gone in ["Tools", "MISSING_FILE", "Glob"] {
        assert!(
            !hidden.contains(gone),
            "hidden mode leaked {gone:?}\n{hidden}"
        );
    }
    fixture.terminal.input("\x0f");
    fixture.terminal.wait_text("5 more · Ctrl+O").await;
    fixture.terminal.wait_text("Tools  3 calls").await;
    dump("refolded", &fixture);
}

#[tokio::test]
async fn terminal_code_details_show_available_source_without_placeholder_blocks() {
    for (label, arguments, has_source) in [
        ("raw", json!("const SOURCE_MARKER = 42;"), true),
        ("code", json!({"code": "const SOURCE_MARKER = 42;"}), true),
        ("input", json!({"input": "const SOURCE_MARKER = 42;"}), true),
        ("missing", json!({}), false),
    ] {
        let mut fixture = Fixture::start_with_active(true).await;
        fixture.terminal.input("\x0f");
        fixture.nested(
            REMOTE_TURN,
            "tool.call",
            json!({
                "call_id": "source-cell", "tool": "exec", "arguments": arguments
            }),
        );
        fixture.nested(
            REMOTE_TURN,
            "tool.result",
            json!({
                "call_id": "source-cell", "tool": "exec", "status": "completed",
                "duration_ns": 1, "result": [{"type": "input_text", "text": "CELL_RESULT_MARKER"}]
            }),
        );
        fixture.complete(REMOTE_TURN);
        fixture.terminal.wait_text("CELL_RESULT_MARKER").await;
        fixture.terminal.wait_text("Enter send").await;
        let screen = fixture.terminal.screen.lock().unwrap().screen().contents();
        assert_eq!(
            screen.contains("SOURCE_MARKER"),
            has_source,
            "{label}: {screen}"
        );
        assert!(
            !screen.contains("<source unavailable>"),
            "{label}: {screen}"
        );
        if !has_source {
            assert!(!screen.contains("javascript"), "{label}: {screen}");
        }
        eprintln!("CODE SOURCE {label}\n{screen}");
    }
}

// ---------------------------------------------------------------------------
// Responsiveness journey: real PTY input-to-visible latency while an attached
// agent streams commentary, reasoning, and large Tools groups into a long
// transcript. Run explicitly:
//   NANOCODEX_TUI_PERF_OUT=/path/report.json cargo test -p nanocodex-bin \
//     --test nanocodex2_tui_lifecycle terminal_perf_ -- --ignored --nocapture
// Optional: NANOCODEX_TUI_PERF_INTERVAL_US (live event pacing, default 2000),
// NANOCODEX_TUI_PERF_HISTORY (preloaded cycles, default 120),
// NANOCODEX_TUI_PERF_CALLS (calls per live Tools group, default 24).

#[derive(Clone)]
struct PerfEmitter {
    events: mpsc::UnboundedSender<Value>,
    history: Arc<Mutex<Vec<Value>>>,
    cursor: Arc<std::sync::atomic::AtomicU64>,
    sent: Arc<std::sync::atomic::AtomicU64>,
}

impl PerfEmitter {
    fn from_fixture(fixture: &Fixture) -> Self {
        Self {
            events: fixture.events.clone(),
            history: fixture.history.clone(),
            cursor: Arc::new(std::sync::atomic::AtomicU64::new(fixture.cursor)),
            sent: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Same wire shape as Fixture::nested, safe to call from the live loader
    /// and the test body concurrently (the history lock orders cursors).
    fn nested(&self, turn: &str, kind: &str, payload: Value) {
        let mut history = self.history.lock().unwrap();
        let cursor = self.cursor.fetch_add(1, Ordering::SeqCst) + 1;
        let value = json!({"type": "event", "cursor": cursor.to_string(), "turn_id": turn,
            "event": {"protocol_version": 1, "request_id": AGENT, "seq": cursor,
                "type": kind, "payload": payload}});
        history.push(value.clone());
        let _ = self.events.send(value);
        drop(history);
        self.sent.fetch_add(1, Ordering::Relaxed);
    }
}

fn perf_env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// One agent "step": commentary, a Code Mode cell with a large nested Tools
/// group, reasoning summary deltas, then streamed commentary deltas.
async fn perf_cycle(
    emitter: &PerfEmitter,
    label: &str,
    step: usize,
    calls: usize,
    deltas: usize,
    pace: Duration,
    stop: &AtomicBool,
) -> bool {
    async fn tick(pace: Duration) {
        if !pace.is_zero() {
            tokio::time::sleep(pace).await;
        }
    }
    emitter.nested(REMOTE_TURN, "assistant.message", json!({
        "model_call_index": 1, "item_id": format!("{label}-c{step}"), "phase": "commentary",
        "text": format!("{label}_{step:04} reviewing module {step}.\nThe second line explains the plan for step {step} in detail.\nA third line keeps the history tall.")
    }));
    tick(pace).await;
    let cell = format!("{label}-cell{step}");
    emitter.nested(REMOTE_TURN, "tool.call", json!({
        "call_id": cell, "tool": "exec",
        "arguments": format!("for (const f of files) await tools.Read({{file_path: f}}); // {label} {step}")
    }));
    for call in 0..calls {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        tick(pace).await;
        let id = format!("{cell}/code-{call}");
        let (tool, arguments, result) = if call % 3 == 2 {
            (
                "Bash",
                json!({"command": format!("rg -n {label}_{step:04}_{call:03} src"), "description": "Search sources"}),
                json!({"stdout": format!("src/lib.rs:{call}: step_{step}_{call}\n").repeat(4), "exit_code": 0}),
            )
        } else {
            (
                "Read",
                json!({"file_path": format!("/ws/src/{label}_{step:04}_{call:03}.rs")}),
                json!({"content": format!("fn module_{step}_{call}() {{}}\n").repeat(20)}),
            )
        };
        emitter.nested(
            REMOTE_TURN,
            "tool.call",
            json!({"call_id": id, "tool": tool, "arguments": arguments}),
        );
        tick(pace).await;
        emitter.nested(REMOTE_TURN, "tool.result", json!({
            "call_id": id, "tool": tool, "status": "completed", "duration_ns": 2_000_000u64, "result": result
        }));
    }
    tick(pace).await;
    emitter.nested(
        REMOTE_TURN,
        "tool.result",
        json!({
            "call_id": cell, "tool": "exec", "status": "completed", "duration_ns": 50_000_000u64,
            "result": [{"type": "input_text", "text": format!("{label} cell {step} done")}]
        }),
    );
    for delta in 0..deltas {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        tick(pace).await;
        if delta % 4 == 0 {
            emitter.nested(REMOTE_TURN, "reasoning.summary.delta", json!({
                "model_call_index": 2 + step as u64, "text": format!("thinking about {label}_{step:04}_{:03}. ", calls + delta + 2)
            }));
        } else {
            emitter.nested(REMOTE_TURN, "assistant.delta", json!({
                "model_call_index": 1, "item_id": format!("{label}-d{step}"), "phase": "commentary",
                "text": format!("{label}_{step:04}_{:03} ", calls + delta + 2)
            }));
        }
    }
    true
}

fn perf_screen(screen: &Arc<Mutex<vt100::Parser>>) -> String {
    screen.lock().unwrap().screen().contents()
}

/// Busy-poll the parsed PTY screen (200us resolution) until done() holds.
fn perf_wait(
    screen: &Arc<Mutex<vt100::Parser>>,
    start: std::time::Instant,
    limit: Duration,
    done: impl Fn(&str) -> bool,
) -> Option<Duration> {
    tokio::task::block_in_place(|| {
        loop {
            let contents = perf_screen(screen);
            if done(&contents) {
                return Some(start.elapsed());
            }
            if start.elapsed() > limit {
                return None;
            }
            std::thread::sleep(Duration::from_micros(200));
        }
    })
}

/// Ordered transcript position markers visible on screen: commentary
/// "HIST_0007"/"LIVE_0003" and tool rows "HIST_0007_012" (label, step, call+1).
/// Sorted ascending, so first()/last() are the oldest/newest visible rows.
fn perf_markers(contents: &str) -> Vec<(u8, u32, u32)> {
    let mut found = Vec::new();
    for (label, rank) in [("HIST_", 0u8), ("LIVE_", 1u8)] {
        let mut rest = contents;
        while let Some(index) = rest.find(label) {
            let tail = &rest[index + 5..];
            let digits = |text: &str, count: usize| {
                (text.len() >= count && text.as_bytes()[..count].iter().all(u8::is_ascii_digit))
                    .then(|| text[..count].parse::<u32>().unwrap())
            };
            if let Some(step) = digits(tail, 4) {
                let call = tail[4..]
                    .strip_prefix('_')
                    .and_then(|call| digits(call, 3))
                    .map_or(0, |call| call + 1);
                found.push((rank, step, call));
            }
            rest = tail;
        }
    }
    found.sort_unstable();
    found
}

/// utime+stime clock ticks of the TUI process and its direct children.
fn perf_cpu_ticks(pid: u32) -> u64 {
    fn fields(path: &Path) -> Option<(u32, u64)> {
        let stat = std::fs::read_to_string(path).ok()?;
        let (_, after) = stat.rsplit_once(')')?;
        let fields: Vec<&str> = after.split_whitespace().collect();
        let ppid = fields.get(1)?.parse().ok()?;
        let ticks = fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?;
        Some((ppid, ticks))
    }
    let mut total = fields(Path::new(&format!("/proc/{pid}/stat"))).map_or(0, |(_, ticks)| ticks);
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            if let Some((ppid, ticks)) = fields(&entry.path().join("stat"))
                && ppid == pid
            {
                total += ticks;
            }
        }
    }
    total
}

fn perf_stats(samples: &[Duration]) -> Value {
    let mut ms: Vec<f64> = samples
        .iter()
        .map(|sample| sample.as_secs_f64() * 1000.0)
        .collect();
    ms.sort_by(|left, right| left.partial_cmp(right).unwrap());
    let pick = |quantile: f64| {
        if ms.is_empty() {
            0.0
        } else {
            ms[((ms.len() as f64 - 1.0) * quantile).round() as usize]
        }
    };
    let round = |value: f64| (value * 100.0).round() / 100.0;
    json!({"n": ms.len(), "p50_ms": round(pick(0.5)), "p95_ms": round(pick(0.95)),
        "max_ms": round(ms.last().copied().unwrap_or(0.0))})
}

struct PerfPhase {
    started: std::time::Instant,
    ticks: u64,
    bytes: usize,
    events: u64,
}

impl PerfPhase {
    fn begin(fixture: &Fixture, pid: u32, emitter: &PerfEmitter) -> Self {
        Self {
            started: std::time::Instant::now(),
            ticks: perf_cpu_ticks(pid),
            bytes: fixture.terminal.output.lock().unwrap().len(),
            events: emitter.sent.load(Ordering::Relaxed),
        }
    }

    /// CPU percent of one core (assumes CLK_TCK=100), PTY bytes and
    /// synchronized-update frames emitted, and events delivered in the phase.
    fn end(&self, fixture: &Fixture, pid: u32, emitter: &PerfEmitter) -> Value {
        let wall = self.started.elapsed().as_secs_f64();
        let ticks = perf_cpu_ticks(pid).saturating_sub(self.ticks);
        let output = fixture.terminal.output.lock().unwrap();
        let emitted = &output[self.bytes.min(output.len())..];
        let frames = emitted
            .windows(8)
            .filter(|window| *window == b"\x1b[?2026h")
            .count();
        let events = emitter.sent.load(Ordering::Relaxed) - self.events;
        json!({"wall_s": (wall * 1000.0).round() / 1000.0,
            "cpu_pct": ((ticks as f64 / wall) * 100.0).round() / 100.0,
            "cpu_ticks": ticks, "pty_bytes": emitted.len(), "sync_frames": frames,
            "events": events, "events_per_s": ((events as f64 / wall) * 10.0).round() / 10.0})
    }
}

fn perf_type_round(terminal: &mut Terminal, round: usize, samples: &mut Vec<Duration>) {
    let text = format!("Q{round:02}xkcdtypingprobeabcdefghijklmnopqrst");
    let screen = terminal.screen.clone();
    for (index, character) in text.char_indices() {
        std::thread::sleep(Duration::from_millis(8));
        let start = std::time::Instant::now();
        terminal.input(&character.to_string());
        let prefix = text[..=index].to_owned();
        let elapsed = perf_wait(&screen, start, Duration::from_secs(5), |contents| {
            contents.contains(&prefix)
        })
        .unwrap_or_else(|| {
            panic!(
                "typed {prefix:?} never became visible:\n{}",
                perf_screen(&screen)
            )
        });
        if index >= 4 {
            samples.push(elapsed);
        }
    }
    let start = std::time::Instant::now();
    terminal.input(&"\x7f".repeat(text.len()));
    let head = text[..5].to_owned();
    perf_wait(&screen, start, Duration::from_secs(5), |contents| {
        !contents.contains(&head)
    })
    .unwrap_or_else(|| panic!("draft {head:?} was not erased:\n{}", perf_screen(&screen)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "responsiveness journey; run explicitly with --ignored and NANOCODEX_TUI_PERF_OUT"]
async fn terminal_perf_input_stays_responsive_while_an_agent_streams_large_tool_groups() {
    let history_cycles = perf_env("NANOCODEX_TUI_PERF_HISTORY", 120) as usize;
    let live_calls = perf_env("NANOCODEX_TUI_PERF_CALLS", 24) as usize;
    let history_calls = perf_env("NANOCODEX_TUI_PERF_HISTORY_CALLS", 8) as usize;
    let pace = Duration::from_micros(perf_env("NANOCODEX_TUI_PERF_INTERVAL_US", 2000));
    let mut fixture = Fixture::start_with_active(true).await;
    let pid = fixture.terminal.child.process_id().expect("TUI pid");
    let screen = fixture.terminal.screen.clone();
    let emitter = PerfEmitter::from_fixture(&fixture);
    let never = AtomicBool::new(false);
    let mut report = serde_json::Map::new();

    // Long history: many commentary blocks and Tools groups in the active turn.
    let preload = PerfPhase::begin(&fixture, pid, &emitter);
    let start = std::time::Instant::now();
    for step in 0..history_cycles {
        perf_cycle(
            &emitter,
            "HIST",
            step,
            history_calls,
            12,
            Duration::ZERO,
            &never,
        )
        .await;
    }
    let last = format!("HIST_{:04}", history_cycles - 1);
    let settle = perf_wait(&screen, start, Duration::from_secs(60), |contents| {
        contents.contains(&last)
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    report.insert("preload".into(), json!({
        "cycles": history_cycles, "visible_after_ms": settle.map(|elapsed| elapsed.as_millis() as u64),
        "load": preload.end(&fixture, pid, &emitter)
    }));

    // Baseline typing with no live stream.
    let idle = PerfPhase::begin(&fixture, pid, &emitter);
    let mut idle_typing = Vec::new();
    perf_type_round(&mut fixture.terminal, 0, &mut idle_typing);
    perf_type_round(&mut fixture.terminal, 1, &mut idle_typing);
    report.insert(
        "idle_typing".into(),
        json!({"latency": perf_stats(&idle_typing), "load": idle.end(&fixture, pid, &emitter)}),
    );

    // Live agent stream: commentary + reasoning + large Tools groups.
    let stop = Arc::new(AtomicBool::new(false));
    let loader = {
        let emitter = emitter.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            let mut step = 0;
            while perf_cycle(&emitter, "LIVE", step, live_calls, 40, pace, &stop).await {
                step += 1;
            }
            step
        })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;

    let streaming = PerfPhase::begin(&fixture, pid, &emitter);
    let mut typing = Vec::new();
    for round in 2..6 {
        perf_type_round(&mut fixture.terminal, round, &mut typing);
    }
    report.insert(
        "streaming_typing".into(),
        json!({"latency": perf_stats(&typing), "load": streaming.end(&fixture, pid, &emitter)}),
    );

    // Scroll into the preloaded history and back while the stream continues.
    let scrolling = PerfPhase::begin(&fixture, pid, &emitter);
    let mut scroll_up = Vec::new();
    let mut scroll_down = Vec::new();
    let mut scroll_misses = 0;
    for _ in 0..8 {
        let before = perf_markers(&perf_screen(&screen));
        let start = std::time::Instant::now();
        fixture.terminal.input("\x1b[5~");
        // Older transcript content must enter the viewport; live appends never
        // satisfy this, and timers/spinners carry no markers.
        match perf_wait(&screen, start, Duration::from_secs(3), |contents| {
            let now = perf_markers(contents);
            match (now.first(), before.first()) {
                (Some(now), Some(before)) => now < before,
                (Some(_), None) => true,
                _ => false,
            }
        }) {
            Some(elapsed) => scroll_up.push(elapsed),
            None => {
                scroll_misses += 1;
                eprintln!("PageUp miss, before={before:?}:\n{}", perf_screen(&screen));
            }
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    // Expand and fold every Tools group while reading history (view change).
    let mut toggles = Vec::new();
    let mut toggle_misses = 0;
    for _ in 0..6 {
        let before = perf_screen(&screen);
        let before_rows: Vec<String> = before.lines().take(20).map(str::to_owned).collect();
        let start = std::time::Instant::now();
        fixture.terminal.input("\x0f");
        match perf_wait(&screen, start, Duration::from_secs(3), |contents| {
            contents
                .lines()
                .take(20)
                .map(str::to_owned)
                .collect::<Vec<_>>()
                != before_rows
        }) {
            Some(elapsed) => toggles.push(elapsed),
            None => toggle_misses += 1,
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    // Fewer PageDowns than PageUps so every step still has newer content below.
    for _ in 0..6 {
        let before = perf_markers(&perf_screen(&screen));
        let start = std::time::Instant::now();
        fixture.terminal.input("\x1b[6~");
        match perf_wait(&screen, start, Duration::from_secs(3), |contents| {
            let now = perf_markers(contents);
            match (now.last(), before.last()) {
                (Some(now), Some(before)) => now > before,
                (Some(_), None) => true,
                _ => false,
            }
        }) {
            Some(elapsed) => scroll_down.push(elapsed),
            None => {
                scroll_misses += 1;
                eprintln!(
                    "PageDown miss, before={before:?}:\n{}",
                    perf_screen(&screen)
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    fixture.terminal.input("\x1b[F");
    tokio::time::sleep(Duration::from_millis(200)).await;
    report.insert(
        "streaming_scroll".into(),
        json!({
            "page_up": perf_stats(&scroll_up), "page_down": perf_stats(&scroll_down),
            "toggle_tools": perf_stats(&toggles), "scroll_misses": scroll_misses,
            "toggle_misses": toggle_misses, "load": scrolling.end(&fixture, pid, &emitter)
        }),
    );

    // Queue a follow-up, move focus into and out of the queue pane.
    let queueing = PerfPhase::begin(&fixture, pid, &emitter);
    let start = std::time::Instant::now();
    fixture
        .terminal
        .input("\x1b[200~PERF_QUEUED_FOLLOWUP\x1b[201~");
    let paste = perf_wait(&screen, start, Duration::from_secs(5), |contents| {
        contents.contains("PERF_QUEUED_FOLLOWUP")
    })
    .expect("pasted follow-up visible");
    let start = std::time::Instant::now();
    fixture.terminal.input("\t");
    let queued = perf_wait(&screen, start, Duration::from_secs(5), |contents| {
        contents.contains("queue · enter steer latest")
    })
    .unwrap_or_else(|| panic!("queue never appeared:\n{}", perf_screen(&screen)));
    let mut focus = Vec::new();
    for round in 0..6 {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let entering = round % 2 == 0;
        let start = std::time::Instant::now();
        fixture.terminal.input("\t");
        focus.push(
            perf_wait(&screen, start, Duration::from_secs(5), |contents| {
                contents.contains("e edit") == entering
            })
            .unwrap_or_else(|| {
                panic!(
                    "queue focus={entering} never rendered:\n{}",
                    perf_screen(&screen)
                )
            }),
        );
    }
    report.insert("streaming_queue".into(), json!({
        "paste_visible_ms": paste.as_secs_f64() * 1000.0, "queue_visible_ms": queued.as_secs_f64() * 1000.0,
        "focus_toggle": perf_stats(&focus), "load": queueing.end(&fixture, pid, &emitter)
    }));

    // Steering: Enter to service receipt, then the acknowledgement round trip.
    let steering = PerfPhase::begin(&fixture, pid, &emitter);
    let mut steer_receipt = Vec::new();
    for index in 0..4 {
        tokio::time::sleep(Duration::from_millis(80)).await;
        let instruction = format!("PERF_STEER_{index}");
        let start = std::time::Instant::now();
        fixture.terminal.prompt(&instruction, "\r");
        let (steer, ack) = tokio::time::timeout(TIMEOUT, fixture.steers.recv())
            .await
            .expect("steer reached the service")
            .unwrap();
        steer_receipt.push(start.elapsed());
        assert_eq!(prompt_text(&steer["input"]), instruction);
        emitter.nested(
            REMOTE_TURN,
            "run.steered",
            json!({"steer_index": index + 1, "instruction_bytes": instruction.len()}),
        );
        ack.send(true).unwrap();
    }
    report.insert("streaming_steer".into(), json!({"enter_to_service": perf_stats(&steer_receipt), "load": steering.end(&fixture, pid, &emitter)}));

    // Cancel under load: first Esc shows the interrupt prompt, second reaches the service.
    let cancelling = PerfPhase::begin(&fixture, pid, &emitter);
    let start = std::time::Instant::now();
    fixture.terminal.input("\x1b");
    let prompt = perf_wait(&screen, start, Duration::from_secs(3), |contents| {
        contents.contains("Interrupt")
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let start = std::time::Instant::now();
    fixture.terminal.input("\x1b");
    let cancelled = tokio::time::timeout(Duration::from_secs(5), fixture.cancellations.recv())
        .await
        .ok()
        .flatten()
        .map(|turn| {
            assert_eq!(turn, REMOTE_TURN);
            start.elapsed()
        });
    report.insert(
        "streaming_cancel".into(),
        json!({
            "interrupt_prompt_ms": prompt.map(|elapsed| elapsed.as_secs_f64() * 1000.0),
            "esc_to_service_ms": cancelled.map(|elapsed| elapsed.as_secs_f64() * 1000.0),
            "load": cancelling.end(&fixture, pid, &emitter)
        }),
    );

    stop.store(true, Ordering::Relaxed);
    let live_steps = loader.await.unwrap();
    fixture.cursor = emitter.cursor.load(Ordering::SeqCst);
    fixture.emit(
        REMOTE_TURN,
        json!({"type": "turn_cancelled", "id": REMOTE_TURN}),
    );
    report.insert("config".into(), json!({
        "history_cycles": history_cycles, "history_calls": history_calls, "live_calls_per_group": live_calls,
        "pace_us": pace.as_micros() as u64, "live_steps": live_steps,
        "events_total": emitter.sent.load(Ordering::Relaxed),
        "terminal": "160x32 vt100 over portable-pty", "clk_tck_assumed": 100
    }));
    let report = Value::Object(report);
    println!("TUI_PERF {report}");
    if let Some(path) = std::env::var_os("NANOCODEX_TUI_PERF_OUT") {
        std::fs::write(path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    }
    assert!(
        settle.is_some(),
        "preloaded history never reached the viewport"
    );
    assert!(!typing.is_empty() && !steer_receipt.is_empty());
    assert!(
        cancelled.is_some(),
        "cancel never reached the service under load"
    );
    let scroll = &report["streaming_scroll"];
    assert_eq!(
        scroll["scroll_misses"], 0,
        "scroll steps never moved the viewport under load: {scroll}"
    );
    assert_eq!(
        scroll["toggle_misses"], 0,
        "tool mode changes were not visible under load: {scroll}"
    );
    // Meaningful but non-flaky ceilings: a frame budget miss is tolerated, a
    // visibly stuck composer or steer is not. Override for slow debug builds.
    let typing_p95 = perf_env("NANOCODEX_TUI_PERF_TYPING_P95_MS", 150) as f64;
    let typing_max = perf_env("NANOCODEX_TUI_PERF_TYPING_MAX_MS", 1000) as f64;
    let steer_p95 = perf_env("NANOCODEX_TUI_PERF_STEER_P95_MS", 1000) as f64;
    let streaming_typing = &report["streaming_typing"]["latency"];
    assert!(
        streaming_typing["p95_ms"].as_f64().unwrap() <= typing_p95
            && streaming_typing["max_ms"].as_f64().unwrap() <= typing_max,
        "typing lagged while the agent streamed: {streaming_typing}"
    );
    let steer = &report["streaming_steer"]["enter_to_service"];
    assert!(
        steer["p95_ms"].as_f64().unwrap() <= steer_p95,
        "steering lagged: {steer}"
    );
}

// Renderer parity journeys: math graphics, completion notifications, tool
// display modes and stream/view telemetry, all through the shipped binary.

fn renderer_evidence(name: &str, bytes: &[u8]) {
    if let Some(dir) = std::env::var_os("NANOCODEX_TUI_EVIDENCE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), bytes).unwrap();
    }
}

async fn attached_renderer_terminal(configure: impl FnOnce(&mut CommandBuilder)) -> Fixture {
    let mut fixture = Fixture::start_with_active(true).await;
    fixture.terminal = Terminal::start_with_command(&fixture.origin, true, None, configure);
    fixture.replacement_connection().await;
    fixture.terminal.wait_text("Enter steer").await;
    fixture
}

fn byte_count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

async fn wait_bytes(terminal: &Terminal, needle: &[u8]) {
    tokio::time::timeout(TIMEOUT, async {
        while byte_count(&terminal.output.lock().unwrap(), needle) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "terminal never emitted {:?}",
            String::from_utf8_lossy(needle)
        )
    });
}

/// PNG payloads of Kitty graphics uploads: APC "ESC _ G keys ; base64 ESC \",
/// chunked while a chunk carries m=1.
fn kitty_pngs(output: &[u8]) -> Vec<Vec<u8>> {
    let find = |haystack: &[u8], needle: &[u8]| {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    };
    let mut pngs = Vec::new();
    let mut payload = String::new();
    let mut rest = output;
    while let Some(start) = find(rest, b"\x1b_G") {
        let body = &rest[start + 3..];
        let Some(end) = find(body, b"\x1b\\") else {
            break;
        };
        let command = &body[..end];
        if let Some(separator) = command.iter().position(|byte| *byte == b';') {
            let keys = String::from_utf8_lossy(&command[..separator]).into_owned();
            payload.push_str(&String::from_utf8_lossy(&command[separator + 1..]));
            if !keys.split(',').any(|key| key == "m=1") {
                if let Ok(png) = base64::engine::general_purpose::STANDARD.decode(&payload)
                    && png.starts_with(b"\x89PNG")
                {
                    pngs.push(png);
                }
                payload.clear();
            }
        }
        rest = &body[end + 2..];
    }
    pngs
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_math_renders_kitty_images_and_falls_back_to_source() {
    let reply = "Euler:\n\n\\[e^{i\\pi} + 1 = 0\\]\n\nInline \\(x^2\\) beside text.\n\n$$\\frac{a}{b}$$\n\nMATH_DONE";
    let placeholder = "\u{10EEEE}".as_bytes();

    let mut kitty = attached_renderer_terminal(|command| {
        command.env("TERM", "xterm-kitty");
        command.env("NANOCODEX_TUI_GRAPHICS", "kitty");
    })
    .await;
    kitty.nested(
        REMOTE_TURN,
        "assistant.message",
        json!({"model_call_index": 1, "item_id": "math", "phase": "final_answer", "text": reply}),
    );
    kitty.complete(REMOTE_TURN);
    kitty.terminal.wait_text("MATH_DONE").await;
    wait_bytes(&kitty.terminal, placeholder).await;
    // Rendered formulas replace their TeX source on screen.
    kitty.terminal.wait_no_text("e^{i").await;
    kitty.terminal.wait_no_text("frac").await;
    // Inline formulas that need more than one row keep their source in line.
    kitty.terminal.wait_text("beside text.").await;
    let output = kitty.terminal.output.lock().unwrap().clone();
    let pngs = kitty_pngs(&output);
    assert!(
        pngs.len() >= 3,
        "expected three formula uploads, got {}",
        pngs.len()
    );
    renderer_evidence("math-kitty.raw", &output);
    renderer_evidence(
        "math-kitty.screen.txt",
        kitty
            .terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .as_bytes(),
    );
    for (index, png) in pngs.iter().enumerate() {
        renderer_evidence(&format!("math-kitty-formula-{index}.png"), png);
    }
    // Typing stays immediate after graphics uploads.
    let mut echoes = Vec::new();
    for probe in ["LAGPROBEA", "LAGPROBEB", "LAGPROBEC"] {
        let started = std::time::Instant::now();
        kitty.terminal.input(probe);
        kitty.terminal.wait_text(probe).await;
        echoes.push(started.elapsed());
    }
    renderer_evidence(
        "math-kitty-input-echo.txt",
        format!("{echoes:?}\n").as_bytes(),
    );
    assert!(
        echoes.iter().all(|echo| *echo < Duration::from_millis(500)),
        "{echoes:?}"
    );

    // Every other terminal keeps the formula source readable.
    let mut plain = attached_renderer_terminal(|_| {}).await;
    plain.nested(
        REMOTE_TURN,
        "assistant.message",
        json!({"model_call_index": 1, "item_id": "math", "phase": "final_answer", "text": reply}),
    );
    plain.complete(REMOTE_TURN);
    plain.terminal.wait_text("MATH_DONE").await;
    plain.terminal.wait_text("e^{i\\pi} + 1 = 0").await;
    plain.terminal.wait_text("$x^2$").await;
    plain.terminal.wait_text("\\frac{a}{b}").await;
    let output = plain.terminal.output.lock().unwrap().clone();
    assert_eq!(byte_count(&output, placeholder), 0);
    assert_eq!(byte_count(&output, b"\x1b_G"), 0);
    renderer_evidence(
        "math-fallback.screen.txt",
        plain
            .terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .as_bytes(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_notifies_finished_turns_only_while_unfocused() {
    let finished: &[u8] = b"\x1b]9;Nanocodex finished\x07";
    let attention: &[u8] = b"\x1b]9;Nanocodex needs attention\x07";
    let mut fixture = attached_renderer_terminal(|command| {
        command.env("TERM_PROGRAM", "kitty");
        command.env("NANOCODEX_TUI_GRAPHICS", "off");
    })
    .await;
    fixture.terminal.input("\x1b[O");
    tokio::time::sleep(Duration::from_millis(200)).await;
    fixture.nested(REMOTE_TURN, "assistant.message", json!({"model_call_index": 1, "item_id": "unfocused", "phase": "final_answer", "text": "UNFOCUSED_DONE"}));
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("UNFOCUSED_DONE").await;
    wait_bytes(&fixture.terminal, finished).await;

    // A focused terminal finishes silently.
    fixture.terminal.input("\x1b[I");
    tokio::time::sleep(Duration::from_millis(200)).await;
    fixture.terminal.prompt("FOCUSED_PROMPT", "\r");
    let focused = fixture.submission("FOCUSED_PROMPT").await;
    fixture.nested(&focused, "assistant.message", json!({"model_call_index": 1, "item_id": "focused", "phase": "final_answer", "text": "FOCUSED_DONE"}));
    fixture.complete(&focused);
    fixture.terminal.wait_text("FOCUSED_DONE").await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // A failure while away asks for attention.
    fixture.terminal.input("\x1b[O");
    tokio::time::sleep(Duration::from_millis(200)).await;
    fixture.terminal.prompt("FAILING_PROMPT", "\r");
    let failing = fixture.submission("FAILING_PROMPT").await;
    fixture.emit(
        &failing,
        json!({"type": "turn_failed", "id": failing, "error": "RENDERER_FAILURE"}),
    );
    fixture.terminal.wait_text("RENDERER_FAILURE").await;
    wait_bytes(&fixture.terminal, attention).await;
    let output = fixture.terminal.output.lock().unwrap().clone();
    assert_eq!(
        byte_count(&output, finished),
        1,
        "focused completion must not notify"
    );
    renderer_evidence("notification.raw", &output);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_hidden_tool_calls_keep_the_turn_working_and_ctrl_o_cycles() {
    let mut fixture = attached_renderer_terminal(|command| {
        command.env("NANOCODEX_TOOL_CALLS", "hidden");
    })
    .await;
    fixture.nested(REMOTE_TURN, "tool.call", json!({"call_id": "hidden-read", "tool": "read_file", "arguments": {"path": "HIDDEN_TOOL_PATH.txt"}}));
    fixture.nested(REMOTE_TURN, "assistant.delta", json!({"model_call_index": 1, "item_id": "visible", "phase": "final_answer", "text": "VISIBLE_AFTER_TOOL"}));
    fixture.terminal.wait_text("VISIBLE_AFTER_TOOL").await;
    fixture.terminal.wait_no_text("HIDDEN_TOOL_PATH").await;
    fixture.terminal.wait_text("Enter steer").await;
    renderer_evidence(
        "tools-hidden.screen.txt",
        fixture
            .terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .as_bytes(),
    );
    // Ctrl+O: hidden -> summaries -> every detail -> hidden.
    fixture.terminal.input("\x0f");
    fixture.terminal.wait_text("HIDDEN_TOOL_PATH").await;
    renderer_evidence(
        "tools-folded.screen.txt",
        fixture
            .terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .as_bytes(),
    );
    fixture.terminal.input("\x0f");
    fixture.terminal.wait_text("HIDDEN_TOOL_PATH").await;
    renderer_evidence(
        "tools-expanded.screen.txt",
        fixture
            .terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .as_bytes(),
    );
    fixture.terminal.input("\x0f");
    fixture.terminal.wait_no_text("HIDDEN_TOOL_PATH").await;
    fixture.nested(REMOTE_TURN, "tool.result", json!({"call_id": "hidden-read", "tool": "read_file", "status": "completed", "duration_ns": 1, "result": {"text": "file contents"}}));
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("Enter send").await;
    fixture.terminal.wait_no_text("HIDDEN_TOOL_PATH").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_stream_and_view_telemetry_reach_the_log_and_otlp() {
    let traces = Arc::new(Mutex::new(Vec::<u8>::new()));
    let captured = traces.clone();
    let collector = Router::new().route(
        "/v1/traces",
        post(move |body: axum::body::Bytes| {
            let captured = captured.clone();
            async move {
                captured.lock().unwrap().extend_from_slice(&body);
                axum::http::StatusCode::OK
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let collector_origin = format!("http://{}", listener.local_addr().unwrap());
    let collector_task = tokio::spawn(async move {
        axum::serve(listener, collector).await.unwrap();
    });
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("tui.log");
    let mut fixture = attached_renderer_terminal(|command| {
        command.env("NANOCODEX_LOG_FILE", &log);
        command.env("OTEL_EXPORTER_OTLP_ENDPOINT", &collector_origin);
        command.env("RUST_LOG", "warn,nanocodex=info");
        command.env("OTEL_LEVEL", "warn,nanocodex=info");
    })
    .await;
    for text in ["TELEMETRY_", "STREAM_", "DONE"] {
        fixture.nested(REMOTE_TURN, "assistant.delta", json!({"model_call_index": 1, "item_id": "telemetry", "phase": "final_answer", "text": text}));
    }
    fixture.terminal.wait_text("TELEMETRY_STREAM_DONE").await;
    fixture.nested(REMOTE_TURN, "assistant.message", json!({"model_call_index": 1, "item_id": "telemetry", "phase": "final_answer", "text": "TELEMETRY_STREAM_DONE"}));
    fixture.complete(REMOTE_TURN);
    fixture.terminal.wait_text("Enter send").await;
    let read_log = || std::fs::read_to_string(&log).unwrap_or_default();
    tokio::time::timeout(TIMEOUT, async {
        while !read_log().contains("TUI stream timing completed") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("stream timing never logged: {}", read_log()));
    let text = read_log();
    assert!(text.contains("TUI view state changed"), "{text}");
    assert!(text.contains(AGENT), "{text}");
    // The batch exporter flushes on its own schedule; no Jaeger is involved.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            {
                let body = traces.lock().unwrap();
                if byte_count(&body, b"tui.stream") > 0 && byte_count(&body, AGENT.as_bytes()) > 0 {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("OTLP collector never received the tui.stream span");
    renderer_evidence("telemetry.log", text.as_bytes());
    renderer_evidence("telemetry-otlp.bin", &traces.lock().unwrap());
    collector_task.abort();
}
