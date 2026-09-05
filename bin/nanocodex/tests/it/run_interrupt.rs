use std::{process::Stdio, time::Duration};

use eyre::{Result, eyre};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{net::TcpListener, process::Command, sync::oneshot, time::timeout};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

#[tokio::test]
async fn interrupts_after_completion_flush_one_terminal_event_and_fail() -> Result<()> {
    assert_signal_after_completion("INT").await?;
    assert_signal_after_completion("TERM").await
}

async fn assert_signal_after_completion(signal_name: &str) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (completed_tx, completed_rx) = oneshot::channel();
    let server = tokio::spawn(serve_completed_response(listener, completed_tx));
    let workspace = tempfile::tempdir()?;
    let child = Command::new(env!("CARGO_BIN_EXE_nanocodex"))
        .current_dir(workspace.path())
        .env_remove("OPENAI_API_KEY")
        .arg("run")
        .arg("--browser=none")
        .arg("--api-key")
        .arg("test-key")
        .arg("--websocket-url")
        .arg(endpoint)
        .arg("--cwd")
        .arg(workspace.path())
        .arg("--rollouts")
        .arg("false")
        .arg("--mcp-defaults")
        .arg("false")
        .arg("--web-search")
        .arg("false")
        .arg("--image-generation")
        .arg("false")
        .arg("complete before interruption")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    timeout(Duration::from_secs(10), completed_rx)
        .await
        .map_err(|_| eyre!("mock response did not complete"))??;

    // The response event is much larger than a pipe, so the adapter is blocked
    // writing stdout while the independently-owned driver commits the turn.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let pid = child.id().ok_or_else(|| eyre!("CLI had no process ID"))?;
    let signal = Command::new("kill")
        .args([format!("-{signal_name}"), pid.to_string()])
        .status()
        .await?;
    assert!(signal.success(), "failed to send SIG{signal_name} to CLI");
    // Keep the pipe backpressured until the CLI has selected the signal branch.
    // Draining it too quickly makes completion and interruption simultaneously
    // ready, so the randomized select can legitimately observe completion first
    // on a loaded runner.
    tokio::time::sleep(Duration::from_secs(1)).await;

    let output = timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .map_err(|_| eyre!("interrupted CLI did not exit"))??;
    timeout(Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    let stdout = String::from_utf8(output.stdout)?;
    let events = stdout
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let terminals = events
        .iter()
        .filter(|event| matches!(event["type"].as_str(), Some("run.completed" | "run.failed")))
        .count();
    assert!(
        !output.status.success(),
        "SIG{signal_name} unexpectedly returned success"
    );
    assert_eq!(
        terminals, 1,
        "every accepted prompt must flush exactly one terminal event"
    );
    Ok(())
}

async fn serve_completed_response(
    listener: TcpListener,
    completed: oneshot::Sender<()>,
) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let mut socket = accept_async(stream).await?;
    let _warmup = next_json(&mut socket).await?;
    send_completed(&mut socket, "warmup", &[]).await?;
    let _generation = next_json(&mut socket).await?;
    socket
        .send(Message::Text(
            json!({
                "type": "response.reasoning_summary_text.delta",
                "summary_index": 0,
                "delta": "x".repeat(2 * 1024 * 1024)
            })
            .to_string()
            .into(),
        ))
        .await?;
    send_completed(
        &mut socket,
        "completed-before-interrupt",
        &[json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "done" }]
        })],
    )
    .await?;
    let _ = completed.send(());
    while let Some(message) = socket.next().await {
        if message.is_err() {
            break;
        }
    }
    Ok(())
}

async fn next_json<S>(socket: &mut WebSocketStream<S>) -> Result<Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let message = socket
            .next()
            .await
            .ok_or_else(|| eyre!("client closed before sending a request"))??;
        if let Message::Text(text) = message {
            return Ok(serde_json::from_str(text.as_str())?);
        }
    }
}

async fn send_completed<S>(
    socket: &mut WebSocketStream<S>,
    response_id: &str,
    output: &[Value],
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            json!({
                "type": "response.completed",
                "response": {
                    "id": response_id,
                    "status": "completed",
                    "output": output,
                    "usage": {
                        "input_tokens": 1,
                        "input_tokens_details": { "cached_tokens": 0 },
                        "output_tokens": 1,
                        "output_tokens_details": { "reasoning_tokens": 0 },
                        "total_tokens": 2
                    }
                }
            })
            .to_string()
            .into(),
        ))
        .await?;
    Ok(())
}
