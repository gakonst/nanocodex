use std::{
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::Arc,
};

use eyre::{Result, eyre};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

use nanocodex_agent::{
    AgentHandle, ExecutionEnvironment, Model, Nanocodex, NanocodexError, OpenAi, PromptRoute,
    ReasoningMode, ResponseError, SpawnOptions, Thinking, Tools,
    events::{AgentEvent, AgentEventData, RunEvent},
    input::Prompt,
    rollout::RolloutConfig,
    session::SessionSnapshot,
    transport::{ResponsesError, ResponsesHistory, ResponsesTransport},
};
use nanocodex_oai_api::{
    auth::{
        OpenAiAuth, OpenAiAuthError, OpenAiAuthFuture, OpenAiAuthMode, OpenAiAuthSnapshot,
        OpenAiAuthSource,
    },
    events::AgentEventKind,
    pricing::CostStatus,
    session::SessionId,
};

const TEST_SESSION_ID: &str = "019c0d31-c308-7d91-bff4-5dca82d15ac6";

fn test_session_id() -> SessionId {
    TEST_SESSION_ID
        .parse()
        .expect("the test session ID is UUIDv7")
}

#[derive(Clone)]
struct StaticChatGptAuth;

impl OpenAiAuthSource for StaticChatGptAuth {
    fn validate(&self) -> std::result::Result<(), OpenAiAuthError> {
        Ok(())
    }

    fn snapshot(
        &self,
    ) -> OpenAiAuthFuture<'_, std::result::Result<OpenAiAuthSnapshot, OpenAiAuthError>> {
        Box::pin(async {
            Ok(OpenAiAuthSnapshot::new(
                OpenAiAuthMode::ChatGpt,
                "subscription-token",
                Some("account-123"),
                false,
                0,
            ))
        })
    }

    fn recover_unauthorized(
        &self,
        _rejected: &OpenAiAuthSnapshot,
    ) -> OpenAiAuthFuture<'_, std::result::Result<(), OpenAiAuthError>> {
        Box::pin(async { Ok(()) })
    }
}

fn chatgpt_auth() -> OpenAiAuth {
    OpenAiAuth::managed_chatgpt(Arc::new(StaticChatGptAuth))
}

mod branching;
mod context;
mod control;
mod instructions;
mod persistence;
mod recovery;
mod tools;
mod transport;

fn assert_warmup(warmup: &Value) {
    assert_warmup_with_store(warmup, false);
}

fn assert_warmup_with_store(warmup: &Value, store: bool) {
    assert_eq!(warmup["store"], store);
    assert_eq!(warmup["generate"], false);
    assert_eq!(warmup["stream"], true);
    assert_eq!(warmup["parallel_tool_calls"], false);
    assert_eq!(warmup["prompt_cache_key"], TEST_SESSION_ID);
    assert_eq!(warmup["input"].as_array().map(Vec::len), Some(2));
    assert_eq!(warmup["input"][0]["type"], "additional_tools");
    assert!(
        warmup["input"][0]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("at_"))
    );
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
    assert!(
        warmup["input"][1]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("msg_"))
    );
    assert!(warmup.get("tools").is_none());
    assert!(warmup.get("instructions").is_none());
    assert!(warmup.get("context_management").is_none());
    assert!(warmup["reasoning"].get("mode").is_none());
    assert_eq!(
        warmup["client_metadata"]["ws_request_header_x_openai_internal_codex_responses_lite"],
        "true"
    );
    let turn_metadata = warmup["client_metadata"]["x-codex-turn-metadata"]
        .as_str()
        .and_then(|metadata| serde_json::from_str::<Value>(metadata).ok())
        .expect("Responses Lite requests include typed Code Mode tool metadata");
    assert_eq!(
        turn_metadata["tool_namespaces_info"]["functions"]["functions"]["view_image"],
        json!({
            "name": "view_image",
            "direct": false,
            "code_mode_name": "view_image",
            "deferred": false,
            "source": { "kind": "harness" },
        })
    );
    assert_eq!(
        turn_metadata["tool_namespaces_info"]["functions"]["functions"]["exec_command"],
        json!({
            "name": "exec_command",
            "direct": false,
            "code_mode_name": "exec_command",
            "deferred": false,
            "source": { "kind": "harness" },
        })
    );
}

fn assert_client_item_id(item: &Value, expected_prefix: &str) {
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .expect("client-authored response item should carry an ID");
    let (prefix, suffix) = id
        .split_once('_')
        .expect("client-authored response item ID should be prefixed");
    assert_eq!(prefix, expected_prefix, "unexpected response item ID: {id}");
    assert!(
        !suffix.is_empty(),
        "response item ID suffix was empty: {id}"
    );
}

fn remove_client_item_id(item: &mut Value, expected_prefix: &str) {
    assert_client_item_id(item, expected_prefix);
    item.as_object_mut()
        .expect("response item should be an object")
        .remove("id")
        .expect("validated response item ID should be present");
}

async fn run_model(endpoint: &str, workspace: &Path, instruction: &str) -> Result<String> {
    let task = Prompt::new(instruction);
    let openai = OpenAi::builder("test-key")
        .experimental_context(false)
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(workspace)
        .session_id(test_session_id())
        .build()?;
    let turn = agent.prompt(task).await?;
    drop(agent);
    let mut output = Vec::new();
    let (event_result, turn_result) = tokio::join!(events.write_jsonl(&mut output), turn.result());
    event_result?;
    turn_result?;
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

async fn send_assistant_output<S>(
    socket: &mut WebSocketStream<S>,
    output_index: u32,
    item_id: &str,
    phase: &str,
    text: &str,
) -> Result<Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let completed = json!({
        "id": item_id,
        "type": "message",
        "role": "assistant",
        "status": "completed",
        "phase": phase,
        "content": [{ "type": "output_text", "text": text }]
    });
    send_json(
        socket,
        json!({
            "type": "response.output_item.added",
            "output_index": output_index,
            "item": {
                "id": item_id,
                "type": "message",
                "role": "assistant",
                "status": "in_progress",
                "phase": phase,
                "content": []
            }
        }),
    )
    .await?;
    send_json(
        socket,
        json!({
            "type": "response.output_text.delta",
            "output_index": output_index,
            "content_index": 0,
            "delta": text
        }),
    )
    .await?;
    send_json(
        socket,
        json!({
            "type": "response.output_item.done",
            "output_index": output_index,
            "item": completed.clone()
        }),
    )
    .await?;
    Ok(completed)
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

struct CapturedHttpRequest {
    stream: TcpStream,
    headers: String,
    body: Value,
}

async fn next_http_json(listener: &TcpListener) -> Result<CapturedHttpRequest> {
    let (mut stream, _) = listener.accept().await?;
    let mut bytes = Vec::with_capacity(4096);
    let header_end = loop {
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let read = stream.read_buf(&mut bytes).await?;
        if read == 0 {
            return Err(eyre!("HTTPS test client closed before request headers"));
        }
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec())?.to_ascii_lowercase();
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .map(str::trim)
        .ok_or_else(|| eyre!("HTTPS test request omitted Content-Length"))?
        .parse::<usize>()?;
    while bytes.len() - header_end < content_length {
        let read = stream.read_buf(&mut bytes).await?;
        if read == 0 {
            return Err(eyre!("HTTPS test client closed before request body"));
        }
    }
    let body = serde_json::from_slice(&bytes[header_end..header_end + content_length])?;
    Ok(CapturedHttpRequest {
        stream,
        headers,
        body,
    })
}

async fn send_http_final(mut stream: TcpStream, response_id: &str) -> Result<()> {
    let event = completed_response(
        response_id,
        &[json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "done" }]
        })],
    );
    let body = format!("data: {event}\n\ndata: [DONE]\n\n");
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

async fn send_http_unexpected_end(mut stream: TcpStream) -> Result<()> {
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        )
        .await?;
    stream.shutdown().await?;
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

async fn send_json<S>(socket: &mut WebSocketStream<S>, value: Value) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket.send(Message::Text(value.to_string().into())).await?;
    Ok(())
}

fn temporary_workspace(label: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "nanocodex-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
