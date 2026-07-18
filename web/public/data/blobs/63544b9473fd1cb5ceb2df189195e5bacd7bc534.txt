use std::path::{Path, PathBuf};

use eyre::{Result, eyre};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{net::TcpListener, time::timeout};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

use super::ModelRun;
use crate::{
    model::{ModelConfig, ReasoningEffort},
    protocol::{EventWriter, Task, TaskInstruction},
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
async fn unsupported_direct_tools_return_failed_results_to_the_model() -> Result<()> {
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
                "resp-unsupported",
                &[
                    json!({
                        "type": "custom_tool_call",
                        "call_id": "call-custom",
                        "name": "missing_custom",
                        "input": "raw input"
                    }),
                    json!({
                        "type": "function_call",
                        "call_id": "call-function",
                        "namespace": "example::",
                        "name": "missing_function",
                        "arguments": "not json"
                    }),
                ],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-unsupported");
        let input = continuation["input"]
            .as_array()
            .ok_or_else(|| eyre!("continuation input was not an array"))?;
        assert_eq!(
            input,
            &[
                json!({
                    "type": "custom_tool_call_output",
                    "call_id": "call-custom",
                    "output": "unsupported custom tool call: missing_custom"
                }),
                json!({
                    "type": "function_call_output",
                    "call_id": "call-function",
                    "output": "unsupported call: example::missing_function"
                }),
            ]
        );
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("unsupported-tools")?;
    let output = run_model(&endpoint, &workspace, "recover from unsupported tools").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert_eq!(
        output.matches(r#""status":"failed""#).count(),
        2,
        "{output}"
    );
    assert!(output.contains("\"tool_calls\":2"));
    assert!(output.contains("\"run.completed\""));
    assert!(!output.contains("\"run.failed\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn code_mode_notify_adds_a_named_exec_output_to_the_next_request() -> Result<()> {
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
                "resp-notify",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "notify({phase: \"working\"}); text(\"done\");"
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-notify");
        let input = continuation["input"]
            .as_array()
            .ok_or_else(|| eyre!("continuation input was not an array"))?;
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "custom_tool_call_output");
        assert_eq!(input[0]["call_id"], "call-exec");
        assert!(input[0].get("name").is_none());
        assert!(input[0].to_string().contains("done"));
        assert_eq!(input[1]["type"], "custom_tool_call_output");
        assert_eq!(input[1]["call_id"], "call-exec");
        assert_eq!(input[1]["name"], "exec");
        assert_eq!(input[1]["output"], r#"{"phase":"working"}"#);
        assert!(input[1].get("success").is_none());
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("code-mode-notify")?;
    run_model(&endpoint, &workspace, "send a progress notification").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn prepares_images_and_recovers_from_invalid_image_requests() -> Result<()> {
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
                "resp-image",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-image",
                    "name": "exec",
                    "input": "image(\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=\", \"original\");"
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        let output = continuation["input"][0]["output"]
            .as_array()
            .ok_or_else(|| eyre!("image tool output was not content"))?;
        let image = output
            .iter()
            .find(|item| item["type"] == "input_image")
            .ok_or_else(|| eyre!("prepared image was missing"))?;
        assert!(
            image["image_url"]
                .as_str()
                .is_some_and(|url| url.starts_with("data:image/png;base64,"))
        );
        assert!(image.get("detail").is_none());

        send_json(
            &mut socket,
            json!({
                "type": "response.failed",
                "response": {
                    "id": "resp-invalid-image",
                    "status": "failed",
                    "error": {
                        "code": "invalid_image",
                        "message": "The image data you provided does not represent a valid image"
                    }
                }
            }),
        )
        .await?;

        let retry = next_json(&mut socket).await?;
        assert_eq!(retry["previous_response_id"], "resp-image");
        let output = retry["input"][0]["output"]
            .as_array()
            .ok_or_else(|| eyre!("sanitized image tool output was not content"))?;
        assert!(output.iter().all(|item| item["type"] != "input_image"));
        assert!(output.iter().any(|item| {
            item["type"] == "input_text" && item["text"].as_str() == Some("Invalid image")
        }));
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("images")?;
    let output = run_model(&endpoint, &workspace, "inspect images").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"model_calls\":3"));
    assert!(output.contains("\"run.completed\""));
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
async fn explicit_end_turn_false_continues_without_a_tool_call() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        let mut response = completed_response(
            "resp-continue",
            &[json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "intermediate" }]
            })],
        );
        response["response"]["end_turn"] = json!(false);
        send_json(&mut socket, response).await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-continue");
        assert_eq!(continuation["input"].as_array().map(Vec::len), Some(0));
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("end-turn-false")?;
    let output = run_model(&endpoint, &workspace, "continue when requested").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"model_calls\":2"));
    assert!(output.contains("\"text\":\"done\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn completed_response_accepts_null_usage() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        let mut response = completed_response(
            "resp-final",
            &[json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "done" }]
            })],
        );
        response["response"]["usage"] = Value::Null;
        send_json(&mut socket, response).await
    });

    let workspace = temporary_workspace("null-usage")?;
    let output = run_model(&endpoint, &workspace, "accept missing usage").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"model.call.completed\""));
    assert!(output.contains("\"usage\":null"));
    assert!(output.contains("\"run.completed\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn completed_response_accepts_null_usage_details() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        let mut response = completed_response(
            "resp-final",
            &[json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "done" }]
            })],
        );
        response["response"]["usage"]["input_tokens_details"] = Value::Null;
        response["response"]["usage"]["output_tokens_details"] = Value::Null;
        send_json(&mut socket, response).await
    });

    let workspace = temporary_workspace("null-usage-details")?;
    let output = run_model(&endpoint, &workspace, "accept missing usage details").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"input_tokens_details\":null"));
    assert!(output.contains("\"output_tokens_details\":null"));
    assert!(output.contains("\"cached_input_tokens\":0"));
    assert!(output.contains("\"reasoning_output_tokens\":0"));
    assert!(output.contains("\"run.completed\""));
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
                372_001,
            ),
        )
        .await?;

        let compact = next_json(&mut socket).await?;
        assert_eq!(compact["previous_response_id"], "resp-tool");
        assert_eq!(compact["input"].as_array().map(Vec::len), Some(2));
        assert_eq!(compact["input"][0]["type"], "custom_tool_call_output");
        assert_eq!(
            compact["input"][0]["output"],
            "Output exceeded the available model context and was truncated"
        );
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
    assert!(
        warmup["input"][0]["tools"][0]["description"]
            .as_str()
            .is_some_and(|description| description.contains("`web__run`"))
    );
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
        instruction: TaskInstruction::Text(instruction.to_owned()),
        workspace: Some(workspace.to_string_lossy().into_owned()),
    };
    let config = ModelConfig {
        model: model.to_owned(),
        api_key: "test-key".to_owned(),
        effort: ReasoningEffort::Low,
        websocket_url: endpoint.to_owned(),
        api_base_url: "http://127.0.0.1:1/v1".to_owned(),
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
