//! Shipped-CLI journey: a yielded Code Mode cell still running when its turn
//! completes or is interrupted must close every nested call it started. Only
//! Responses inference is synthetic.
#![cfg(unix)]
#[path = "support/local_cli.rs"]
mod local_cli;

use std::{collections::HashMap, process::Stdio, time::Duration};

use eyre::{Result, eyre};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{net::TcpListener, process::Command, sync::oneshot, time::timeout};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const YIELDED_CELL: &str = "// @exec: {\"yield_time_ms\": 300}\n\
const r = await tools.exec_command({cmd: \"sleep 30\", login: false, yield_time_ms: 60000});\n\
text(JSON.stringify(r));";

#[tokio::test]
async fn yielded_cell_calls_close_when_the_turn_completes_or_is_interrupted() -> Result<()> {
    // The model answers without waiting: shutdown must report the nested call.
    let events = run_cli(false).await?;
    assert_every_call_closed(&events)?;
    assert!(
        events.iter().any(|event| event["type"] == "run.completed"),
        "completed run missing: {events:?}"
    );
    // The user interrupts while the model is thinking after the yield.
    let events = run_cli(true).await?;
    assert_every_call_closed(&events)?;
    assert!(
        events.iter().any(|event| event["type"] == "run.failed"),
        "interrupted run missing run.failed: {events:?}"
    );
    Ok(())
}

async fn run_cli(interrupt: bool) -> Result<Vec<Value>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (stalled_tx, stalled_rx) = oneshot::channel();
    let server = tokio::spawn(serve(listener, interrupt, stalled_tx));
    let workspace = tempfile::tempdir()?;
    // The local agent tree (and its run --browser flags) is selected by the ncl name.
    let child = Command::new(local_cli::local_cli())
        .current_dir(workspace.path())
        .env("HOME", workspace.path())
        .env("CODEX_HOME", workspace.path().join(".codex"))
        .env("NANOCODEX_COMPUTER", "off")
        .env_remove("OPENAI_API_KEY")
        .args([
            "run",
            "--browser=none",
            "--api-key",
            "test-key",
            "--websocket-url",
        ])
        .arg(&endpoint)
        .arg("--cwd")
        .arg(workspace.path())
        .args([
            "--rollouts",
            "false",
            "--mcp-defaults",
            "false",
            "--web-search",
            "false",
        ])
        .args(["--image-generation", "false", "run the yielded cell"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    if interrupt {
        timeout(Duration::from_secs(20), stalled_rx)
            .await
            .map_err(|_| eyre!("CLI never continued after the yield"))??;
        let pid = child.id().ok_or_else(|| eyre!("CLI had no process ID"))?;
        let status = Command::new("kill")
            .args(["-INT", &pid.to_string()])
            .status()
            .await?;
        assert!(status.success(), "failed to interrupt the CLI");
    }
    // The nested command sleeps 30s; finishing well before proves the run did
    // not wait for it, and a missing receipt would not hang the CLI either.
    let output = timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .map_err(|_| eyre!("CLI did not exit after its yielded cell"))??;
    server.abort();
    assert_eq!(
        output.status.success(),
        !interrupt,
        "unexpected exit status: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)?
        .lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn assert_every_call_closed(events: &[Value]) -> Result<()> {
    let mut results = HashMap::<&str, Vec<&Value>>::new();
    for event in events.iter().filter(|event| event["type"] == "tool.result") {
        let call = event["payload"]["call_id"].as_str().unwrap_or_default();
        results.entry(call).or_default().push(&event["payload"]);
    }
    let calls = events
        .iter()
        .filter(|event| event["type"] == "tool.call")
        .map(|event| event["payload"]["call_id"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert!(
        calls.iter().any(|call| call.ends_with("/code-1")),
        "the cell never started its nested call: {events:?}"
    );
    for call in calls {
        let terminal = results.get(call).map_or(0, Vec::len);
        assert_eq!(terminal, 1, "{call} has {terminal} terminal results");
        if call.ends_with("/code-1") {
            let result = results[call][0];
            assert_eq!(result["status"], "failed", "{result}");
            assert_eq!(
                result["structured_result"]["code"], "CODE_MODE_CALL_INTERRUPTED",
                "{result}"
            );
        }
    }
    Ok(())
}

async fn serve(
    listener: TcpListener,
    stall_after_yield: bool,
    stalled: oneshot::Sender<()>,
) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let mut socket = accept_async(stream).await?;
    let mut stalled = Some(stalled);
    let mut responses = 0_u32;
    while let Some(message) = socket.next().await {
        let Message::Text(text) = message? else {
            continue;
        };
        let request: Value = serde_json::from_str(text.as_str())?;
        responses += 1;
        let id = format!("response-{responses}");
        let continuation = request["input"]
            .as_array()
            .and_then(|input| input.last())
            .is_some_and(|item| item["type"] == "custom_tool_call_output");
        let output = if request["generate"] == false {
            vec![]
        } else if !continuation {
            vec![
                json!({"type": "custom_tool_call", "call_id": "call-cell", "name": "exec",
                "input": YIELDED_CELL, "status": "completed"}),
            ]
        } else if stall_after_yield {
            if let Some(stalled) = stalled.take() {
                let _ = stalled.send(());
            }
            continue;
        } else {
            vec![json!({"type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "answered without waiting"}]})]
        };
        let completed = json!({"type": "response.completed", "response": {"id": id,
            "status": "completed", "output": output, "usage": {"input_tokens": 1,
            "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 1,
            "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 2}}});
        socket
            .send(Message::Text(completed.to_string().into()))
            .await?;
    }
    Ok(())
}
