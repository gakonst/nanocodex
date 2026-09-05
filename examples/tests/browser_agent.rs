use std::time::Duration;

use eyre::{Result, eyre};
use futures_util::{SinkExt, StreamExt};
use nanocodex::{Nanocodex, OpenAi, Thinking, Tools};
use nanocodex_browser::{BrowserAction, BrowserTool};
use serde_json::{Value, json};
use tokio::{net::TcpListener, time::timeout};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

const BROWSER_PROGRAM: &str = r#"
const opened = await tools.browser({
  action: "open",
  url: "data:text/html,<main><h1>agent-browser-proof</h1></main>"
});
const snapshot = await tools.browser({ action: "snapshot" });
text({ opened, snapshot });
"#;

#[tokio::test]
async fn nanocodex_drives_browser_through_code_mode() -> Result<()> {
    let (browser, recording) = BrowserTool::recording();
    let continuation = run_agent(browser).await?;

    assert_code_mode_output(&continuation, false)?;
    let actions = recording.actions()?;
    assert_eq!(actions.len(), 2);
    assert_eq!(
        actions[0].action,
        BrowserAction::Open {
            url: "data:text/html,<main><h1>agent-browser-proof</h1></main>".to_owned(),
        }
    );
    assert!(matches!(actions[1].action, BrowserAction::Snapshot { .. }));
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
async fn nanocodex_drives_a_live_chromium_browser_through_code_mode() -> Result<()> {
    let continuation = run_agent(BrowserTool::new()?).await?;
    assert_code_mode_output(&continuation, true)
}

async fn run_agent(browser: BrowserTool) -> Result<Value> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;

        let warmup = next_json(&mut socket).await?;
        assert_eq!(warmup["generate"], false);
        let exec = warmup["input"][0]["tools"]
            .as_array()
            .and_then(|tools| tools.iter().find(|tool| tool["name"] == "exec"))
            .ok_or_else(|| eyre!("warmup did not expose Code Mode"))?;
        let description = exec["description"]
            .as_str()
            .ok_or_else(|| eyre!("Code Mode description was not text"))?;
        assert!(description.contains("tools.browser"));
        assert!(description.contains("host-managed browser session"));
        assert!(
            !description.contains("detect_gate"),
            "warmup unexpectedly included the browser action schema: {exec}"
        );
        send_completed(&mut socket, "resp-warmup", &[], None).await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_completed(
            &mut socket,
            "resp-browser",
            &[json!({
                "id": "item-browser-exec",
                "type": "custom_tool_call",
                "call_id": "call-browser-exec",
                "name": "exec",
                "input": BROWSER_PROGRAM,
            })],
            Some(12),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-browser");
        assert_eq!(continuation["input"][0]["type"], "custom_tool_call_output");
        assert_eq!(continuation["input"][0]["call_id"], "call-browser-exec");
        send_completed(
            &mut socket,
            "resp-final",
            &[json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "browser complete"}],
            })],
            Some(12),
        )
        .await?;
        Result::<Value>::Ok(continuation)
    });

    let tools = Tools::builder()
        .without_defaults()
        .provider(browser)
        .build()?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .tools(tools)
        .build()?;
    let turn = agent.prompt("Use the browser tool now.").await?;
    let result = timeout(Duration::from_secs(20), turn.result())
        .await
        .map_err(|_| eyre!("Nanocodex did not complete the browser turn"))??;
    assert_eq!(result.final_message(), "browser complete");
    drop((agent, events));

    timeout(Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))??
}

fn assert_code_mode_output(continuation: &Value, live: bool) -> Result<()> {
    let content = continuation["input"][0]["output"]
        .as_array()
        .ok_or_else(|| eyre!("Code Mode output was not content: {continuation}"))?;
    let output = content
        .iter()
        .filter_map(|item| item["text"].as_str())
        .find_map(|text| serde_json::from_str::<Value>(text).ok())
        .ok_or_else(|| eyre!("Code Mode output omitted its JSON result: {continuation}"))?;
    assert_eq!(output["opened"]["sequence"], 0);
    assert_eq!(output["snapshot"]["sequence"], 1);
    assert_eq!(output["opened"]["executed"], live);
    assert_eq!(output["snapshot"]["executed"], live);
    if live {
        assert!(
            output["snapshot"]["snapshot"]
                .as_str()
                .is_some_and(|snapshot| snapshot.contains("agent-browser-proof")),
            "live browser snapshot omitted page content: {output}"
        );
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
    total_tokens: Option<u64>,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let usage = total_tokens.map(|total_tokens| {
        json!({
            "input_tokens": 10,
            "input_tokens_details": {"cached_tokens": 5},
            "output_tokens": 2,
            "output_tokens_details": {"reasoning_tokens": 1},
            "total_tokens": total_tokens,
        })
    });
    let event = json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "status": "completed",
            "output": output,
            "usage": usage,
        },
    });
    socket.send(Message::Text(event.to_string().into())).await?;
    Ok(())
}
