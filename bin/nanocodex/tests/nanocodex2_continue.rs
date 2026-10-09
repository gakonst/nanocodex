//! Black-box continuation journeys. All account traffic terminates at a local
//! synthetic service; tmux itself and every attached nanocodex2 are real.
#![cfg(unix)]

use std::{
    collections::{BTreeMap, BTreeSet},
    process::Output,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use serde_json::{Value, json};
use tokio::process::Command;
use unicode_segmentation::UnicodeSegmentation;

const TIMEOUT: Duration = Duration::from_secs(30);
const KEY: &str = "ncx_live_aaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const A: &str = "019fc927-b280-79a7-8445-1b9996ad2fb0";
const B: &str = "019fc927-b281-79a7-8445-1b9996ad2fb0";
const C: &str = "019fc927-b282-79a7-8445-1b9996ad2fb0";
const D: &str = "019fc927-b283-79a7-8445-1b9996ad2fb0";
const E: &str = "019fc927-b284-79a7-8445-1b9996ad2fb0";
const F: &str = "019fc927-b285-79a7-8445-1b9996ad2fb0";

#[derive(Clone)]
struct ServiceState {
    list: Arc<Mutex<Value>>,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    messages: Arc<Mutex<Vec<Value>>>,
    done_writes: Arc<Mutex<Vec<(String, bool)>>>,
    fail: Arc<Mutex<bool>>,
}

// Cargo's shared target can be rebuilt by another worktree while a parent
// continue process is running. Copy the actual executable once per overlapping
// fixture group so current_exe() always launches the same real binary inode.
struct StableExecutable {
    _directory: tempfile::TempDir,
    path: std::path::PathBuf,
}
fn stable_executable() -> Arc<StableExecutable> {
    static SHARED: OnceLock<Mutex<Weak<StableExecutable>>> = OnceLock::new();
    let mut shared = SHARED
        .get_or_init(|| Mutex::new(Weak::new()))
        .lock()
        .unwrap();
    if let Some(executable) = shared.upgrade() {
        return executable;
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("nanocodex2");
    std::fs::copy(env!("CARGO_BIN_EXE_nanocodex"), &path).unwrap();
    let executable = Arc::new(StableExecutable {
        _directory: directory,
        path,
    });
    *shared = Arc::downgrade(&executable);
    executable
}

struct Fixture {
    executable: Arc<StableExecutable>,
    state: ServiceState,
    origin: String,
    workspace: tempfile::TempDir,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new(list: Value) -> Self {
        let state = ServiceState {
            list: Arc::new(Mutex::new(list)),
            requests: Arc::new(Mutex::new(Vec::new())),
            messages: Arc::new(Mutex::new(Vec::new())),
            done_writes: Arc::new(Mutex::new(Vec::new())),
            fail: Arc::new(Mutex::new(false)),
        };
        let app = Router::new()
            .route("/v1/agents", get(agent_list))
            .route("/v1/agents/{id}/done", put(set_done))
            .route("/v1/agents/{id}/tool-host", get(tool_host))
            .route("/v1/agents/{id}/ws", get(live))
            .fallback(other_request)
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            executable: stable_executable(),
            state,
            origin,
            workspace: tempfile::tempdir().unwrap(),
            server,
        }
    }

    fn command(&self) -> Command {
        self.configure_command(Command::new(&self.executable.path))
    }

    fn configure_command(&self, mut command: Command) -> Command {
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env(
                "LC_ALL",
                if cfg!(target_os = "macos") {
                    "en_US.UTF-8"
                } else {
                    "C.UTF-8"
                },
            )
            .current_dir(self.workspace.path())
            .env("HOME", self.workspace.path())
            .env("CODEX_HOME", self.workspace.path().join(".codex"))
            .env("NANOCODEX_DISABLE_HAND", "1")
            .env("NANOCODEX_COMPUTER", "off")
            .env("NANOCODEX_MANAGED_URL", &self.origin)
            .env("NANOCODEX_API_KEY", KEY)
            .env(
                "NANOCODEX_RELOAD_DIR",
                self.workspace.path().join(".reload"),
            )
            .env("TERM", "xterm-256color")
            .env("SSH_TTY", "/dev/nanocodex-continue-test")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("NANOCODEX_MANAGED2_URL")
            .env_remove("NANOCODEX2_RELOAD_EXECUTABLE")
            .kill_on_drop(true);
        command
    }

    async fn cli(&self, args: &[&str]) -> Output {
        bounded_output(self.command().args(args)).await
    }

    async fn candidates(&self, extra: &[&str]) -> Vec<Value> {
        let mut args = vec!["continue", "--dry-run"];
        args.extend_from_slice(extra);
        let output = self.cli(&args).await;
        success(&output);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "dry-run was not a JSON array: {error}: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn assert_read_only(&self) {
        let requests = self.state.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .all(|(method, path)| allowed_attach_request(method, path)),
            "unexpected write: {requests:?}"
        );
        assert!(
            self.state.messages.lock().unwrap().iter().all(|message| {
                !matches!(
                    message["type"].as_str(),
                    Some("prompt" | "cancel" | "restart")
                )
            }),
            "continuation submitted or cancelled a turn"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn record(state: &ServiceState, headers: &HeaderMap, method: &str, path: &str) {
    assert_eq!(
        headers.get("authorization").unwrap().to_str().unwrap(),
        format!("Bearer {KEY}")
    );
    state
        .requests
        .lock()
        .unwrap()
        .push((method.into(), path.into()));
}

async fn agent_list(State(state): State<ServiceState>, headers: HeaderMap) -> Response {
    record(&state, &headers, "GET", "/v1/agents");
    if *state.fail.lock().unwrap() {
        return (StatusCode::UNAUTHORIZED, "synthetic account denied").into_response();
    }
    Json(state.list.lock().unwrap().clone()).into_response()
}

async fn set_done(
    State(state): State<ServiceState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    record(&state, &headers, "PUT", &format!("/v1/agents/{id}/done"));
    assert_eq!(body.as_object().unwrap().len(), 1);
    let done = body["done"].as_bool().unwrap();
    state.done_writes.lock().unwrap().push((id.clone(), done));
    state.list.lock().unwrap()["summaries"][&id]["presentation"]["done"] = json!(done);
    Json(json!({"done": done, "done_at": if done { Some(now()) } else { None }})).into_response()
}

fn settings() -> Value {
    json!({"model":"gpt-6-astra", "thinking":"low", "reasoning_mode":"standard", "fast_mode":false})
}
fn capabilities() -> Value {
    json!({"durable_turns":true,"resumable_events":true,"workspace":"private-hosted-tools-v1","execution_environments":true,"execution_namespace":"cwd-root-v1","native_cross_mounts":false})
}

async fn other_request(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
) -> Response {
    record(&state, &headers, method.as_str(), uri.path());
    if method == Method::POST && uri.path().ends_with("/prepare") {
        return Json(json!({"prepared":true})).into_response();
    }
    if method != Method::GET {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            "no turns or destructive actions allowed",
        )
            .into_response();
    }
    if uri.path().ends_with("/events/history") {
        return Json(json!({"data":[],"has_more":false,"latest_cursor":"0"})).into_response();
    }
    if let Some(id) = uri
        .path()
        .strip_prefix("/v1/agents/")
        .filter(|id| !id.contains('/'))
    {
        return Json(json!({"agent_id":id,"session_id":id,"has_snapshot":false,"completed_turns":0,"last_active":1,"active_turns":[],"agent_loaded":false,"connected_clients":0,"capabilities":capabilities(),"settings":settings(),"latest_event_cursor":"0","stream_error":null})).into_response();
    }
    (StatusCode::NOT_FOUND, "synthetic endpoint not found").into_response()
}

async fn live(
    State(state): State<ServiceState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    record(&state, &headers, "GET", &format!("/v1/agents/{id}/ws"));
    upgrade.on_upgrade(move |mut socket| async move {
        let ready = json!({"type":"ready","session_id":id,"restored":false,"active_turns":[],"capabilities":capabilities(),"settings":settings(),"latest_event_cursor":"0"});
        if socket.send(Message::Text(ready.to_string().into())).await.is_ok() { monitor(socket, state).await; }
    }).into_response()
}

async fn tool_host(
    State(state): State<ServiceState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    record(
        &state,
        &headers,
        "GET",
        &format!("/v1/agents/{id}/tool-host"),
    );
    upgrade
        .on_upgrade(move |mut socket| async move {
            if let Some(Ok(Message::Text(catalog))) = socket.recv().await {
                let catalog: Value = serde_json::from_str(&catalog).unwrap();
                assert_eq!(catalog["type"], "catalog");
                state.messages.lock().unwrap().push(catalog);
                if socket
                    .send(Message::Text(json!({"type":"ready"}).to_string().into()))
                    .await
                    .is_ok()
                {
                    monitor(socket, state).await;
                }
            }
        })
        .into_response()
}

async fn monitor(mut socket: WebSocket, state: ServiceState) {
    while let Some(Ok(message)) = socket.recv().await {
        match message {
            Message::Text(text) => {
                if let Ok(value) = serde_json::from_str(&text) {
                    state.messages.lock().unwrap().push(value);
                }
            }
            Message::Ping(bytes) => {
                if socket.send(Message::Pong(bytes)).await.is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

// Existing attach warms its retained conversation, but must not create a
// conversation or submit/cancel/restart/delete a turn.
fn allowed_attach_request(method: &str, path: &str) -> bool {
    method == "GET" && path != "/v1/agents/live"
        || method == "POST" && path.starts_with("/v1/agents/") && path.ends_with("/prepare")
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as f64
}
fn summary(title: &str, updated: f64, presentation: Value) -> Value {
    json!({"title":title,"created_at":1,"updated_at":updated,"turn_count":1,"presentation":presentation})
}
fn presentation(status: &str, touched: f64) -> Value {
    json!({"revision":1,"status":status,"activeTurnIds":[],"lastUserMessageAt":touched,"lastUserPrompt":"fallback prompt","done":false})
}
fn ids(candidates: &[Value]) -> BTreeSet<&str> {
    candidates
        .iter()
        .map(|value| value["agent_id"].as_str().unwrap())
        .collect()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "status={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
async fn bounded_output(command: &mut Command) -> Output {
    command.kill_on_drop(true);
    tokio::time::timeout(TIMEOUT, command.output())
        .await
        .expect("child command timed out")
        .unwrap()
}

#[tokio::test]
async fn dry_run_selects_user_touches_not_background_output_and_running_regardless_of_age() {
    let now = now();
    let old = now - 48.0 * 3600.0 * 1000.0;
    let mut summaries = BTreeMap::new();
    summaries.insert(A, summary("old running", old, presentation("running", old)));
    summaries.insert(
        B,
        summary(
            "recent user",
            old,
            presentation("completed", now - 5400.0 * 1000.0),
        ),
    );
    summaries.insert(
        C,
        summary("background only", now, presentation("completed", old)),
    );
    summaries.insert(
        D,
        summary("legacy recent", now - 5400.0 * 1000.0, Value::Null),
    );
    let mut done = presentation("running", now);
    done["done"] = json!(true);
    summaries.insert(E, summary("done running", now, done));
    // A summary outside AgentList.data must never grant attachment authority.
    summaries.insert(
        F,
        summary("orphan summary", now, presentation("running", now)),
    );
    let fixture = Fixture::new(json!({"data":[A,B,C,D,E,A],"summaries":summaries})).await;
    let candidates = fixture.candidates(&[]).await;
    assert_eq!(ids(&candidates), BTreeSet::from([A, B, D]));
    assert_eq!(candidates.len(), 3, "duplicate ID was not deduplicated");
    for candidate in &candidates {
        for field in [
            "agent_id",
            "title",
            "window_name",
            "status",
            "last_user_message_at",
        ] {
            assert!(
                candidate.get(field).is_some(),
                "missing {field}: {candidate}"
            );
        }
    }
    assert_eq!(
        candidates
            .iter()
            .find(|item| item["agent_id"] == B)
            .unwrap()["last_user_message_at"],
        now - 5400.0 * 1000.0
    );
    assert_eq!(
        ids(&fixture.candidates(&["--hours", "1"]).await),
        BTreeSet::from([A])
    );
    assert_eq!(
        ids(&fixture.candidates(&["--hours", "72"]).await),
        BTreeSet::from([A, B, C, D])
    );
    fixture.assert_read_only();
}

#[tokio::test]
async fn dry_run_names_are_terminal_safe_graphemes_with_prompt_and_id_fallbacks() {
    let now = now();
    let title = "  é👩🏽‍💻\t alpha\n beta gamma delta  ";
    let prompt = "  fallback\t title\nfrom prompt ";
    let mut fallback = presentation("running", now);
    fallback["lastUserPrompt"] = json!(prompt);
    let mut no_prompt = presentation("running", now);
    no_prompt["lastUserPrompt"] = json!("");
    let fixture = Fixture::new(json!({"data":[A,B,C],"summaries":{
        A:summary(title,now,presentation("running",now)),
        B:summary("",now,fallback), C:summary("",now,no_prompt)
    }}))
    .await;
    let candidates = fixture.candidates(&[]).await;
    // Preview must work without a tmux executable anywhere on PATH.
    let preview = bounded_output(
        fixture
            .command()
            .args(["continue", "--dry-run"])
            .env("PATH", fixture.workspace.path()),
    )
    .await;
    success(&preview);
    assert_eq!(
        serde_json::from_slice::<Vec<Value>>(&preview.stdout).unwrap(),
        candidates
    );
    for (id, expected) in [
        (A, "é👩🏽‍💻 alpha beta gamma delta"),
        (B, "fallback title from prompt"),
        (C, C),
    ] {
        let candidate = candidates
            .iter()
            .find(|item| item["agent_id"] == id)
            .unwrap();
        let name = candidate["window_name"].as_str().unwrap();
        assert_eq!(name, expected.graphemes(true).take(15).collect::<String>());
        assert!(!name.chars().any(char::is_control));
    }
    fixture.assert_read_only();
}

#[tokio::test]
async fn done_and_undone_are_reversible_without_cancelling_the_agent() {
    let now = now();
    let fixture = Fixture::new(
        json!({"data":[A],"summaries":{A:summary("running",now,presentation("running",now))}}),
    )
    .await;
    assert_eq!(ids(&fixture.candidates(&[]).await), BTreeSet::from([A]));
    success(&fixture.cli(&["done", A]).await);
    assert!(fixture.candidates(&[]).await.is_empty());
    success(&fixture.cli(&["undone", A]).await);
    assert_eq!(ids(&fixture.candidates(&[]).await), BTreeSet::from([A]));
    assert_eq!(
        *fixture.state.done_writes.lock().unwrap(),
        [(A.to_owned(), true), (A.to_owned(), false)]
    );
    assert!(
        fixture
            .state
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(method, path)| method == "GET"
                || method == "PUT" && path == &format!("/v1/agents/{A}/done"))
    );
}

/// Each test owns a UUID-named socket. Never call unqualified `tmux kill-server`.
struct Tmux {
    socket: String,
}
impl Tmux {
    async fn new() -> Self {
        let output = bounded_output(Command::new("tmux").arg("-V")).await;
        success(&output); // Missing tmux is a test setup failure, not a fake/skip.
        Self {
            socket: format!("ncx-continue-test-{}", uuid::Uuid::new_v4()),
        }
    }
    async fn output(&self, args: &[&str]) -> Output {
        bounded_output(
            Command::new("tmux")
                .args(["-L", &self.socket, "-f", "/dev/null"])
                .args(args),
        )
        .await
    }
    async fn text(&self, args: &[&str]) -> String {
        let output = self.output(args).await;
        success(&output);
        String::from_utf8(output.stdout)
            .unwrap()
            .trim_end()
            .to_owned()
    }
    async fn bootstrap(&self, fixture: &Fixture, session: &str) {
        // Existing servers retain their startup environment. Bootstrap with the
        // synthetic account, never the developer's account or tmux config.
        let output = bounded_output(fixture.configure_command(Command::new("tmux")).args([
            "-L",
            &self.socket,
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            session,
            "-n",
            "sentinel",
            "sleep 120",
        ]))
        .await;
        success(&output);
    }

    async fn panes(&self, session: &str) -> Vec<Pane> {
        let text = self.text(&["list-panes","-s","-t",session,"-F","#{window_id}\t#{window_name}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{pane_current_path}\t#{pane_start_command}"]).await;
        text.lines()
            .map(|line| {
                let fields: Vec<_> = line.splitn(7, '\t').collect();
                assert_eq!(fields.len(), 7, "bad pane record: {line}");
                Pane {
                    window: fields[0].into(),
                    name: fields[1].into(),
                    id: fields[2].into(),
                    pid: fields[3].into(),
                    dead: fields[4] == "1",
                    cwd: fields[5].into(),
                    command: fields[6].into(),
                }
            })
            .collect()
    }
    async fn continue_detached(&self, fixture: &Fixture, session: Option<&str>) -> Output {
        let mut args = vec!["continue", "--detach", "--tmux-socket", &self.socket];
        if let Some(session) = session {
            args.extend(["--session", session]);
        }
        fixture.cli(&args).await
    }
}
impl Drop for Tmux {
    fn drop(&mut self) {
        // Timeout limits also apply to cleanup; only the test's own socket is targeted.
        if let Ok(mut child) = std::process::Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while matches!(child.try_wait(), Ok(None)) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
#[derive(Debug)]
struct Pane {
    window: String,
    name: String,
    id: String,
    pid: String,
    dead: bool,
    cwd: String,
    command: String,
}

async fn wait_for_attach(fixture: &Fixture, agent: &str, minimum: usize) {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let count = fixture
                .state
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, path)| path == &format!("/v1/agents/{agent}/ws"))
                .count();
            if count >= minimum {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "real attach for {agent} never connected: {:?}",
            fixture.state.requests.lock().unwrap()
        )
    });
}

#[tokio::test]
async fn real_tmux_reuses_live_panes_and_respawns_dead_panes_in_the_same_cwd() {
    let now = now();
    let fixture = Fixture::new(json!({"data":[A,B,A],"summaries":{
        A:summary("same title collision",now,presentation("running",now)),
        B:summary("same title collision",now,presentation("completed",now))
    }}))
    .await;
    let tmux = Tmux::new().await;
    success(&tmux.continue_detached(&fixture, Some("journey")).await);
    wait_for_attach(&fixture, A, 1).await;
    wait_for_attach(&fixture, B, 1).await;
    let first = tmux.panes("journey").await;
    assert_eq!(
        first.len(),
        2,
        "title collisions or duplicate IDs made extra/missing panes: {first:?}"
    );
    assert_eq!(first[0].name, first[1].name);
    for pane in &first {
        assert!(!pane.dead, "attached pane exited: {pane:?}");
        assert_eq!(
            std::path::Path::new(&pane.cwd).canonicalize().unwrap(),
            fixture.workspace.path().canonicalize().unwrap()
        );
        assert!(
            pane.command.contains("nanocodex2") && pane.command.contains("attach"),
            "child is not the real executable attach: {pane:?}"
        );
    }
    success(&tmux.continue_detached(&fixture, Some("journey")).await);
    let second = tmux.panes("journey").await;
    assert_eq!(
        first.iter().map(|p| (&p.id, &p.pid)).collect::<Vec<_>>(),
        second.iter().map(|p| (&p.id, &p.pid)).collect::<Vec<_>>(),
        "live panes were replaced"
    );
    // Keep a genuinely dead pane in the actual server, then ask continue to recover it.
    let victim = first.iter().find(|pane| pane.command.contains(A)).unwrap();
    tmux.text(&[
        "set-option",
        "-w",
        "-t",
        &victim.window,
        "remain-on-exit",
        "on",
    ])
    .await;
    let output = bounded_output(Command::new("kill").args(["-KILL", &victim.pid])).await;
    success(&output);
    tokio::time::timeout(TIMEOUT, async {
        loop {
            if tmux
                .panes("journey")
                .await
                .iter()
                .any(|pane| pane.id == victim.id && pane.dead)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("pane did not become dead");
    success(&tmux.continue_detached(&fixture, Some("journey")).await);
    wait_for_attach(&fixture, A, 2).await;
    let recovered = tmux.panes("journey").await;
    assert_eq!(recovered.len(), 2);
    let respawned = recovered
        .iter()
        .find(|pane| pane.id == victim.id)
        .expect("dead pane should be respawned, not duplicated");
    assert!(!respawned.dead);
    assert_ne!(respawned.pid, victim.pid);
    fixture.assert_read_only();
}

#[tokio::test]
async fn real_tmux_empty_and_failed_selection_have_no_creation_side_effects() {
    let fixture = Fixture::new(json!({"data":[],"summaries":{}})).await;
    let tmux = Tmux::new().await;
    success(&tmux.continue_detached(&fixture, None).await);
    assert!(
        !tmux.output(&["list-sessions"]).await.status.success(),
        "empty selection started a server/session"
    );
    *fixture.state.fail.lock().unwrap() = true;
    assert!(
        !tmux
            .continue_detached(&fixture, None)
            .await
            .status
            .success()
    );
    assert!(
        !tmux.output(&["list-sessions"]).await.status.success(),
        "API error started a server/session"
    );
    // Also preserve an existing session and unrelated live window.
    tmux.text(&[
        "new-session",
        "-d",
        "-s",
        "existing",
        "-n",
        "sentinel",
        "sleep 120",
    ])
    .await;
    let before = tmux.panes("existing").await;
    assert!(
        !tmux
            .continue_detached(&fixture, Some("existing"))
            .await
            .status
            .success()
    );
    *fixture.state.fail.lock().unwrap() = false;
    success(&tmux.continue_detached(&fixture, Some("existing")).await);
    let after = tmux.panes("existing").await;
    assert_eq!(
        before.iter().map(|p| (&p.id, &p.pid)).collect::<Vec<_>>(),
        after.iter().map(|p| (&p.id, &p.pid)).collect::<Vec<_>>()
    );
    fixture.assert_read_only();
}

#[tokio::test]
async fn real_tmux_rejects_id_injection_and_never_evaluates_title_shell_syntax() {
    let now = now();
    let unsafe_id = "x;touch injected-id";
    let title = "$(touch injected-title);'\n\u{1b}[31m";
    let fixture = Fixture::new(
        json!({"data":[unsafe_id,A,"../escape","-option","bad id","a\nb"],"summaries":{
            unsafe_id:summary("injection",now,presentation("running",now)),
            A:summary(title,now,presentation("running",now))
        }}),
    )
    .await;
    let candidates = fixture.candidates(&[]).await;
    assert_eq!(ids(&candidates), BTreeSet::from([A]));
    let tmux = Tmux::new().await;
    success(&tmux.continue_detached(&fixture, None).await);
    wait_for_attach(&fixture, A, 1).await;
    let panes = tmux.panes("nanocodex").await;
    assert_eq!(panes.len(), 1);
    assert!(!panes[0].name.chars().any(char::is_control));
    assert!(!fixture.workspace.path().join("injected-title").exists());
    assert!(!fixture.workspace.path().join("injected-id").exists());
    assert!(
        fixture
            .state
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(_, path)| !path.contains("escape") && !path.contains("touch"))
    );
    fixture.assert_read_only();
}

#[tokio::test]
async fn real_tmux_current_session_and_explicit_target_preserve_unrelated_windows_and_done_state() {
    let now = now();
    let mut done = presentation("running", now);
    done["done"] = json!(true);
    let fixture = Fixture::new(json!({"data":[A,B],"summaries":{
        A:summary("working",now,presentation("running",now)),
        B:summary("done session",now,done)
    }}))
    .await;
    let tmux = Tmux::new().await;
    tmux.bootstrap(&fixture, "current").await;
    let original = tmux.panes("current").await;
    let inside = tmux
        .text(&[
            "display-message",
            "-p",
            "-t",
            "current",
            "#{socket_path},#{pid},0",
        ])
        .await;
    let output = bounded_output(
        fixture
            .command()
            .args(["continue", "--detach"])
            .env("TMUX", &inside)
            .env("TMUX_PANE", &original[0].id),
    )
    .await;
    success(&output);
    wait_for_attach(&fixture, A, 1).await;
    let current = tmux.panes("current").await;
    assert_eq!(
        current.len(),
        2,
        "default inside tmux did not target current session"
    );
    assert!(
        current
            .iter()
            .any(|pane| pane.id == original[0].id && pane.pid == original[0].pid)
    );
    assert!(
        current.iter().all(|pane| !pane.command.contains(B)),
        "manually done running session was reopened"
    );
    assert!(
        !tmux
            .output(&["has-session", "-t", "=nanocodex"])
            .await
            .status
            .success()
    );
    // Marking Done must not stop or delete the existing terminal or running agent.
    success(&fixture.cli(&["done", A]).await);
    success(&tmux.continue_detached(&fixture, Some("current")).await);
    let after_done = tmux.panes("current").await;
    assert_eq!(
        current.iter().map(|p| (&p.id, &p.pid)).collect::<Vec<_>>(),
        after_done
            .iter()
            .map(|p| (&p.id, &p.pid))
            .collect::<Vec<_>>()
    );
    success(&fixture.cli(&["undone", B]).await);
    let output = bounded_output(
        fixture
            .command()
            .args(["continue", "--detach", "--session", "chosen"])
            .env("TMUX", &inside)
            .env("TMUX_PANE", &original[0].id),
    )
    .await;
    success(&output);
    wait_for_attach(&fixture, B, 1).await;
    let chosen = tmux.panes("chosen").await;
    assert_eq!(
        chosen.len(),
        1,
        "explicit --session ignored current tmux context"
    );
    assert!(chosen[0].command.contains(B));
    assert!(
        tmux.panes("current")
            .await
            .iter()
            .any(|pane| pane.id == original[0].id && pane.pid == original[0].pid)
    );
    assert!(
        fixture
            .state
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(method, path)| allowed_attach_request(method, path)
                || method == "PUT" && path.ends_with("/done"))
    );
    assert!(
        fixture
            .state
            .messages
            .lock()
            .unwrap()
            .iter()
            .all(|message| !matches!(
                message["type"].as_str(),
                Some("prompt" | "cancel" | "restart")
            ))
    );
}

#[tokio::test]
async fn explicit_zero_user_touch_never_falls_back_to_background_and_legacy_seconds_are_supported()
{
    let now = now();
    let fixture = Fixture::new(json!({"data":[A,B,C],"summaries":{
        A:summary("no user message",now,presentation("idle",0.0)),
        B:summary("legacy seconds",((now - 3600000.0) / 1000.0).floor(),Value::Null),
        C:summary("old user, new output",now,presentation("completed",now-86400000.0))
    }}))
    .await;
    let candidates = fixture.candidates(&[]).await;
    assert_eq!(ids(&candidates), BTreeSet::from([B]));
    assert_eq!(
        candidates[0]["last_user_message_at"],
        ((now - 3600000.0) / 1000.0).floor() * 1000.0
    );
    fixture.assert_read_only();
}

#[tokio::test]
async fn real_tmux_default_interactive_launch_enters_and_can_detach_from_restored_session() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::{Read, Write};
    let now = now();
    let fixture = Fixture::new(
        json!({"data":[A],"summaries":{A:summary("interactive",now,presentation("running",now))}}),
    )
    .await;
    let tmux = Tmux::new().await;
    tmux.bootstrap(&fixture, "setup").await;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 30,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(&fixture.executable.path);
    command.env_clear();
    command.env("PATH", std::env::var_os("PATH").unwrap_or_default());
    command.env(
        "LC_ALL",
        if cfg!(target_os = "macos") {
            "en_US.UTF-8"
        } else {
            "C.UTF-8"
        },
    );
    command.args(["continue", "--tmux-socket", &tmux.socket]);
    command.cwd(fixture.workspace.path());
    command.env("HOME", fixture.workspace.path());
    command.env("CODEX_HOME", fixture.workspace.path().join(".codex"));
    command.env("NANOCODEX_DISABLE_HAND", "1");
    command.env("NANOCODEX_COMPUTER", "off");
    command.env("NANOCODEX_MANAGED_URL", &fixture.origin);
    command.env("NANOCODEX_API_KEY", KEY);
    command.env(
        "NANOCODEX_RELOAD_DIR",
        fixture.workspace.path().join(".reload"),
    );
    command.env("TERM", "xterm-256color");
    command.env("SSH_TTY", "/dev/nanocodex-continue-test");
    for key in [
        "TMUX",
        "TMUX_PANE",
        "TERM_PROGRAM",
        "NANOCODEX_MANAGED2_URL",
        "NANOCODEX2_RELOAD_EXECUTABLE",
    ] {
        command.env_remove(key);
    }
    struct ChildGuard(Box<dyn portable_pty::Child + Send + Sync>);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
        }
    }
    let mut child = ChildGuard(pair.slave.spawn_command(command).unwrap());
    drop(pair.slave);
    let mut writer = pair.master.take_writer().unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let copy = captured.clone();
    std::thread::spawn(move || {
        let mut bytes = [0; 8192];
        while let Ok(count) = reader.read(&mut bytes) {
            if count == 0 {
                break;
            }
            copy.lock().unwrap().extend_from_slice(&bytes[..count]);
        }
    });
    wait_for_attach(&fixture, A, 1).await;
    // Verify a real tmux client, not merely a created detached session.
    tokio::time::timeout(TIMEOUT, async {
        loop {
            if tmux
                .text(&[
                    "display-message",
                    "-p",
                    "-t",
                    "nanocodex",
                    "#{session_attached}",
                ])
                .await
                == "1"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("default continue never attached its interactive tmux client");
    tmux.text(&["set-option", "-g", "prefix", "C-b"]).await;
    tmux.text(&["bind-key", "d", "detach-client"]).await;
    writer.write_all(b"\x02d").unwrap(); // tmux's default detach binding
    writer.flush().unwrap();
    let status = tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "interactive continue did not detach: {}",
            String::from_utf8_lossy(
                &captured
                    .lock()
                    .unwrap()
                    .iter()
                    .copied()
                    .rev()
                    .take(2000)
                    .collect::<Vec<_>>()
            )
        )
    });
    assert!(status.success());
    let panes = tmux.panes("nanocodex").await;
    assert_eq!(panes.len(), 1);
    assert!(!panes[0].dead, "detaching tmux killed the continued agent");
    fixture.assert_read_only();
}

#[tokio::test]
async fn real_tmux_fresh_server_never_inherits_caller_credentials() {
    let now = now();
    let fixture = Fixture::new(json!({"data":[A],"summaries":{A:summary("private handoff",now,presentation("running",now))}})).await;
    let tmux = Tmux::new().await;
    let output = bounded_output(
        fixture
            .command()
            .args(["continue", "--detach", "--tmux-socket", &tmux.socket])
            .env("NC_API_KEY", "synthetic-unused-alias"),
    )
    .await;
    success(&output);
    wait_for_attach(&fixture, A, 1).await;
    let panes = tmux.panes("nanocodex").await;
    assert_eq!(panes.len(), 1);
    assert!(!panes[0].dead);
    for key in ["NANOCODEX_API_KEY", "NC_API_KEY"] {
        let receipt = tmux.output(&["show-environment", "-g", key]).await;
        assert!(
            !receipt.status.success(),
            "fresh tmux server retained {key}"
        );
    }
    assert!(!panes[0].command.contains(KEY));
    assert!(!panes[0].command.contains("synthetic-unused-alias"));
    fixture.assert_read_only();
}

#[tokio::test]
async fn real_tmux_existing_server_keeps_stale_credentials_and_custom_configuration_private() {
    let now = now();
    let fixture = Fixture::new(json!({"data":[A],"summaries":{A:summary("fresh account",now,presentation("running",now))}})).await;
    let tmux = Tmux::new().await;
    tmux.bootstrap(&fixture, "existing").await;
    let stale_key = "ncx_live_stale00000000_stale000000000000000000000000000000000000000";
    // This is synthetic test data in our UUID-owned server, not user secrets.
    tmux.text(&["set-environment", "-g", "NANOCODEX_API_KEY", stale_key])
        .await;
    let custom_config = fixture.workspace.path().join("custom-tmux.conf");
    std::fs::write(
        &custom_config,
        "set-option -g prefix C-a\nset-option -g @continue-test-user-config preserve-me\nset-option -g remain-on-exit on\n",
    )
    .unwrap();
    tmux.text(&["source-file", custom_config.to_str().unwrap()])
        .await;
    let before = tmux.panes("existing").await;
    success(&tmux.continue_detached(&fixture, Some("existing")).await);
    wait_for_attach(&fixture, A, 1).await;
    let after = tmux.panes("existing").await;
    assert_eq!(after.len(), 2);
    assert!(
        after
            .iter()
            .any(|pane| pane.id == before[0].id && pane.pid == before[0].pid)
    );
    assert_eq!(tmux.text(&["show-options", "-gv", "prefix"]).await, "C-a");
    assert_eq!(
        tmux.text(&["show-options", "-gv", "@continue-test-user-config"])
            .await,
        "preserve-me"
    );
    assert_eq!(
        tmux.text(&["show-environment", "-g", "NANOCODEX_API_KEY"])
            .await,
        format!("NANOCODEX_API_KEY={stale_key}")
    );
    for pane in &after {
        assert!(
            !pane.command.contains(KEY),
            "fresh credential leaked into pane command"
        );
        assert!(
            !pane.command.contains(stale_key),
            "stale credential leaked into pane command"
        );
    }
    // Never put the caller's credential in globally visible tmux options/env.
    let options = tmux.text(&["show-options", "-g"]).await;
    let environment = tmux.text(&["show-environment", "-g"]).await;
    assert!(!options.contains(KEY) && !environment.contains(KEY));
    fixture.assert_read_only();
}

async fn wait_for_ready_pane(tmux: &Tmux, pane: &str) {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let text = tmux
                .text(&[
                    "display-message",
                    "-p",
                    "-t",
                    pane,
                    "#{@nanocodex-overview}",
                ])
                .await;
            if serde_json::from_str::<Value>(&text)
                .ok()
                .is_some_and(|value| value["agent_id"] == A && value["status"] == "idle")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("attached TUI never published ready metadata");
}

async fn wait_for_screen(tmux: &Tmux, pane: &str, expected: &str) {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            if tmux
                .text(&["capture-pane", "-p", "-t", pane])
                .await
                .contains(expected)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("TUI never acknowledged {expected:?}"));
}

#[tokio::test]
async fn real_attached_tui_done_and_undone_acknowledge_without_submitting_or_stopping_work() {
    let now = now();
    let fixture = Fixture::new(json!({"data":[A],"summaries":{A:summary("slash command",now,presentation("running",now))}})).await;
    let tmux = Tmux::new().await;
    tmux.bootstrap(&fixture, "setup").await;
    success(&tmux.continue_detached(&fixture, Some("tui")).await);
    wait_for_attach(&fixture, A, 1).await;
    let panes = tmux.panes("tui").await;
    let pane = &panes[0];
    wait_for_ready_pane(&tmux, &pane.id).await;
    tmux.text(&["send-keys", "-t", &pane.id, "-l", "/done"])
        .await;
    tmux.text(&["send-keys", "-t", &pane.id, "Enter"]).await;
    wait_for_screen(&tmux, &pane.id, "Marked done").await;
    assert!(fixture.candidates(&[]).await.is_empty());
    tmux.text(&["send-keys", "-t", &pane.id, "-l", "/undone"])
        .await;
    tmux.text(&["send-keys", "-t", &pane.id, "Enter"]).await;
    wait_for_screen(&tmux, &pane.id, "Session restored").await;
    assert_eq!(ids(&fixture.candidates(&[]).await), BTreeSet::from([A]));
    assert_eq!(
        *fixture.state.done_writes.lock().unwrap(),
        [(A.to_owned(), true), (A.to_owned(), false)]
    );
    let after = tmux.panes("tui").await;
    assert_eq!(after.len(), 1);
    assert_eq!((&after[0].id, &after[0].pid), (&pane.id, &pane.pid));
    assert!(!after[0].dead);
    assert!(
        fixture
            .state
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(method, path)| allowed_attach_request(method, path)
                || method == "PUT" && path == &format!("/v1/agents/{A}/done"))
    );
    assert!(
        fixture
            .state
            .messages
            .lock()
            .unwrap()
            .iter()
            .all(|message| !matches!(
                message["type"].as_str(),
                Some("prompt" | "cancel" | "restart")
            ))
    );
}

#[tokio::test]
async fn real_tmux_restores_and_reuses_live_panes_with_c_locale() {
    let now = now();
    let fixture = Fixture::new(json!({"data":[A],"summaries":{
        A:summary("ASCII locale",now,presentation("running",now))
    }}))
    .await;
    let tmux = Tmux::new().await;
    let args = [
        "continue",
        "--detach",
        "--tmux-socket",
        &tmux.socket,
        "--session",
        "c-locale",
    ];
    // tmux sanitizes tab format separators to '_' under this locale. This must
    // exercise the production printable-ID receipt path, not the UTF-8 fixture.
    let first = bounded_output(fixture.command().args(args).env("LC_ALL", "C")).await;
    success(&first);
    wait_for_attach(&fixture, A, 1).await;
    let before = tmux.panes("c-locale").await;
    assert_eq!(before.len(), 1);
    assert!(!before[0].dead);
    let repeated = bounded_output(fixture.command().args(args).env("LC_ALL", "C")).await;
    success(&repeated);
    let after = tmux.panes("c-locale").await;
    assert_eq!(after.len(), 1);
    assert_eq!(
        (&before[0].id, &before[0].pid),
        (&after[0].id, &after[0].pid)
    );
    assert!(String::from_utf8_lossy(&repeated.stderr).contains("Reused"));
    fixture.assert_read_only();
}
