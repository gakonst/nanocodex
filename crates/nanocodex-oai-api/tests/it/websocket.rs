use eyre::{Result, eyre};
use futures_util::{SinkExt, StreamExt, TryStreamExt};
use nanocodex_oai_api::{OpenAi, ResponseEvent};
use serde_json::{Value, json};
use tokio::{net::TcpListener, time::timeout};
use tokio_tungstenite::{
    WebSocketStream, accept_async, accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
    },
};

#[tokio::test]
async fn dropping_a_streamed_response_reconnects_before_the_next_request() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let websocket_url = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut first_socket = accept_async(stream).await?;
        let first_request = next_ws_json(&mut first_socket).await?;
        assert!(first_request.to_string().contains("Abandon this response."));
        send_ws_json(
            &mut first_socket,
            json!({
                "type": "response.metadata",
                "headers": { "x-codex-turn-state": "cancelled-turn-state" }
            }),
        )
        .await?;
        send_ws_json(
            &mut first_socket,
            json!({
                "type": "response.output_text.delta",
                "output_index": 0,
                "delta": "partial"
            }),
        )
        .await?;

        let reconnected =
            match timeout(std::time::Duration::from_secs(2), first_socket.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let second_request: Value = serde_json::from_str(text.as_str())?;
                    assert!(second_request.to_string().contains("Answer this response."));
                    send_ws_json(
                        &mut first_socket,
                        completed_response("resp-stale", "stale abandoned response"),
                    )
                    .await?;
                    false
                }
                Ok(_) => {
                    let (stream, _) = listener.accept().await?;
                    let mut second_socket =
                        accept_hdr_async(stream, |request: &Request, response: Response| {
                            assert_eq!(
                                request
                                    .headers()
                                    .get("x-codex-turn-state")
                                    .and_then(|value| value.to_str().ok()),
                                Some("cancelled-turn-state")
                            );
                            Ok(response)
                        })
                        .await?;
                    let second_request = next_ws_json(&mut second_socket).await?;
                    assert!(second_request.to_string().contains("Answer this response."));
                    send_ws_json(
                        &mut second_socket,
                        completed_response("resp-fresh", "fresh response"),
                    )
                    .await?;
                    true
                }
                Err(_) => return Err(eyre!("cancelled response left its WebSocket open and idle")),
            };

        Ok::<_, eyre::Report>(reconnected)
    });

    let openai = OpenAi::builder("test-api-key")
        .websocket_url(websocket_url)
        .build()?;
    let mut session = openai
        .instructions("Answer only the active request.")
        .build()?;
    let mut turn = session.turn();

    let mut abandoned = turn.create("Abandon this response.");
    assert!(matches!(
        abandoned.try_next().await?,
        Some(ResponseEvent::OutputTextDelta(delta)) if delta == "partial"
    ));
    drop(abandoned);

    let completed = turn.create("Answer this response.").await?;
    assert_eq!(completed.output_text(), "fresh response");
    assert!(
        timeout(std::time::Duration::from_secs(5), server)
            .await
            .map_err(|_| eyre!("mock WebSocket server did not finish"))???
    );
    Ok(())
}

#[tokio::test]
async fn reconnect_replays_the_turn_state_in_the_websocket_handshake() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let websocket_url = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut first_socket = accept_hdr_async(stream, |request: &Request, response: Response| {
            assert!(request.headers().get("x-codex-turn-state").is_none());
            Ok(response)
        })
        .await?;
        drop(next_ws_json(&mut first_socket).await?);
        send_ws_json(
            &mut first_socket,
            json!({
                "type": "response.metadata",
                "headers": { "x-codex-turn-state": "sticky-turn-state" }
            }),
        )
        .await?;
        drop(first_socket);

        let (stream, _) = listener.accept().await?;
        let mut second_socket =
            accept_hdr_async(stream, |request: &Request, response: Response| {
                assert_eq!(
                    request
                        .headers()
                        .get("x-codex-turn-state")
                        .and_then(|value| value.to_str().ok()),
                    Some("sticky-turn-state")
                );
                Ok(response)
            })
            .await?;
        let replay = next_ws_json(&mut second_socket).await?;
        assert_eq!(
            replay["client_metadata"]["x-codex-turn-state"],
            "sticky-turn-state"
        );
        send_ws_json(
            &mut second_socket,
            completed_response("resp-reconnected", "reconnected response"),
        )
        .await
    });

    let openai = OpenAi::builder("test-api-key")
        .websocket_url(websocket_url)
        .build()?;
    let mut session = openai
        .instructions("Retry safely after a transport replacement.")
        .build()?;
    let completed = session.turn().create("Reconnect this response.").await?;
    assert_eq!(completed.output_text(), "reconnected response");
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock WebSocket reconnect server did not finish"))???;
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

fn completed_response(response_id: &str, output_text: &str) -> Value {
    json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": output_text
                }]
            }],
            "usage": null
        }
    })
}
