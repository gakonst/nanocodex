use std::path::{Path, PathBuf};

use eyre::{Result, eyre};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{net::TcpListener, time::timeout};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

use super::ModelRun;
use crate::{
    model::{ModelConfig, ReasoningEffort},
    protocol::{EventWriter, Task},
};

#[tokio::test]
async fn store_false_local_code_mode_round_trip() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let warmup = next_json(&mut socket).await?;
        assert_warmup(&warmup);
        send_json(
            &mut socket,
            json!({
                "type": "response.metadata",
                "headers": { "x-codex-turn-state": "sticky-test" }
            }),
        )
        .await?;
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        assert_eq!(generation["store"], false);
        assert!(generation.get("generate").is_none());
        assert_eq!(generation["input"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            generation["client_metadata"]["x-codex-turn-state"],
            "sticky-test"
        );
        send_json(
            &mut socket,
            completed_response(
                "resp-tool",
                &[json!({
                    "id": "item-exec",
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "const result = await tools.exec_command({cmd: \"printf hello\"}); text(result.output);"
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-tool");
        assert_eq!(continuation["input"].as_array().map(Vec::len), Some(1));
        assert_eq!(continuation["input"][0]["type"], "custom_tool_call_output");
        assert_eq!(continuation["input"][0]["call_id"], "call-exec");
        assert!(continuation["input"][0].get("success").is_none());
        assert!(
            continuation["input"][0]["output"]
                .as_array()
                .is_some_and(|content| content.iter().any(|item| {
                    item["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("hello"))
                }))
        );
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("code-mode")?;
    let output = run_model(&endpoint, &workspace, "run a shell command").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"tool\":\"exec\""));
    assert!(output.contains("\"tool\":\"exec_command\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn yielded_exec_cell_continues_through_direct_wait_tool() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut socket,
            completed_response(
                "resp-exec",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "text(\"before\"); await yield_control(); await new Promise((resolve) => setTimeout(resolve, 10)); text(\"after\");"
                })],
            ),
        )
        .await?;

        let yielded = next_json(&mut socket).await?;
        assert_eq!(yielded["previous_response_id"], "resp-exec");
        assert_eq!(yielded["input"][0]["type"], "custom_tool_call_output");
        assert!(
            yielded
                .to_string()
                .contains("Script running with cell ID 1")
        );
        send_json(
            &mut socket,
            completed_response(
                "resp-wait",
                &[json!({
                    "type": "function_call",
                    "call_id": "call-wait",
                    "name": "wait",
                    "arguments": "{\"cell_id\":\"1\",\"yield_time_ms\":1000}"
                })],
            ),
        )
        .await?;

        let completed = next_json(&mut socket).await?;
        assert_eq!(completed["previous_response_id"], "resp-wait");
        assert_eq!(completed["input"][0]["type"], "function_call_output");
        assert_eq!(completed["input"][0]["call_id"], "call-wait");
        assert!(completed.to_string().contains("after"));
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("code-mode-wait")?;
    let output = run_model(&endpoint, &workspace, "yield and wait").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"tool\":\"wait\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn warmup_failure_falls_back_to_a_full_first_request() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut first = accept_async(stream).await?;
        assert_warmup(&next_json(&mut first).await?);
        send_json(
            &mut first,
            json!({
                "type": "error",
                "error": { "message": "prewarm unavailable" }
            }),
        )
        .await?;
        drop(first);

        let (stream, _) = listener.accept().await?;
        let mut second = accept_async(stream).await?;
        let generation = next_json(&mut second).await?;
        assert!(generation.get("previous_response_id").is_none());
        assert!(generation.get("generate").is_none());
        assert_eq!(generation["input"].as_array().map(Vec::len), Some(4));
        assert_eq!(generation["input"][0]["type"], "additional_tools");
        assert_eq!(generation["input"][1]["role"], "developer");
        assert_eq!(generation["input"][2]["role"], "user");
        assert_eq!(generation["input"][3]["role"], "user");
        send_final(&mut second, "resp-final").await
    });

    let workspace = temporary_workspace("warmup-fallback")?;
    let output = run_model(&endpoint, &workspace, "exercise warmup fallback").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"model.warmup.failed\""));
    assert!(output.contains("\"purpose\":\"warmup_fallback\""));
    assert!(output.contains("\"connection_attempts\":2"));
    assert!(output.contains("\"websocket_reconnects\":1"));
    assert!(output.contains("\"run.completed\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn warmup_connection_failure_falls_back_to_a_full_first_request() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (failed_prewarm, _) = listener.accept().await?;
        drop(failed_prewarm);

        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let generation = next_json(&mut socket).await?;
        assert!(generation.get("previous_response_id").is_none());
        assert!(generation.get("generate").is_none());
        assert_eq!(generation["input"].as_array().map(Vec::len), Some(4));
        assert_eq!(generation["input"][0]["type"], "additional_tools");
        assert_eq!(generation["input"][1]["role"], "developer");
        assert_eq!(generation["input"][2]["role"], "user");
        assert_eq!(generation["input"][3]["role"], "user");
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("warmup-connection-fallback")?;
    let output = run_model(&endpoint, &workspace, "exercise warmup connection fallback").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"model.connection.failed\""));
    assert!(output.contains("\"purpose\":\"warmup_fallback\""));
    assert!(output.contains("\"connection_attempts\":2"));
    assert!(output.contains("\"websocket_reconnects\":1"));
    assert!(output.contains("\"run.completed\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn continues_past_previous_model_call_limit() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        for call_index in 1..=33 {
            let generation = next_json(&mut socket).await?;
            let previous_response_id = if call_index == 1 {
                "resp-warmup".to_owned()
            } else {
                format!("resp-tool-{}", call_index - 1)
            };
            assert_eq!(generation["previous_response_id"], previous_response_id);
            let response_id = format!("resp-tool-{call_index}");
            let call_id = format!("call-exec-{call_index}");
            send_json(
                &mut socket,
                completed_response(
                    &response_id,
                    &[json!({
                        "type": "custom_tool_call",
                        "call_id": call_id,
                        "name": "exec",
                        "input": "text(\"continue\")"
                    })],
                ),
            )
            .await?;
        }

        let final_generation = next_json(&mut socket).await?;
        assert_eq!(final_generation["previous_response_id"], "resp-tool-33");
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("unbounded-turn")?;
    let output = run_model(&endpoint, &workspace, "continue until done").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"model_calls\":34"));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn reconnect_drops_previous_response_id_and_replays_full_history() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut first = accept_async(stream).await?;
        let warmup = next_json(&mut first).await?;
        assert_warmup(&warmup);
        send_warmup(&mut first, "resp-warmup").await?;
        let generation = next_json(&mut first).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut first,
            completed_response(
                "resp-tool",
                &[json!({
                    "id": "server-item-id",
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "text(\"continued\")"
                })],
            ),
        )
        .await?;
        first.send(Message::Close(None)).await?;
        drop(first);

        let (stream, _) = listener.accept().await?;
        let mut second = accept_async(stream).await?;
        let replay = next_json(&mut second).await?;
        assert!(replay.get("previous_response_id").is_none());
        assert_eq!(replay["store"], false);
        assert_eq!(replay["input"].as_array().map(Vec::len), Some(6));
        assert_eq!(replay["input"][0]["type"], "additional_tools");
        assert_eq!(replay["input"][1]["role"], "developer");
        assert_eq!(replay["input"][2]["role"], "user");
        assert_eq!(replay["input"][4]["type"], "custom_tool_call");
        assert!(replay["input"][4].get("id").is_none());
        assert_eq!(replay["input"][5]["type"], "custom_tool_call_output");
        send_final(&mut second, "resp-final").await
    });

    let workspace = temporary_workspace("reconnect")?;
    run_model(&endpoint, &workspace, "exercise reconnect").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn sol_compacts_with_a_trigger_and_installs_the_returned_context() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut socket,
            completed_response_with_usage(
                "resp-tool",
                &[json!({
                    "id": "item-exec",
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "text(\"tool completed\")"
                })],
                334_800,
            ),
        )
        .await?;

        let compact = next_json(&mut socket).await?;
        assert_eq!(compact["previous_response_id"], "resp-tool");
        assert_eq!(compact["input"].as_array().map(Vec::len), Some(2));
        assert_eq!(compact["input"][0]["type"], "custom_tool_call_output");
        assert_eq!(compact["input"][1], json!({ "type": "compaction_trigger" }));
        send_json(
            &mut socket,
            json!({
                "type": "response.output_item.done",
                "item": {
                    "id": "cmp-server-id",
                    "type": "compaction",
                    "encrypted_content": "opaque-summary"
                }
            }),
        )
        .await?;
        send_json(
            &mut socket,
            completed_response_with_usage("resp-compact", &[], 120),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert!(continuation.get("previous_response_id").is_none());
        assert_eq!(continuation["input"].as_array().map(Vec::len), Some(5));
        assert_eq!(continuation["input"][0]["type"], "additional_tools");
        assert_eq!(continuation["input"][1]["role"], "developer");
        assert_eq!(continuation["input"][2]["role"], "user");
        assert_eq!(continuation["input"][3]["role"], "user");
        assert_eq!(continuation["input"][4]["type"], "compaction");
        assert_eq!(
            continuation["input"][4]["encrypted_content"],
            "opaque-summary"
        );
        assert!(continuation["input"][4].get("id").is_none());
        assert!(continuation.to_string().contains("exercise compaction"));
        assert!(!continuation.to_string().contains("tool completed"));
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("compaction")?;
    let output =
        run_model_with_model(&endpoint, &workspace, "exercise compaction", "gpt-5.6-sol").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"model.compaction.started\""));
    assert!(output.contains("\"model.compaction.completed\""));
    assert!(output.contains("\"compactions\":1"));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

fn assert_warmup(warmup: &Value) {
    assert_eq!(warmup["store"], false);
    assert_eq!(warmup["generate"], false);
    assert_eq!(warmup["stream"], true);
    assert_eq!(warmup["parallel_tool_calls"], false);
    assert_eq!(warmup["prompt_cache_key"], "model-test");
    assert_eq!(warmup["input"].as_array().map(Vec::len), Some(2));
    assert_eq!(warmup["input"][0]["type"], "additional_tools");
    assert_eq!(warmup["input"][0]["role"], "developer");
    assert_eq!(warmup["input"][0]["tools"][0]["type"], "custom");
    assert_eq!(warmup["input"][0]["tools"][0]["name"], "exec");
    assert_eq!(warmup["input"][0]["tools"][1]["type"], "function");
    assert_eq!(warmup["input"][0]["tools"][1]["name"], "wait");
    assert_eq!(warmup["input"][1]["role"], "developer");
    assert!(warmup.get("tools").is_none());
    assert!(warmup.get("instructions").is_none());
    assert!(warmup.get("context_management").is_none());
    assert!(warmup["reasoning"].get("mode").is_none());
    assert_eq!(
        warmup["client_metadata"]["ws_request_header_x_openai_internal_codex_responses_lite"],
        "true"
    );
}

async fn run_model(endpoint: &str, workspace: &Path, instruction: &str) -> Result<String> {
    run_model_with_model(endpoint, workspace, instruction, "test-model").await
}

async fn run_model_with_model(
    endpoint: &str,
    workspace: &Path,
    instruction: &str,
    model: &str,
) -> Result<String> {
    let task = Task {
        instruction: instruction.to_owned(),
        workspace: Some(workspace.to_string_lossy().into_owned()),
    };
    let config = ModelConfig {
        model: model.to_owned(),
        api_key: "test-key".to_owned(),
        effort: ReasoningEffort::Low,
        websocket_url: endpoint.to_owned(),
    };
    let mut output = Vec::new();
    {
        let mut events = EventWriter::new(&mut output, "model-test".to_owned());
        ModelRun::new(&mut events, &task, &config).execute().await?;
    }
    Ok(String::from_utf8(output)?)
}

async fn send_warmup<S>(socket: &mut WebSocketStream<S>, response_id: &str) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_json(
        socket,
        json!({
            "type": "response.completed",
            "response": { "id": response_id, "usage": null }
        }),
    )
    .await
}

async fn send_final<S>(socket: &mut WebSocketStream<S>, response_id: &str) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_json(
        socket,
        completed_response(
            response_id,
            &[json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "done" }]
            })],
        ),
    )
    .await
}

fn completed_response(response_id: &str, output: &[Value]) -> Value {
    completed_response_with_usage(response_id, output, 12)
}

fn completed_response_with_usage(response_id: &str, output: &[Value], total_tokens: u64) -> Value {
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
                "total_tokens": total_tokens
            }
        }
    })
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

async fn send_json<S>(socket: &mut WebSocketStream<S>, value: Value) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket.send(Message::Text(value.to_string().into())).await?;
    Ok(())
}

fn temporary_workspace(label: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "harness-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
