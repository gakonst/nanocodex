#![allow(missing_docs)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use nanocodex_agent::{Nanocodex, Tools};
use nanocodex_oai_api::{
    anthropic::{
        ANTHROPIC_OAUTH_BETA, Anthropic, AnthropicAuth, AnthropicAuthError, AnthropicAuthFuture,
        AnthropicAuthMode, AnthropicAuthSnapshot, AnthropicAuthSource,
    },
    events::AgentEventKind,
    pricing::CostStatus,
};
use nanocodex_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult, contract::async_trait,
};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex,
};

#[derive(Clone, Debug, Default)]
struct Captured {
    target: String,
    headers: String,
    body: String,
}

async fn read_request(stream: &mut TcpStream) -> Option<Captured> {
    let mut request = Vec::new();
    let headers_end = loop {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        request.extend_from_slice(&chunk[..read]);
        if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = String::from_utf8_lossy(&request[..headers_end]).to_string();
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::trim)
                .and_then(|value| value.parse::<usize>().ok())
        })
        .unwrap_or_default();
    while request.len() < headers_end + content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
    }
    Some(Captured {
        target: headers.lines().next().unwrap_or_default().to_owned(),
        headers,
        body: String::from_utf8_lossy(&request[headers_end..]).to_string(),
    })
}

fn text_turn() -> String {
    [
        r#"{"type":"message_start","message":{"id":"msg_test","usage":{"input_tokens":11}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello from "}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Anthropic."}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
        r#"{"type":"message_stop"}"#,
    ]
    .into_iter()
    .map(|event| format!("event: x\ndata: {event}\n\n"))
    .collect()
}

fn tool_turn() -> String {
    [
        r#"{"type":"message_start","message":{"id":"msg_tool","usage":{"input_tokens":10}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"exec","input":{}}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"input\":\"const result = await tools.echo({ value: \\\"hello\\\" }); text(result);\"}"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}}"#,
        r#"{"type":"message_stop"}"#,
    ]
    .into_iter()
    .map(|event| format!("event: x\ndata: {event}\n\n"))
    .collect()
}

struct EchoTool {
    calls: Arc<AtomicU64>,
}

#[async_trait]
impl Tool for EchoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "echo",
            "Returns the supplied value.",
            json!({
                "type": "object",
                "properties": {"value": {"type": "string"}},
                "required": ["value"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        self.calls.fetch_add(1, Ordering::AcqRel);
        let input: serde_json::Value = input.decode_json()?;
        Ok(ToolOutput::text(
            input["value"].as_str().unwrap_or_default(),
        ))
    }
}

async fn write_response(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .expect("write mock response");
}

#[tokio::test]
async fn messages_tool_call_executes_and_replays_its_result() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
    let base = format!("http://{}/v1", listener.local_addr().expect("mock address"));
    let captured = Arc::new(Mutex::new(Vec::<Captured>::new()));
    let server_capture = Arc::clone(&captured);
    let server = tokio::spawn(async move {
        for response in [tool_turn(), text_turn()] {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let request = read_request(&mut stream).await.expect("complete request");
            server_capture.lock().await.push(request);
            write_response(&mut stream, "200 OK", "text/event-stream", &response).await;
        }
    });

    let workspace = tempfile::tempdir().expect("workspace");
    let client = Anthropic::builder(AnthropicAuth::api_key("test-anthropic-key"))
        .api_base_url(base)
        .build()
        .expect("Anthropic recipe");
    let echo_calls = Arc::new(AtomicU64::new(0));
    let tools = Tools::builder()
        .without_defaults()
        .tool(EchoTool {
            calls: Arc::clone(&echo_calls),
        })
        .build()
        .expect("tools");
    let (agent, _) = Nanocodex::builder(client)
        .workspace(workspace.path())
        .tools(tools)
        .build()
        .expect("agent");

    let result = agent
        .prompt("echo hello")
        .await
        .expect("prompt accepted")
        .result()
        .await
        .expect("tool turn");
    assert_eq!(result.final_message(), "Hello from Anthropic.");
    assert_eq!(echo_calls.load(Ordering::Acquire), 1);
    server.await.expect("mock server");

    let requests = captured.lock().await;
    assert_eq!(requests.len(), 2);
    let first: serde_json::Value = serde_json::from_str(&requests[0].body).expect("first request");
    assert_eq!(first["tools"][0]["name"], "exec");
    let second: serde_json::Value =
        serde_json::from_str(&requests[1].body).expect("continuation request");
    let messages = second["messages"].as_array().expect("messages");
    assert!(
        messages.iter().any(|message| {
            message["role"] == "assistant"
                && message["content"]
                    .as_array()
                    .is_some_and(|content| content.iter().any(|block| block["type"] == "tool_use"))
        }),
        "{second:#}"
    );
    assert!(
        messages.iter().any(|message| {
            message["role"] == "user"
                && message["content"].as_array().is_some_and(|content| {
                    content.iter().any(|block| {
                        block["type"] == "tool_result"
                            && block["tool_use_id"] == "toolu_1"
                            && block["content"]
                                .as_str()
                                .is_some_and(|text| text.contains("hello"))
                    })
                })
        }),
        "{second:#}"
    );
}

#[tokio::test]
async fn messages_stream_runs_through_the_owned_agent_without_a_warmup() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
    let base = format!("http://{}/v1", listener.local_addr().expect("mock address"));
    let captured = Arc::new(Mutex::new(Captured::default()));
    let server_capture = Arc::clone(&captured);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept request");
        let request = read_request(&mut stream).await.expect("complete request");
        *server_capture.lock().await = request;
        write_response(&mut stream, "200 OK", "text/event-stream", &text_turn()).await;
    });

    let workspace = tempfile::tempdir().expect("workspace");
    let client = Anthropic::builder(AnthropicAuth::api_key("test-anthropic-key"))
        .api_base_url(base)
        .build()
        .expect("Anthropic recipe");
    let (agent, mut events) = Nanocodex::builder(client)
        .workspace(workspace.path())
        .tools(Tools::default())
        .build()
        .expect("agent");

    let turn = agent.prompt("say hello").await.expect("prompt accepted");
    let result = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn timeout")
        .expect("turn result");
    server.await.expect("mock server");

    assert_eq!(result.final_message(), "Hello from Anthropic.");
    assert_eq!(result.usage().input_tokens(), 11);
    assert_eq!(result.usage().output_tokens(), 5);
    assert_eq!(result.usage().cost_status(), CostStatus::NotEstimated);
    assert_eq!(
        serde_json::to_value(result.snapshot()).expect("snapshot")["model"],
        "claude-opus-5"
    );

    let mut raw_types = Vec::new();
    let mut outbound_requests = 0;
    let mut attempts_started = 0;
    let mut started = None;
    let mut completed = None;
    while let Some(event) = events.recv().await {
        if event.kind == AgentEventKind::RunStarted {
            started = Some(
                serde_json::from_str::<serde_json::Value>(event.payload.get())
                    .expect("run.started payload"),
            );
        }
        if event.kind == AgentEventKind::ModelAttemptStarted {
            attempts_started += 1;
        }
        if event.kind == AgentEventKind::RunCompleted {
            completed = Some(
                serde_json::from_str::<serde_json::Value>(event.payload.get())
                    .expect("run.completed payload"),
            );
        }
        if event.kind == AgentEventKind::ApiEvent {
            let payload: serde_json::Value =
                serde_json::from_str(event.payload.get()).expect("api.event payload");
            if payload["direction"] == "outbound" {
                outbound_requests += 1;
            } else {
                raw_types.push(
                    payload["event"]["type"]
                        .as_str()
                        .expect("Anthropic event type")
                        .to_owned(),
                );
            }
        }
        if event.kind.is_terminal() {
            break;
        }
    }
    assert_eq!(
        raw_types,
        [
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );
    assert_eq!(outbound_requests, 1);
    assert_eq!(attempts_started, 1);
    let started = started.expect("run.started");
    assert_eq!(started["mode"], "anthropic_model");
    assert_eq!(started["model"], "claude-opus-5");
    assert_eq!(started["transport"], "responses_https_sse");
    assert_eq!(completed.expect("run.completed")["response_attempts"], 1);

    let captured = captured.lock().await;
    assert!(captured.target.starts_with("POST /v1/messages "));
    let headers = captured.headers.to_ascii_lowercase();
    assert!(headers.contains("x-api-key: test-anthropic-key"));
    assert!(headers.contains("anthropic-version: 2023-06-01"));
    assert!(!headers.contains("authorization:"));
    let body: serde_json::Value = serde_json::from_str(&captured.body).expect("request body");
    assert_eq!(body["model"], "claude-opus-5");
    assert_eq!(body["stream"], true);
    assert_eq!(body["messages"][0]["role"], "user");
    assert!(
        body["system"][0]["text"]
            .as_str()
            .is_some_and(|prompt| prompt.contains("You are Claude Code"))
    );
}

struct RotatingOAuth {
    revision: AtomicU64,
    recoveries: AtomicU64,
}

impl AnthropicAuthSource for RotatingOAuth {
    fn validate(&self) -> Result<(), AnthropicAuthError> {
        Ok(())
    }

    fn snapshot(
        &self,
    ) -> AnthropicAuthFuture<'_, Result<AnthropicAuthSnapshot, AnthropicAuthError>> {
        let revision = self.revision.load(Ordering::Acquire);
        Box::pin(async move {
            let token = if revision == 0 {
                "expired"
            } else {
                "refreshed"
            };
            Ok(AnthropicAuthSnapshot::new(
                AnthropicAuthMode::OAuth,
                token,
                Some(ANTHROPIC_OAUTH_BETA),
                revision,
            ))
        })
    }

    fn recover_unauthorized(
        &self,
        rejected: &AnthropicAuthSnapshot,
    ) -> AnthropicAuthFuture<'_, Result<(), AnthropicAuthError>> {
        let rejected_revision = rejected.revision();
        Box::pin(async move {
            if self.revision.load(Ordering::Acquire) == rejected_revision {
                self.revision.fetch_add(1, Ordering::AcqRel);
                self.recoveries.fetch_add(1, Ordering::AcqRel);
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn oauth_401_refreshes_once_and_replays_the_identical_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
    let base = format!("http://{}/v1", listener.local_addr().expect("mock address"));
    let captured = Arc::new(Mutex::new(Vec::<Captured>::new()));
    let server_capture = Arc::clone(&captured);
    let server = tokio::spawn(async move {
        for attempt in 0..2 {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let request = read_request(&mut stream).await.expect("complete request");
            server_capture.lock().await.push(request);
            if attempt == 0 {
                write_response(
                    &mut stream,
                    "401 Unauthorized",
                    "application/json",
                    r#"{"type":"error","error":{"type":"authentication_error"}}"#,
                )
                .await;
            } else {
                write_response(&mut stream, "200 OK", "text/event-stream", &text_turn()).await;
            }
        }
    });

    let source = Arc::new(RotatingOAuth {
        revision: AtomicU64::new(0),
        recoveries: AtomicU64::new(0),
    });
    let client = Anthropic::builder(AnthropicAuth::managed_oauth(source.clone()))
        .api_base_url(base)
        .build()
        .expect("Anthropic recipe");
    let workspace = tempfile::tempdir().expect("workspace");
    let (agent, _) = Nanocodex::builder(client)
        .workspace(workspace.path())
        .tools(Tools::default())
        .build()
        .expect("agent");

    let result = agent
        .prompt("say hello")
        .await
        .expect("prompt accepted")
        .result()
        .await
        .expect("refreshed turn");
    assert_eq!(result.final_message(), "Hello from Anthropic.");
    server.await.expect("mock server");

    let requests = captured.lock().await;
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .headers
            .to_ascii_lowercase()
            .contains("authorization: bearer expired")
    );
    assert!(
        requests[1]
            .headers
            .to_ascii_lowercase()
            .contains("authorization: bearer refreshed")
    );
    assert_eq!(requests[0].body, requests[1].body);
    assert_eq!(source.recoveries.load(Ordering::Acquire), 1);
}

const fn main() {}
