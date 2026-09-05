use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use eyre::{Result, eyre};
use futures_util::{SinkExt, StreamExt};
use nanocodex_oai_api::{
    OpenAi,
    auth::{
        OpenAiAuth, OpenAiAuthError, OpenAiAuthFuture, OpenAiAuthMode, OpenAiAuthSnapshot,
        OpenAiAuthSource,
    },
    transport::ResponsesTransport,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
    },
};

#[tokio::test]
async fn oauth_covers_https_create_compact_and_unauthorized_recovery() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let api_base_url = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let rejected = read_http_json(&listener).await?;
        assert_eq!(rejected.path, "/responses");
        assert_oauth_headers(&rejected.headers, "oauth-token-0");
        send_http_status(rejected.stream, 401, "Unauthorized").await?;

        let create = read_http_json(&listener).await?;
        assert_eq!(create.path, "/responses");
        assert_oauth_headers(&create.headers, "oauth-token-1");
        assert_eq!(create.body["store"], false);
        assert!(create.body.to_string().contains("Remember build req_7f3."));
        send_http_events(
            create.stream,
            [completed_response(
                "resp-create",
                assistant_output("remembered"),
            )],
        )
        .await?;

        let compact = read_http_json(&listener).await?;
        assert_eq!(compact.path, "/responses");
        assert_oauth_headers(&compact.headers, "oauth-token-1");
        assert_eq!(
            compact.body["input"]
                .as_array()
                .and_then(|input| input.last()),
            Some(&json!({ "type": "compaction_trigger" }))
        );
        send_http_events(
            compact.stream,
            [
                compaction_item("cmp-https", "encrypted-https-context"),
                completed_response("resp-compact", Vec::new()),
            ],
        )
        .await
    });

    let source = Arc::new(RotatingChatGptAuth::default());
    let openai = OpenAi::builder(OpenAiAuth::managed_chatgpt(source.clone()))
        .transport(ResponsesTransport::Https)
        .api_base_url(api_base_url)
        .build()?;
    let mut session = openai
        .instructions("Preserve exact build identifiers across turns.")
        .build()?;

    let created = session.turn().create("Remember build req_7f3.").await?;
    assert_eq!(created.output_text(), "remembered");
    session.turn().compact().await?;

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock HTTPS OAuth server did not finish"))???;
    assert_eq!(source.recoveries.load(Ordering::Relaxed), 1);
    Ok(())
}

#[tokio::test]
async fn oauth_covers_websocket_create_compact_and_unauthorized_recovery() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let websocket_url = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (mut rejected, _) = listener.accept().await?;
        let rejected_headers = read_http_head(&mut rejected).await?;
        assert_oauth_headers(&rejected_headers.to_ascii_lowercase(), "oauth-token-0");
        send_http_status(rejected, 401, "Unauthorized").await?;

        let (stream, _) = listener.accept().await?;
        let mut socket = accept_hdr_async(stream, |request: &Request, response: Response| {
            assert_eq!(
                request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer oauth-token-1")
            );
            assert_eq!(
                request
                    .headers()
                    .get("chatgpt-account-id")
                    .and_then(|value| value.to_str().ok()),
                Some("account-test")
            );
            assert_eq!(
                request
                    .headers()
                    .get("x-openai-fedramp")
                    .and_then(|value| value.to_str().ok()),
                Some("true")
            );
            Ok(response)
        })
        .await?;

        let create = next_ws_json(&mut socket).await?;
        assert!(create.to_string().contains("Remember deploy dep_9c2."));
        send_ws_json(
            &mut socket,
            completed_response("resp-create", assistant_output("remembered")),
        )
        .await?;

        let compact = next_ws_json(&mut socket).await?;
        assert_eq!(
            compact["input"].as_array().and_then(|input| input.last()),
            Some(&json!({ "type": "compaction_trigger" }))
        );
        send_ws_json(
            &mut socket,
            compaction_item("cmp-ws", "encrypted-websocket-context"),
        )
        .await?;
        send_ws_json(&mut socket, completed_response("resp-compact", Vec::new())).await
    });

    let source = Arc::new(RotatingChatGptAuth::default());
    let openai = OpenAi::builder(OpenAiAuth::managed_chatgpt(source.clone()))
        .websocket_url(websocket_url)
        .build()?;
    let mut session = openai
        .instructions("Preserve exact deployment identifiers across turns.")
        .build()?;

    let created = session.turn().create("Remember deploy dep_9c2.").await?;
    assert_eq!(created.output_text(), "remembered");
    session.turn().compact().await?;

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock WebSocket OAuth server did not finish"))???;
    assert_eq!(source.recoveries.load(Ordering::Relaxed), 1);
    Ok(())
}

#[derive(Default)]
struct RotatingChatGptAuth {
    revision: AtomicU64,
    recoveries: AtomicU64,
}

impl OpenAiAuthSource for RotatingChatGptAuth {
    fn validate(&self) -> std::result::Result<(), OpenAiAuthError> {
        Ok(())
    }

    fn snapshot(
        &self,
    ) -> OpenAiAuthFuture<'_, std::result::Result<OpenAiAuthSnapshot, OpenAiAuthError>> {
        let revision = self.revision.load(Ordering::Acquire);
        Box::pin(async move {
            Ok(OpenAiAuthSnapshot::new(
                OpenAiAuthMode::ChatGpt,
                format!("oauth-token-{revision}"),
                Some("account-test"),
                true,
                revision,
            ))
        })
    }

    fn recover_unauthorized(
        &self,
        rejected: &OpenAiAuthSnapshot,
    ) -> OpenAiAuthFuture<'_, std::result::Result<(), OpenAiAuthError>> {
        let rejected_revision = rejected.revision();
        Box::pin(async move {
            self.recoveries.fetch_add(1, Ordering::Relaxed);
            let _ = self.revision.compare_exchange(
                rejected_revision,
                rejected_revision.saturating_add(1),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            Ok(())
        })
    }
}

struct CapturedHttpRequest {
    stream: TcpStream,
    path: String,
    headers: String,
    body: Value,
}

async fn read_http_json(listener: &TcpListener) -> Result<CapturedHttpRequest> {
    let (mut stream, _) = listener.accept().await?;
    let mut bytes = Vec::with_capacity(4_096);
    let header_end = read_http_head_into(&mut stream, &mut bytes).await?;
    let headers = String::from_utf8(bytes[..header_end].to_vec())?.to_ascii_lowercase();
    let path = headers
        .lines()
        .next()
        .and_then(|line| line.split_ascii_whitespace().nth(1))
        .ok_or_else(|| eyre!("HTTP request omitted its path"))?
        .to_owned();
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .map(str::trim)
        .ok_or_else(|| eyre!("HTTP request omitted Content-Length"))?
        .parse::<usize>()?;
    while bytes.len().saturating_sub(header_end) < content_length {
        let read = stream.read_buf(&mut bytes).await?;
        if read == 0 {
            return Err(eyre!("HTTP request body ended early"));
        }
    }
    let body = serde_json::from_slice(&bytes[header_end..header_end + content_length])?;
    Ok(CapturedHttpRequest {
        stream,
        path,
        headers,
        body,
    })
}

async fn read_http_head(stream: &mut TcpStream) -> Result<String> {
    let mut bytes = Vec::with_capacity(2_048);
    let header_end = read_http_head_into(stream, &mut bytes).await?;
    Ok(String::from_utf8(bytes[..header_end].to_vec())?)
}

async fn read_http_head_into(stream: &mut TcpStream, bytes: &mut Vec<u8>) -> Result<usize> {
    loop {
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            return Ok(position + 4);
        }
        let read = stream.read_buf(bytes).await?;
        if read == 0 {
            return Err(eyre!("HTTP request ended before its headers"));
        }
    }
}

fn assert_oauth_headers(headers: &str, token: &str) {
    assert!(headers.contains(&format!("authorization: bearer {token}")));
    assert!(headers.contains("chatgpt-account-id: account-test"));
    assert!(headers.contains("x-openai-fedramp: true"));
    assert!(headers.contains("session-id:"));
    assert!(headers.contains("thread-id:"));
    assert!(headers.contains("x-client-request-id:"));
}

async fn send_http_status(mut stream: TcpStream, status: u16, reason: &str) -> Result<()> {
    stream
        .write_all(
            format!("HTTP/1.1 {status} {reason}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await?;
    stream.shutdown().await?;
    Ok(())
}

async fn send_http_events(
    mut stream: TcpStream,
    events: impl IntoIterator<Item = Value>,
) -> Result<()> {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await?;
    stream.shutdown().await?;
    Ok(())
}

async fn next_ws_json<S>(socket: &mut WebSocketStream<S>) -> Result<Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let message = socket
            .next()
            .await
            .ok_or_else(|| eyre!("WebSocket closed before the client request"))??;
        if let Message::Text(text) = message {
            return Ok(serde_json::from_str(text.as_str())?);
        }
    }
}

async fn send_ws_json<S>(socket: &mut WebSocketStream<S>, value: Value) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket.send(Message::Text(value.to_string().into())).await?;
    Ok(())
}

fn assistant_output(text: &str) -> Vec<Value> {
    vec![json!({
        "type": "message",
        "role": "assistant",
        "content": [{ "type": "output_text", "text": text }]
    })]
}

fn completed_response(response_id: &str, output: Vec<Value>) -> Value {
    json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "status": "completed",
            "output": output,
            "usage": null
        }
    })
}

fn compaction_item(item_id: &str, encrypted_content: &str) -> Value {
    json!({
        "type": "response.output_item.done",
        "item": {
            "id": item_id,
            "type": "compaction",
            "encrypted_content": encrypted_content
        }
    })
}
