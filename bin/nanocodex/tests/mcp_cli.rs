use std::path::PathBuf;

use eyre::{Result, eyre};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{net::TcpListener, process::Command, time::timeout};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

const TURNS: usize = 8;

#[tokio::test]
async fn repeated_cli_turns_search_and_call_mcp_through_the_library() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(serve_responses(listener));

    let workspace = temporary_workspace()?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/nanocodex-mcp/tests/fixtures/stdio-server.mjs")
        .canonicalize()?;
    let output = timeout(
        std::time::Duration::from_secs(30),
        Command::new(env!("CARGO_BIN_EXE_nanocodex"))
            .arg("--api-key")
            .arg("test-key")
            .arg("--websocket-url")
            .arg(endpoint)
            .arg("--cwd")
            .arg(&workspace)
            .arg("--mcp-stdio")
            .arg("fixture=node")
            .arg("--mcp-arg")
            .arg(format!("fixture={}", fixture.display()))
            .arg("run")
            .arg("--repeat")
            .arg(TURNS.to_string())
            .arg("exercise the fixture MCP server")
            .output(),
    )
    .await
    .map_err(|_| eyre!("CLI MCP stress harness exceeded 30 seconds"))??;

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(output.status.success(), "CLI failed:\n{stderr}\n{stdout}");
    let events = stdout
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "run.completed")
            .count(),
        TURNS
    );
    assert_eq!(tool_results(&events, "tool_search"), TURNS);
    assert_eq!(tool_results(&events, "mcp__fixture__echo"), TURNS);
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

async fn serve_responses(listener: TcpListener) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let mut socket = accept_async(stream).await?;
    let warmup = next_json(&mut socket).await?;
    assert!(warmup["input"].is_array());
    assert!(
        warmup["input"][0]["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| {
                tool["description"]
                    .as_str()
                    .is_some_and(|description| description.contains("### `tool_search`"))
            })),
        "tool_search was missing from warmup: {warmup}"
    );
    send_completed(&mut socket, "response-warmup", &[]).await?;

    let mut previous_response = "response-warmup".to_owned();
    for turn in 0..TURNS {
        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], previous_response);
        assert!(generation.get("tools").is_none());
        let tool_response = format!("response-tool-{turn}");
        send_completed(
            &mut socket,
            &tool_response,
            &[json!({
                "type": "custom_tool_call",
                "call_id": format!("call-exec-{turn}"),
                "name": "exec",
                "input": format!(
                    "const found = await tools.tool_search({{query: \"echo message\"}}); \
                     const called = await tools[found.tools[0].name]({{message: \"turn-{turn}\"}}); \
                     text(called);"
                )
            })],
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], tool_response);
        assert_eq!(continuation["input"][0]["type"], "custom_tool_call_output");
        assert!(
            continuation
                .to_string()
                .contains(&format!("fixture:turn-{turn}")),
            "MCP result was missing from continuation: {continuation}"
        );
        previous_response = format!("response-final-{turn}");
        send_completed(
            &mut socket,
            &previous_response,
            &[json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": format!("done {turn}") }]
            })],
        )
        .await?;
    }
    Ok(())
}

fn tool_results(events: &[Value], tool: &str) -> usize {
    events
        .iter()
        .filter(|event| event["type"] == "tool.result" && event["payload"]["tool"] == tool)
        .count()
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
                        "input_tokens": 10,
                        "input_tokens_details": { "cached_tokens": 5 },
                        "output_tokens": 2,
                        "output_tokens_details": { "reasoning_tokens": 1 },
                        "total_tokens": 12
                    }
                }
            })
            .to_string()
            .into(),
        ))
        .await?;
    Ok(())
}

fn temporary_workspace() -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "nanocodex-mcp-cli-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
