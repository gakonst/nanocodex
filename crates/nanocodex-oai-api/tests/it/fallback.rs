use std::num::NonZeroU32;

use eyre::{Result, eyre};
use futures_util::{SinkExt, StreamExt};
use nanocodex_oai_api::{OpenAi, transport::ResponsesTransport};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream, accept_async, accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
        http::StatusCode,
    },
};

const TURN_STATE_HEADER: &str = "x-codex-turn-state";

#[tokio::test]
async fn exhausted_websocket_budget_falls_back_to_sticky_https_full_replay() -> Result<()> {
    let websocket_listener = TcpListener::bind("127.0.0.1:0").await?;
    let websocket_url = format!("ws://{}", websocket_listener.local_addr()?);
    let http_listener = TcpListener::bind("127.0.0.1:0").await?;
    let api_base_url = format!("http://{}", http_listener.local_addr()?);

    let websocket_server = tokio::spawn(async move {
        let (stream, _) = websocket_listener.accept().await?;
        let mut first = accept_async(stream).await?;
        let initial = next_ws_json(&mut first).await?;
        assert!(initial.to_string().contains("fall back safely"));
        send_ws_json(
            &mut first,
            json!({
                "type": "response.metadata",
                "headers": { TURN_STATE_HEADER: "sticky-before-fallback" }
            }),
        )
        .await?;
        drop(first);

        let (stream, _) = websocket_listener.accept().await?;
        let mut second = accept_hdr_async(stream, |request: &Request, response: Response| {
            assert_eq!(
                request
                    .headers()
                    .get(TURN_STATE_HEADER)
                    .and_then(|value| value.to_str().ok()),
                Some("sticky-before-fallback")
            );
            Ok(response)
        })
        .await?;
        let replay = next_ws_json(&mut second).await?;
        assert!(replay.get("previous_response_id").is_none());
        assert!(replay.to_string().contains("fall back safely"));
        drop(second);
        Result::<()>::Ok(())
    });

    let http_server = tokio::spawn(async move {
        let fallback = read_http_json(&http_listener).await?;
        assert_eq!(
            turn_state(&fallback.headers).as_deref(),
            Some("sticky-before-fallback")
        );
        assert!(
            fallback.body.get("kind").is_none(),
            "HTTPS must not reuse the WebSocket response.create envelope"
        );
        assert!(fallback.body.get("previous_response_id").is_none());
        assert!(
            fallback.body["client_metadata"]
                .get("responses_lite")
                .is_none(),
            "HTTPS must not retain Responses Lite client metadata"
        );
        assert!(fallback.body.to_string().contains("fall back safely"));
        send_http_events(
            fallback.stream,
            None,
            [completed_response("resp-fallback", "fallback")],
        )
        .await?;

        let continuation = read_http_json(&http_listener).await?;
        assert_eq!(
            turn_state(&continuation.headers).as_deref(),
            Some("sticky-before-fallback")
        );
        assert!(
            continuation.body.get("previous_response_id").is_none(),
            "store:false HTTPS continuation must replay authoritative history"
        );
        let body = continuation.body.to_string();
        assert!(body.contains("fall back safely"));
        assert!(body.contains("fallback"));
        assert!(body.contains("stay on https"));
        send_http_events(
            continuation.stream,
            None,
            [completed_response("resp-sticky", "https")],
        )
        .await?;
        Result::<()>::Ok(())
    });

    let openai = OpenAi::builder("test-key")
        .websocket_url(websocket_url)
        .api_base_url(api_base_url)
        .max_attempts(NonZeroU32::new(2).unwrap())
        .build()?;
    let mut session = openai
        .instructions("Use the configured transport transparently.")
        .build()?;
    let mut turn = session.turn();
    assert_eq!(
        turn.create("fall back safely").await?.output_text(),
        "fallback"
    );
    assert_eq!(turn.create("stay on https").await?.output_text(), "https");

    timeout(std::time::Duration::from_secs(5), websocket_server)
        .await
        .map_err(|_| eyre!("mock WebSocket exhaustion server did not finish"))???;
    timeout(std::time::Duration::from_secs(5), http_server)
        .await
        .map_err(|_| eyre!("mock HTTPS fallback server did not finish"))???;
    Ok(())
}

#[tokio::test]
async fn explicit_https_never_probes_the_websocket_endpoint() -> Result<()> {
    let websocket_listener = TcpListener::bind("127.0.0.1:0").await?;
    let websocket_url = format!("ws://{}", websocket_listener.local_addr()?);
    let http_listener = TcpListener::bind("127.0.0.1:0").await?;
    let api_base_url = format!("http://{}", http_listener.local_addr()?);

    let http_server = tokio::spawn(async move {
        let request = read_http_json(&http_listener).await?;
        send_http_events(
            request.stream,
            None,
            [completed_response("resp-https", "https only")],
        )
        .await
    });

    let openai = OpenAi::builder("test-key")
        .transport(ResponsesTransport::Https)
        .websocket_url(websocket_url)
        .api_base_url(api_base_url)
        .build()?;
    let mut session = openai.instructions("Use HTTPS only.").build()?;
    assert_eq!(
        session.turn().create("answer").await?.output_text(),
        "https only"
    );
    timeout(std::time::Duration::from_secs(5), http_server)
        .await
        .map_err(|_| eyre!("mock explicit HTTPS server did not finish"))???;
    assert!(
        timeout(
            std::time::Duration::from_millis(100),
            websocket_listener.accept()
        )
        .await
        .is_err(),
        "explicit HTTPS unexpectedly probed the WebSocket endpoint"
    );
    Ok(())
}

#[tokio::test]
async fn upgrade_required_falls_back_without_another_websocket_attempt() -> Result<()> {
    let websocket_listener = TcpListener::bind("127.0.0.1:0").await?;
    let websocket_url = format!("ws://{}", websocket_listener.local_addr()?);
    let http_listener = TcpListener::bind("127.0.0.1:0").await?;
    let api_base_url = format!("http://{}", http_listener.local_addr()?);

    let websocket_server = tokio::spawn(async move {
        let (stream, _) = websocket_listener.accept().await?;
        let rejected = accept_hdr_async(stream, |_request: &Request, response: Response| {
            let mut response = response.map(|()| Some("upgrade required".to_owned()));
            *response.status_mut() = StatusCode::UPGRADE_REQUIRED;
            Err(response)
        })
        .await;
        assert!(rejected.is_err());
        assert!(
            timeout(
                std::time::Duration::from_millis(250),
                websocket_listener.accept()
            )
            .await
            .is_err(),
            "HTTP 426 should switch transports without another WebSocket attempt"
        );
        Result::<()>::Ok(())
    });
    let http_server = tokio::spawn(async move {
        let request = read_http_json(&http_listener).await?;
        assert!(request.body.get("previous_response_id").is_none());
        assert!(request.body.to_string().contains("switch immediately"));
        send_http_events(
            request.stream,
            None,
            [completed_response("resp-upgrade", "upgraded")],
        )
        .await
    });

    let openai = OpenAi::builder("test-key")
        .websocket_url(websocket_url)
        .api_base_url(api_base_url)
        .max_attempts(NonZeroU32::new(3).unwrap())
        .build()?;
    let mut session = openai
        .instructions("Recover from unsupported WebSocket transport.")
        .build()?;
    assert_eq!(
        session
            .turn()
            .create("switch immediately")
            .await?
            .output_text(),
        "upgraded"
    );
    timeout(std::time::Duration::from_secs(5), websocket_server)
        .await
        .map_err(|_| eyre!("mock 426 WebSocket server did not finish"))???;
    timeout(std::time::Duration::from_secs(5), http_server)
        .await
        .map_err(|_| eyre!("mock 426 fallback HTTP server did not finish"))???;
    Ok(())
}

#[tokio::test]
async fn forbidden_websocket_handshake_retries_then_falls_back_to_https() -> Result<()> {
    let websocket_listener = TcpListener::bind("127.0.0.1:0").await?;
    let websocket_url = format!("ws://{}", websocket_listener.local_addr()?);
    let http_listener = TcpListener::bind("127.0.0.1:0").await?;
    let api_base_url = format!("http://{}", http_listener.local_addr()?);

    let websocket_server = tokio::spawn(async move {
        for attempt in 1..=2 {
            let (stream, _) = websocket_listener.accept().await?;
            let cf_ray = format!("ray-{attempt}").parse()?;
            let rejected =
                accept_hdr_async(stream, move |_request: &Request, response: Response| {
                    let mut response = response.map(|()| Some(String::new()));
                    *response.status_mut() = StatusCode::FORBIDDEN;
                    response.headers_mut().insert("cf-ray", cf_ray);
                    Err(response)
                })
                .await;
            assert!(rejected.is_err());
        }
        assert!(
            timeout(
                std::time::Duration::from_millis(250),
                websocket_listener.accept()
            )
            .await
            .is_err(),
            "HTTP 403 recovery exceeded the configured WebSocket attempt budget"
        );
        Result::<()>::Ok(())
    });
    let http_server = tokio::spawn(async move {
        let request = read_http_json(&http_listener).await?;
        assert!(request.body.get("previous_response_id").is_none());
        assert!(
            request
                .body
                .to_string()
                .contains("recover forbidden upgrade")
        );
        send_http_events(
            request.stream,
            None,
            [completed_response("resp-forbidden", "recovered")],
        )
        .await
    });

    let openai = OpenAi::builder("test-key")
        .websocket_url(websocket_url)
        .api_base_url(api_base_url)
        .max_attempts(NonZeroU32::new(2).unwrap())
        .build()?;
    let mut session = openai
        .instructions("Recover transient WebSocket upgrade failures.")
        .build()?;
    assert_eq!(
        session
            .turn()
            .create("recover forbidden upgrade")
            .await?
            .output_text(),
        "recovered"
    );
    timeout(std::time::Duration::from_secs(5), websocket_server)
        .await
        .map_err(|_| eyre!("mock 403 WebSocket server did not finish"))???;
    timeout(std::time::Duration::from_secs(5), http_server)
        .await
        .map_err(|_| eyre!("mock 403 fallback HTTP server did not finish"))???;
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

struct CapturedHttpRequest {
    stream: TcpStream,
    headers: String,
    body: Value,
}

async fn read_http_json(listener: &TcpListener) -> Result<CapturedHttpRequest> {
    let (mut stream, _) = listener.accept().await?;
    let mut bytes = Vec::with_capacity(4_096);
    let header_end = loop {
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        if stream.read_buf(&mut bytes).await? == 0 {
            return Err(eyre!("HTTP request ended before its headers"));
        }
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec())?.to_ascii_lowercase();
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .map(str::trim)
        .ok_or_else(|| eyre!("HTTP request omitted Content-Length"))?
        .parse::<usize>()?;
    while bytes.len().saturating_sub(header_end) < content_length {
        if stream.read_buf(&mut bytes).await? == 0 {
            return Err(eyre!("HTTP request body ended early"));
        }
    }
    let body = serde_json::from_slice(&bytes[header_end..header_end + content_length])?;
    Ok(CapturedHttpRequest {
        stream,
        headers,
        body,
    })
}

fn turn_state(headers: &str) -> Option<String> {
    headers.lines().find_map(|line| {
        line.strip_prefix(TURN_STATE_HEADER)
            .and_then(|value| value.strip_prefix(':'))
            .map(str::trim)
            .map(str::to_owned)
    })
}

async fn send_http_events(
    mut stream: TcpStream,
    turn_state: Option<&str>,
    events: impl IntoIterator<Item = Value>,
) -> Result<()> {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    let turn_state = turn_state.map_or_else(String::new, |value| {
        format!("{TURN_STATE_HEADER}: {value}\r\n")
    });
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                 content-length: {}\r\n{turn_state}connection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await?;
    stream.shutdown().await?;
    Ok(())
}

fn completed_response(response_id: &str, text: &str) -> Value {
    json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": text }]
            }],
            "usage": null
        }
    })
}
