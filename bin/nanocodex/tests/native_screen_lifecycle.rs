//! Real shipped screen publisher against a loopback broker and isolated X11
//! desktop. No installed account service, user display, or credentials are used.
//! Run on Linux with Xvfb, openbox, xterm and fonts installed:
//! cargo test -p nanocodex-bin --test nanocodex2_managed \
//!   standalone_screen_retries_and_exits_on_replacement -- --ignored --nocapture
use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    routing::{get, post},
};
use base64::Engine;
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};

#[derive(Clone)]
struct Broker {
    connections: Arc<AtomicUsize>,
    attempts: Arc<AtomicUsize>,
    before_publication: bool,
    workspace: PathBuf,
    observed: mpsc::UnboundedSender<()>,
    replace: Arc<tokio::sync::Notify>,
    recovered: Arc<tokio::sync::Notify>,
}
/// Next publisher frame, answering liveness pings. None once the publisher
/// closes this connection: a WebRTC media failure ends the session, and the
/// publisher recovers on a new connection.
async fn next_frame(socket: &mut WebSocket) -> Option<Value> {
    loop {
        let Message::Text(text) = socket.recv().await?.ok()? else {
            continue;
        };
        let frame: Value = serde_json::from_str(&text).unwrap();
        if frame["type"] == "ping" {
            socket
                .send(Message::Text(
                    json!({"type":"pong","nonce":frame["nonce"]})
                        .to_string()
                        .into(),
                ))
                .await
                .ok()?;
            continue;
        }
        return Some(frame);
    }
}
async fn drain(socket: &mut WebSocket) {
    while next_frame(socket).await.is_some() {}
}
async fn agent_call(socket: &mut WebSocket, request_id: String, input: Value) -> Option<Value> {
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 8_000;
    socket.send(Message::Text(json!({"type":"agent_call","request_id":request_id,"agent_id":"synthetic-agent","surface_id":"desktop","generation":"synthetic-generation","deadline_at":deadline,"input":input}).to_string().into())).await.ok()?;
    loop {
        let reply = next_frame(socket).await?;
        if reply["type"] == "agent_result" {
            assert_eq!(reply["status"], "ok", "{reply}");
            return Some(reply);
        }
    }
}
async fn host(State(state): State<Broker>, upgrade: WebSocketUpgrade) -> axum::response::Response {
    upgrade.on_upgrade(move |mut socket| async move {
        state.connections.fetch_add(1, Ordering::SeqCst);
        if socket.send(Message::Text(json!({"type":"ready","connection_id":"synthetic-screen"}).to_string().into())).await.is_err() {
            return;
        }
        // A recovering session may close before cataloging while capture is down.
        let Some(catalog) = next_frame(&mut socket).await else { return };
        assert_eq!(catalog["type"], "catalog");
        assert_eq!(catalog["machine_id"], "synthetic-screen");
        // Native Hands publish H.264 video over WebRTC; JPEG frames-v1 is reserved
        // for restricted sandboxes, so a native catalog names no frame transport.
        assert!(catalog["surfaces"][0].get("transport").is_none(), "{catalog}");
        if !state.before_publication {
            if socket.send(Message::Text(json!({"type":"published","generation":"synthetic-generation"}).to_string().into())).await.is_err() {
                return;
            }
            loop {
                let attempt = state.attempts.load(Ordering::SeqCst);
                if attempt >= 2 {
                    break;
                }
                let Some(reply) = agent_call(&mut socket, format!("synthetic-observe-{attempt}"), json!({"action":"observe"})).await else { return };
                let jpeg = base64::engine::general_purpose::STANDARD
                    .decode(reply["jpeg"].as_str().expect("JPEG base64"))
                    .expect("valid JPEG base64");
                let decoded = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg)
                    .expect("actual decodable JPEG");
                assert_eq!(Some(u64::from(decoded.width())), reply["width"].as_u64());
                assert_eq!(Some(u64::from(decoded.height())), reply["height"].as_u64());
                assert!(decoded.width() > 0 && decoded.height() > 0);
                eprintln!("decoded broker observation {attempt}: JPEG bytes={}, dimensions={}x{}", jpeg.len(), decoded.width(), decoded.height());
                // Verify delivery through the shipped broker input boundary, not
                // merely an acknowledgment: type into only the owned X11 terminal.
                let marker = format!("native-screen-input-{attempt}");
                for (step, input) in [
                    json!({"action":"type", "text":format!("printf '%s' '{marker}' > {marker}.txt")}),
                    json!({"action":"key", "key":40}),
                ].into_iter().enumerate() {
                    if agent_call(&mut socket, format!("synthetic-input-{attempt}-{step}"), input).await.is_none() {
                        return;
                    }
                }
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        if std::fs::read_to_string(state.workspace.join(format!("{marker}.txt"))).ok().as_deref() == Some(&marker) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }).await.expect("broker native type/key must deliver the harmless terminal marker");
                eprintln!("native broker input delivered before/after capture recovery: {marker}");
                state.attempts.store(attempt + 1, Ordering::SeqCst);
                state.observed.send(()).unwrap();
                if attempt == 0 {
                    // Recovery either keeps this session or replaces it with a
                    // new connection, which then runs the next attempt.
                    tokio::select! {
                        () = state.recovered.notified() => {}
                        () = drain(&mut socket) => return,
                    }
                }
            }
            tokio::select! {
                () = state.replace.notified() => {}
                () = drain(&mut socket) => return,
            }
        }
        // Before publication, the host is replaced right after cataloging.
        let _ = socket.send(Message::Close(Some(CloseFrame { code: 1000, reason: "Host replaced".into() }))).await;
    })
}
fn executable(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join(name))
        .find(|path| {
            path.metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
        .unwrap_or_else(|| panic!("install {name} before running this journey"))
}
/// The publisher's owned helper runs `__hand-desktop --workspace W --runtime R`
/// with its IPC in a short private runtime outside the state directory.
fn desktop_runtime(workspace: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    std::fs::read_dir("/proc")
        .ok()?
        .flatten()
        .find_map(|entry| {
            let cmdline = std::fs::read(entry.path().join("cmdline")).ok()?;
            let args: Vec<&[u8]> = cmdline.split(|byte| *byte == 0).collect();
            let value = |flag: &[u8]| {
                let index = args.iter().position(|argument| *argument == flag)?;
                args.get(index + 1).copied()
            };
            (args.contains(&b"__hand-desktop".as_slice())
                && value(b"--workspace")? == workspace.as_os_str().as_bytes())
            .then(|| {
                value(b"--runtime")
                    .map(|runtime| PathBuf::from(std::ffi::OsStr::from_bytes(runtime)))
            })
            .flatten()
        })
}
fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[tokio::test]
#[ignore = "real isolated Linux desktop; requires Xvfb, openbox, xterm and fonts"]
async fn standalone_screen_retries_and_exits_on_replacement() {
    assert_eq!(
        std::env::consts::OS,
        "linux",
        "this opt-in journey requires an isolated Linux desktop; never capture the shared macOS display"
    );
    let xvfb = executable("Xvfb");
    executable("openbox");
    executable("xterm");
    let mut runtimes = Vec::new();
    for before_publication in [false, true] {
        let mut expected_connections = 1;
        let fixture = tempfile::tempdir().unwrap();
        let workspace = fixture.path().join("workspace");
        let bin = fixture.path().join("bin");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&bin).unwrap();
        let attempted = fixture.path().join("attempted");
        let gate = fixture.path().join("available");
        // An unavailable actual X server is a startup failure, not a mocked
        // capture backend. Recovery launches the installed server unmodified.
        let wrapper = bin.join("Xvfb");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nprintf attempt > {}\ntest -e {} || exit 1\nexec {} \"$@\"\n",
                quoted(&attempted),
                quoted(&gate),
                quoted(&xvfb)
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        if before_publication {
            std::fs::write(&gate, "available").unwrap();
        }
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let (observed, mut observations) = mpsc::unbounded_channel();
        let broker = Broker {
            connections: Arc::new(AtomicUsize::new(0)),
            attempts: Arc::new(AtomicUsize::new(0)),
            before_publication,
            workspace: workspace.clone(),
            observed,
            replace: Arc::new(tokio::sync::Notify::new()),
            recovered: Arc::new(tokio::sync::Notify::new()),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/account/hands/host", get(host))
            .route(
                "/v1/account/hands/renew",
                post(|| async { axum::Json(json!({"ok":true})) }),
            )
            .with_state(broker.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let state_dir = fixture.path().join("state");
        let key = format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43));
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex"))
            .env_clear()
            .env("LANG", "C.UTF-8")
            .args(["__hand-screen", "--workspace"])
            .arg(&workspace)
            .args([
                "--machine-id",
                "synthetic-screen",
                "--machine-name",
                "Synthetic desktop",
                "--state-dir",
            ])
            .arg(&state_dir)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("HOME", fixture.path())
            .env("NANOCODEX_HOME", fixture.path())
            .env("NC_API_KEY", key)
            .env_remove("NANOCODEX_API_KEY")
            .env("NANOCODEX_MANAGED_URL", origin)
            .env("NANOCODEX_DISABLE_HAND", "1")
            .env("NANOCODEX_SCREEN_BACKEND", "x11")
            .env_remove("NANOCODEX_PARENT_PIPE")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (lines_tx, mut lines_rx) = mpsc::unbounded_channel();
        let stderr = child.stderr.take().unwrap();
        let transcript = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut output = String::new();
            while let Some(line) = lines.next_line().await.unwrap() {
                eprintln!("[hand-stderr] {line}");
                output.push_str(&line);
                output.push('\n');
                let _ = lines_tx.send(line);
            }
            output
        });
        if !before_publication {
            tokio::time::timeout(Duration::from_secs(10), async {
                while !attempted.exists() {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let line = lines_rx.recv().await.unwrap();
                    assert_ne!(line, "Hand screen is ready");
                    if line.starts_with("Hand screen unavailable:") {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(broker.connections.load(Ordering::SeqCst), 0);
            assert!(
                child.try_wait().unwrap().is_none(),
                "publisher exited instead of retrying startup"
            );
            while let Ok(line) = lines_rx.try_recv() {
                assert_ne!(line, "Hand screen is ready");
            }
            std::fs::write(&gate, "available").unwrap();
            tokio::time::timeout(Duration::from_secs(45), async {
                observations.recv().await.unwrap();
                loop {
                    if lines_rx.recv().await.unwrap() == "Hand screen is ready" {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            let runtime = desktop_runtime(&workspace)
                .filter(|path| path.join("hand.sock").exists())
                .expect("ready publisher owns a private desktop runtime with its IPC socket");
            runtimes.push(runtime.clone());
            // Stop only the synthetic owned helper through its real IPC, then
            // make its next X-server startup fail. The publisher must remain
            // connected while reporting unavailable and later usable again.
            std::fs::remove_file(&gate).unwrap();
            let stream = tokio::net::UnixStream::connect(runtime.join("hand.sock"))
                .await
                .unwrap();
            let mut desktop = BufReader::new(stream);
            desktop
                .get_mut()
                .write_all(b"{\"action\":\"shutdown\"}\n")
                .await
                .unwrap();
            let mut response = String::new();
            desktop.read_line(&mut response).await.unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&response).unwrap()["status"],
                "ok"
            );
            drop(desktop);
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    if lines_rx
                        .recv()
                        .await
                        .unwrap()
                        .starts_with("Hand screen unavailable:")
                    {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            // WebRTC capture loss ends the media session; the publisher stays
            // running and reconnects rather than exiting.
            assert!(
                child.try_wait().unwrap().is_none(),
                "publisher exited on capture loss"
            );
            std::fs::write(&gate, "available").unwrap();
            tokio::time::timeout(Duration::from_secs(45), async {
                loop {
                    if lines_rx.recv().await.unwrap() == "Hand screen is ready" {
                        break;
                    }
                }
                broker.recovered.notify_one();
                observations.recv().await.unwrap();
            })
            .await
            .unwrap();
            expected_connections = broker.connections.load(Ordering::SeqCst);
            // One initial session plus bounded reconnects across the helper
            // crash and recovery; a reconnect storm fails here.
            assert!(
                (2..=3).contains(&expected_connections),
                "unexpected publisher connections: {expected_connections}"
            );
            broker.replace.notify_one();
        }
        let status = tokio::time::timeout(Duration::from_secs(45), child.wait())
            .await
            .unwrap()
            .unwrap();
        let output = transcript.await.unwrap();
        assert!(status.success(), "{output}");
        assert_eq!(
            broker.connections.load(Ordering::SeqCst),
            expected_connections,
            "fenced publisher reclaimed its replacement"
        );
        if before_publication {
            assert!(
                !output.lines().any(|line| line == "Hand screen is ready"),
                "{output}"
            );
        }
        assert!(
            desktop_runtime(&workspace).is_none() && runtimes.iter().all(|path| !path.exists()),
            "owned desktop helper or runtime leaked: {runtimes:?}"
        );
        eprintln!(
            "__hand-screen journey: replacement_before_publication={before_publication}, connections={expected_connections}, exit={status}, socket_removed=true\n{output}"
        );
        server.abort();
    }
}

#[tokio::test]
#[ignore = "isolated Linux infrastructure cancellation; requires Xvfb"]
async fn standalone_screen_sigterm_during_startup_reaps_desktop() {
    assert_eq!(
        std::env::consts::OS,
        "linux",
        "requires isolated Linux infrastructure"
    );
    let xvfb = executable("Xvfb");
    let fixture = tempfile::tempdir().unwrap();
    let workspace = fixture.path().join("workspace");
    let bin = fixture.path().join("bin");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&bin).unwrap();
    let pid_file = fixture.path().join("x-server-bootstrap.pid");
    let wrapper = bin.join("Xvfb");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > {}\n/bin/sleep 60\nexec {} \"$@\"\n",
            quoted(&pid_file),
            quoted(&xvfb)
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state_dir = fixture.path().join("state");
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex"))
        .env_clear()
        .env("LANG", "C.UTF-8")
        .args(["__hand-screen", "--workspace"])
        .arg(&workspace)
        .args([
            "--machine-id",
            "synthetic-cancel",
            "--machine-name",
            "Synthetic cancel",
            "--state-dir",
        ])
        .arg(&state_dir)
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("HOME", fixture.path())
        .env("NANOCODEX_HOME", fixture.path())
        .env(
            "NC_API_KEY",
            format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)),
        )
        .env(
            "NANOCODEX_MANAGED_URL",
            format!("http://{}", listener.local_addr().unwrap()),
        )
        .env("NANOCODEX_DISABLE_HAND", "1")
        .env("NANOCODEX_SCREEN_BACKEND", "x11")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let descendants = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(value) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = value.parse::<u32>()
                && let Ok(children) =
                    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
            {
                let mut pids = children
                    .split_whitespace()
                    .map(|child| child.parse::<u32>().unwrap())
                    .collect::<Vec<_>>();
                if !pids.is_empty() {
                    pids.push(pid);
                    break pids;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id().unwrap() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let transcript = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{transcript}");
    assert!(
        !transcript
            .lines()
            .any(|line| line == "Hand screen is ready"),
        "{transcript}"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while descendants
            .iter()
            .any(|pid| Path::new(&format!("/proc/{pid}")).exists())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cancelled screen helper leaked its bootstrap descendants");
    assert!(
        std::fs::read_dir(&state_dir).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("desktop-")),
        "cancelled screen leaked its owned runtime"
    );
    eprintln!(
        "__hand-screen SIGTERM during startup: exit={}, owned infrastructure pids={descendants:?} reaped, runtime removed\n{transcript}",
        output.status
    );
}
