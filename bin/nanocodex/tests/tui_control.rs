//! A second client controls a running native TUI through its private socket.

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

// Slash suggestions work through the shipped TUI, then external clients prompt
// without an execution policy, run commands, and follow filtered state while
// the user's own draft survives.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_client_prompts_runs_commands_and_filters_events_without_touching_the_draft()
-> Result<()> {
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
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_nanocodex"));
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
    assert_eq!(snapshot["capabilities"]["commands"], true);
    assert_eq!(snapshot["capabilities"]["event_filter"], true);

    keyboard.write_all(b"/")?;
    keyboard.flush()?;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let state = client.request("state.get", json!({})).await?;
        if state["state"]["composer"]["text"] == "/" {
            assert_eq!(state["state"]["menu"], "slash");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "slash suggestions never opened in the TUI"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    keyboard.write_all(b"\x1b[B\t")?;
    keyboard.flush()?;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let state = client.request("state.get", json!({})).await?;
        if state["state"]["composer"]["text"] == "/thinking " {
            assert!(state["state"]["menu"].is_null());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "arrow and Tab did not complete the selected slash command"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    keyboard.write_all(b"\x15")?;
    keyboard.flush()?;
    let deadline = Instant::now() + TIMEOUT;
    while !client.request("state.get", json!({})).await?["state"]["composer"]["text"]
        .as_str()
        .is_some_and(str::is_empty)
    {
        assert!(
            Instant::now() < deadline,
            "completed slash command could not be cleared"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    keyboard.write_all(DRAFT.as_bytes())?;
    keyboard.flush()?;
    let deadline = Instant::now() + TIMEOUT;
    while client.request("state.get", json!({})).await?["state"]["composer"]["text"] != DRAFT {
        assert!(
            Instant::now() < deadline,
            "typed draft never reached the TUI state"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

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
    let fast = !state["state"]["settings"]["fast_mode"]
        .as_bool()
        .unwrap_or(false);
    let toggle = if fast { "/fast on" } else { "/fast off" };
    let receipt = client
        .request("command", target(json!({"input":{"text":toggle}})))
        .await?;
    assert_eq!(receipt["status"], "accepted", "{receipt}");
    let changed = client
        .notification(|event| {
            event["type"] == "state.changed"
                && event["data"]["state"]["settings"]["fast_mode"] == fast
        })
        .await?;
    assert_eq!(changed["data"]["composer_empty"], false);
    assert!(changed["data"]["state"].get("composer").is_none());
    assert!(
        !changed.to_string().contains(DRAFT),
        "notifications must not replay the draft"
    );
    for (text, code) in [
        ("plain text", "not_a_command"),
        ("/model", "interactive_command"),
    ] {
        let receipt = client
            .request("command", target(json!({"input":{"text":text}})))
            .await?;
        assert_eq!(receipt["code"], code, "{text}: {receipt}");
    }

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
            event["data"]["type"] == "assistant.message"
                && event["data"]["payload"]["text"] == "EXTERNAL_REPLY"
                && event["data"]["payload"]["turn_id"] == turn.as_str()
        })
        .await?;
    client
        .notification(|event| event["data"]["type"] == "run.completed")
        .await?;
    let kinds = |client: &Client| {
        client
            .events
            .iter()
            .filter_map(|event| event["data"]["type"].as_str().map(str::to_owned))
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
        .notification(|event| event["data"]["type"] == "run.completed")
        .await?;
    assert!(kinds(&observer).iter().any(|kind| kind == "api.event"));
    assert_eq!(
        client.request("state.get", json!({})).await?["state"]["composer"]["text"],
        DRAFT
    );
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
