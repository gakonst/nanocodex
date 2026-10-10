//! A second client controls a running native TUI through its private socket.
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

use std::{
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

use eyre::{Result, eyre};
use futures_util::{SinkExt, StreamExt};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::{
        TcpListener, UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    time::timeout,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const TIMEOUT: Duration = Duration::from_secs(20);
const DRAFT: &str = "unfinished local draft";

// The unified TUI's private control socket, driven by a second client: its
// action menu and composer draft are observable state, external prompts run
// without an execution policy, and filtered subscribers skip raw provider
// events while the user's own draft survives. The legacy TUI's external
// "command" channel and slash-completion state were removed with it in
// 02acb18e6; the unified TUI reports commands=false (#946).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_client_prompts_and_filters_events_without_touching_the_draft() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(serve_responses(listener));
    let workspace = tempfile::tempdir()?;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 32,
            cols: 140,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(pty)?;
    let mut command = CommandBuilder::new(local_cli());
    command.cwd(workspace.path());
    for argument in [
        "--api-key",
        "test-key",
        "--websocket-url",
        &endpoint,
        "--browser=none",
        "--mcp-defaults",
        "false",
        "--web-search",
        "false",
        "--image-generation",
        "false",
    ] {
        command.arg(argument);
    }
    command.env("HOME", workspace.path());
    command.env("CODEX_HOME", workspace.path().join(".codex"));
    command.env("NANOCODEX_COMPUTER", "off");
    command.env("TERM", "xterm-256color");
    command.env_remove("OPENAI_API_KEY");
    command.env_remove("ANTHROPIC_API_KEY");
    command.env_remove("TMUX");
    command.env_remove("TMUX_PANE");
    let mut child = pair.slave.spawn_command(command).map_err(pty)?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(pty)?;
    std::thread::spawn(move || {
        let mut bytes = [0; 8192];
        while matches!(std::io::Read::read(&mut reader, &mut bytes), Ok(count) if count > 0) {}
    });
    let mut keyboard = pair.master.take_writer().map_err(pty)?;

    let registration =
        registration(&workspace.path().join(".codex/nanocodex/tui/instances")).await?;
    let mut client = Client::connect(&registration).await?;
    let mut observer = Client::connect(&registration).await?;
    let hello = client.hello.clone();
    let snapshot = &hello["snapshot"];
    assert_eq!(snapshot["capabilities"]["commands"], false);
    assert_eq!(snapshot["capabilities"]["event_filter"], true);
    assert_eq!(snapshot["capabilities"]["state_notifications"], true);

    // "/" opens the action menu without typing into the composer; Esc closes it.
    keyboard.write_all(b"/")?;
    keyboard.flush()?;
    client
        .state_until("the action menu never opened", |state| {
            state["menu"] == "actions" && state["composer"]["text"] == ""
        })
        .await?;
    keyboard.write_all(b"\x1b")?;
    keyboard.flush()?;
    client
        .state_until("Esc did not close the action menu", |state| {
            state["menu"].is_null()
        })
        .await?;

    keyboard.write_all(DRAFT.as_bytes())?;
    keyboard.flush()?;
    client
        .state_until("typed draft never reached the TUI state", |state| {
            state["composer"]["text"] == DRAFT
        })
        .await?;

    let filter = json!(["api.event", "model.*"]);
    let subscribed = client
        .request(
            "events.subscribe",
            json!({"after_seq":snapshot["seq"],"exclude_types":filter}),
        )
        .await?;
    assert_eq!(subscribed["exclude_types"], filter);
    observer
        .request("events.subscribe", json!({"after_seq":snapshot["seq"]}))
        .await?;

    let models = client.request("models.list", json!({})).await?;
    let offered = |id: &str| {
        models["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model["id"] == id)
    };
    assert!(
        offered("gpt-6-astra") && offered("claude-sonnet-5-5"),
        "{models}"
    );
    let state = client.request("state.get", json!({})).await?;
    let target = |extra: Value| {
        let mut params = json!({"expected_instance_id":registration["instance_id"],
            "expected_session_id":state["active_session_id"],"expected_active_generation":state["active_generation"]});
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        params
    };
    let original_model = state["state"]["settings"]["model"].clone();
    // A model from the other harness family is refused outright, leaving the
    // local session's settings and the user's draft untouched.
    let foreign = client
        .request(
            "settings.set",
            target(json!({"expected_settings_revision":state["state"]["settings_revision"],"settings":{"model":"claude-sonnet-5-5"}})),
        )
        .await?;
    assert_eq!(foreign["status"], "rejected", "{foreign}");
    assert!(
        foreign["message"]
            .as_str()
            .unwrap_or("")
            .contains("another harness family"),
        "{foreign}"
    );
    let unchanged = client.request("state.get", json!({})).await?;
    assert_eq!(unchanged["state"]["settings"]["model"], original_model);
    assert_eq!(unchanged["state"]["composer"]["text"], DRAFT);
    // Before the first turn a same-family model can still be selected.
    let selected = client
        .request(
            "settings.set",
            target(json!({"expected_settings_revision":unchanged["state"]["settings_revision"],"settings":{"model":"gpt-6-astra"}})),
        )
        .await?;
    assert_eq!(selected["status"], "accepted", "{selected}");
    client
        .state_until("the selected model never reached the TUI state", |state| {
            state["settings"]["model"] == "gpt-6-astra"
        })
        .await?;

    let receipt = client
        .request(
            "prompt",
            target(json!({"input":{"text":"external prompt"}})),
        )
        .await?;
    assert_eq!(receipt["status"], "accepted", "{receipt}");
    let turn = receipt["result"]["turn_id"]
        .as_str()
        .ok_or_else(|| eyre!("no turn ID"))?
        .to_owned();
    client
        .notification(|event| {
            event["type"] == "managed.event"
                && event["data"]["turn_id"] == turn.as_str()
                && event["data"]["event"]["type"] == "assistant.message"
                && event["data"]["event"]["payload"]["text"] == "EXTERNAL_REPLY"
        })
        .await?;
    client
        .notification(|event| {
            event["type"] == "managed.event" && event["data"]["event"]["type"] == "run.completed"
        })
        .await?;
    let kinds = |client: &Client| {
        client
            .events
            .iter()
            .filter(|event| event["type"] == "managed.event")
            .filter_map(|event| event["data"]["event"]["type"].as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    assert!(
        !kinds(&client)
            .iter()
            .any(|kind| kind == "api.event" || kind.starts_with("model.")),
        "filtered subscriber received {:?}",
        kinds(&client)
    );
    observer
        .notification(|event| {
            event["type"] == "managed.event" && event["data"]["event"]["type"] == "run.completed"
        })
        .await?;
    assert!(kinds(&observer).iter().any(|kind| kind == "api.event"));
    assert!(
        kinds(&observer)
            .iter()
            .any(|kind| kind.starts_with("model."))
    );
    assert!(
        client
            .events
            .iter()
            .filter(|event| event["type"] == "state.changed")
            .all(|event| event["data"]["state"].get("composer").is_none()
                && !event.to_string().contains(DRAFT)),
        "state notifications must not replay the draft"
    );
    assert_eq!(
        client.request("state.get", json!({})).await?["state"]["composer"]["text"],
        DRAFT
    );
    let locked_state = client.request("state.get", json!({})).await?;
    let locked = client
        .request(
            "settings.set",
            target(json!({"expected_settings_revision":locked_state["state"]["settings_revision"],"settings":{"model":"gpt-6.1-sol"}})),
        )
        .await?;
    assert_eq!(locked["status"], "rejected", "{locked}");
    assert!(
        locked["message"]
            .as_str()
            .unwrap_or("")
            .contains("before the first turn is accepted"),
        "{locked}"
    );
    let continued = client
        .request(
            "prompt",
            target(json!({"input":{"text":"continue after rejected model change"}})),
        )
        .await?;
    assert_eq!(continued["status"], "accepted", "{continued}");
    child.kill()?;
    server.abort();
    Ok(())
}

fn pty(error: impl std::fmt::Display) -> eyre::Report {
    eyre!(error.to_string())
}

async fn registration(directory: &Path) -> Result<Value> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(entry) = std::fs::read_dir(directory)
            .ok()
            .and_then(|mut entries| entries.next())
        {
            let value: Value = serde_json::from_slice(&std::fs::read(entry?.path())?)?;
            if value["active_session_id"].is_string() {
                return Ok(value);
            }
        }
        if Instant::now() > deadline {
            return Err(eyre!("the TUI never published a control registration"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct Client {
    lines: Lines<BufReader<OwnedReadHalf>>,
    write: OwnedWriteHalf,
    hello: Value,
    events: Vec<Value>,
    next: u64,
}

impl Client {
    async fn connect(registration: &Value) -> Result<Self> {
        let socket = UnixStream::connect(registration["socket_path"].as_str().unwrap()).await?;
        let (read, mut write) = socket.into_split();
        let mut lines = BufReader::new(read).lines();
        let auth = json!({"protocol_version":1,"instance_id":registration["instance_id"],"auth_token":registration["auth_token"]});
        write.write_all(format!("{auth}\n").as_bytes()).await?;
        let hello =
            serde_json::from_str(&lines.next_line().await?.ok_or_else(|| eyre!("no hello"))?)?;
        Ok(Self {
            lines,
            write,
            hello,
            events: Vec::new(),
            next: 0,
        })
    }

    async fn frame(&mut self) -> Result<Value> {
        let line = timeout(TIMEOUT, self.lines.next_line())
            .await
            .map_err(|_| eyre!("control socket stalled"))??
            .ok_or_else(|| eyre!("control socket closed"))?;
        Ok(serde_json::from_str(&line)?)
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = format!("journey-{}", self.next);
        let request = json!({"id":id,"method":method,"params":params});
        self.write
            .write_all(format!("{request}\n").as_bytes())
            .await?;
        loop {
            let frame = self.frame().await?;
            if frame["id"] == id.as_str() {
                return Ok(frame["result"].clone());
            }
            self.events.push(frame);
        }
    }

    async fn state_until(&mut self, failure: &str, ready: impl Fn(&Value) -> bool) -> Result<()> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let state = self.request("state.get", json!({})).await?;
            if ready(&state["state"]) {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(eyre!("{failure}: {state}"));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn notification(&mut self, matches: impl Fn(&Value) -> bool) -> Result<Value> {
        if let Some(event) = self.events.iter().find(|event| matches(event)) {
            return Ok(event.clone());
        }
        loop {
            let frame = self.frame().await?;
            self.events.push(frame.clone());
            if matches(&frame) {
                return Ok(frame);
            }
        }
    }
}

/// Answers warmups with an empty response and every generation with one reply.
async fn serve_responses(listener: TcpListener) -> Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            let mut socket = accept_async(stream).await?;
            while let Some(message) = socket.next().await {
                let Message::Text(text) = message? else {
                    continue;
                };
                let request: Value = serde_json::from_str(text.as_str())?;
                let item = json!({"type":"message","id":"msg_external","role":"assistant","status":"completed",
                    "content":[{"type":"output_text","text":"EXTERNAL_REPLY","annotations":[]}]});
                let output = if request["generate"] == false {
                    vec![]
                } else {
                    for event in [
                        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg_external","role":"assistant","content":[]}}),
                        json!({"type":"response.output_text.delta","output_index":0,"delta":"EXTERNAL_"}),
                        json!({"type":"response.output_text.delta","output_index":0,"delta":"REPLY"}),
                        json!({"type":"response.output_item.done","output_index":0,"item":item}),
                    ] {
                        socket.send(Message::Text(event.to_string().into())).await?;
                    }
                    vec![item]
                };
                let completed = json!({"type":"response.completed","response":{"id":"resp_external","status":"completed","output":output,
                    "usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,
                        "output_tokens_details":{"reasoning_tokens":0},"total_tokens":2}}});
                socket
                    .send(Message::Text(completed.to_string().into()))
                    .await?;
            }
            Ok::<(), eyre::Report>(())
        });
    }
}
