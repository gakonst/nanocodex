use std::time::Duration;

use eyre::{Result, ensure};
use futures_util::{SinkExt, StreamExt, TryStreamExt};
use nanocodex_oai_api::{OpenAi, ResponseEvent, transport::ResponsesTransport};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[tokio::test]
async fn model_silence_does_not_restart_websocket_or_http_responses() -> Result<()> {
    for use_http in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (release, wait) = oneshot::channel();
        let delta = json!({
            "type": "response.output_text.delta", "output_index": 0, "delta": "before silence"
        })
        .to_string();
        let complete = json!({
            "type": "response.completed",
            "response": { "id": "resp-silent", "status": "completed", "output": [], "usage": null }
        })
        .to_string();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            if use_http {
                let mut request = Vec::new();
                loop {
                    ensure!(
                        stream.read_buf(&mut request).await? > 0,
                        "request ended early"
                    );
                    if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .unwrap()
                            .trim()
                            .parse()?;
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let first = format!("data: {delta}\n\n");
                let last = format!("data: {complete}\n\ndata: [DONE]\n\n");
                stream.write_all(format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{first}",
                    first.len() + last.len()
                ).as_bytes()).await?;
                wait.await?;
                stream.write_all(last.as_bytes()).await?;
            } else {
                let mut socket = accept_async(stream).await?;
                socket.next().await.unwrap()?;
                socket.send(Message::Text(delta.into())).await?;
                wait.await?;
                socket.send(Message::Text(complete.into())).await?;
            }
            Result::<()>::Ok(())
        });
        let builder = OpenAi::builder("test-api-key");
        let openai = if use_http {
            builder
                .transport(ResponsesTransport::Https)
                .api_base_url(format!("http://{address}"))
        } else {
            builder.websocket_url(format!("ws://{address}"))
        }
        .build()?;
        let mut session = openai.instructions("Allow quiet reasoning.").build()?;
        let mut turn = session.turn();
        let mut response = turn.create("Think for as long as needed.");
        assert!(matches!(
            response.try_next().await?,
            Some(ResponseEvent::OutputTextDelta(_))
        ));
        tokio::time::pause();
        let mut pending = std::pin::pin!(response.try_next());
        assert!(futures_util::poll!(&mut pending).is_pending());
        tokio::time::advance(Duration::from_secs(360)).await;
        assert!(
            futures_util::poll!(&mut pending).is_pending(),
            "silence must not fail or restart the response"
        );
        tokio::time::resume();
        release.send(()).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), pending).await??,
            Some(ResponseEvent::Completed { .. })
        ));
        tokio::time::timeout(Duration::from_secs(5), server).await???;
    }
    Ok(())
}
