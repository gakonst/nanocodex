use super::*;
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        handshake::server::{Request, Response},
        http::StatusCode,
    },
};

#[tokio::test]
async fn upgrade_required_warmup_moves_generation_to_https() -> Result<()> {
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
            "warmup fallback attempted another WebSocket connection"
        );
        Result::<()>::Ok(())
    });
    let http_server = tokio::spawn(async move {
        let (stream, request) = read_http_json(&http_listener).await?;
        assert!(request.get("kind").is_none());
        assert!(request.get("previous_response_id").is_none());
        assert!(
            request["client_metadata"].get("responses_lite").is_none(),
            "fallback generation retained WebSocket-only metadata"
        );
        assert!(request.to_string().contains("finish over https"));
        send_http_completion(stream, "resp-final").await
    });

    let workspace = temporary_workspace("warmup-https-fallback")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(websocket_url)
        .api_base_url(api_base_url)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let turn = agent.prompt("finish over https").await?;
    drop(agent);
    let mut output = Vec::new();
    let (event_result, turn_result) = tokio::join!(events.write_jsonl(&mut output), turn.result());
    event_result?;
    let completed = turn_result?;
    assert_eq!(completed.final_message(), "done");

    timeout(std::time::Duration::from_secs(5), websocket_server)
        .await
        .map_err(|_| eyre!("mock warmup 426 server did not finish"))???;
    timeout(std::time::Duration::from_secs(5), http_server)
        .await
        .map_err(|_| eyre!("mock fallback HTTPS server did not finish"))???;
    assert!(String::from_utf8(output)?.contains("\"model.warmup.failed\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn cancellation_preserves_session_scoped_https_fallback() -> Result<()> {
    let websocket_listener = TcpListener::bind("127.0.0.1:0").await?;
    let websocket_url = format!("ws://{}", websocket_listener.local_addr()?);
    let http_listener = TcpListener::bind("127.0.0.1:0").await?;
    let api_base_url = format!("http://{}", http_listener.local_addr()?);
    let (first_http_seen, first_http_seen_rx) = tokio::sync::oneshot::channel();
    let (cancelled, cancelled_rx) = tokio::sync::oneshot::channel();
    let (follow_up_done, follow_up_done_rx) = tokio::sync::oneshot::channel();

    let websocket_server = tokio::spawn(async move {
        let (stream, _) = websocket_listener.accept().await?;
        let rejected = accept_hdr_async(stream, |_request: &Request, response: Response| {
            let mut response = response.map(|()| Some("upgrade required".to_owned()));
            *response.status_mut() = StatusCode::UPGRADE_REQUIRED;
            Err(response)
        })
        .await;
        assert!(rejected.is_err());
        follow_up_done_rx
            .await
            .map_err(|_| eyre!("follow-up completion signal dropped"))?;
        assert!(
            timeout(
                std::time::Duration::from_millis(250),
                websocket_listener.accept()
            )
            .await
            .is_err(),
            "cancellation rebuilt a session that probed WebSocket again"
        );
        Result::<()>::Ok(())
    });
    let http_server = tokio::spawn(async move {
        let (first_stream, first) = read_http_json(&http_listener).await?;
        assert!(first.get("previous_response_id").is_none());
        assert!(first.to_string().contains("cancel over https"));
        first_http_seen
            .send(())
            .map_err(|()| eyre!("first HTTPS signal receiver dropped"))?;
        cancelled_rx
            .await
            .map_err(|_| eyre!("cancellation signal sender dropped"))?;
        drop(first_stream);

        let (stream, follow_up) = read_http_json(&http_listener).await?;
        assert!(
            follow_up.get("previous_response_id").is_none(),
            "a cancelled provider request must force authoritative replay"
        );
        let replay = follow_up.to_string();
        assert!(replay.contains("cancel over https"));
        assert!(replay.contains("<turn_aborted>"));
        assert!(replay.contains("continue on https"));
        send_http_completion(stream, "resp-follow-up").await
    });

    let workspace = temporary_workspace("cancel-https-fallback")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(websocket_url)
        .api_base_url(api_base_url)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let interrupted = agent.prompt("cancel over https").await?;
    first_http_seen_rx
        .await
        .map_err(|_| eyre!("first HTTPS request was not observed"))?;
    interrupted.cancel().await?;
    assert!(matches!(
        interrupted.result().await,
        Err(NanocodexError::TurnCancelled)
    ));
    cancelled
        .send(())
        .map_err(|()| eyre!("cancellation signal receiver dropped"))?;
    assert_eq!(
        agent
            .prompt("continue on https")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    follow_up_done
        .send(())
        .map_err(|()| eyre!("follow-up completion receiver dropped"))?;
    agent.shutdown().await?;
    drop((agent, events));

    timeout(std::time::Duration::from_secs(5), websocket_server)
        .await
        .map_err(|_| eyre!("mock fallback WebSocket server did not finish"))???;
    timeout(std::time::Duration::from_secs(5), http_server)
        .await
        .map_err(|_| eyre!("mock fallback HTTPS server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

async fn read_http_json(listener: &TcpListener) -> Result<(TcpStream, Value)> {
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
    let headers = String::from_utf8(bytes[..header_end].to_vec())?;
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::trim)
                .map(str::to_owned)
        })
        .ok_or_else(|| eyre!("HTTP request omitted Content-Length"))?
        .parse::<usize>()?;
    while bytes.len().saturating_sub(header_end) < content_length {
        if stream.read_buf(&mut bytes).await? == 0 {
            return Err(eyre!("HTTP request body ended early"));
        }
    }
    let request = serde_json::from_slice(&bytes[header_end..header_end + content_length])?;
    Ok((stream, request))
}

async fn send_http_completion(mut stream: TcpStream, response_id: &str) -> Result<()> {
    let event = completed_response(
        response_id,
        &[json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "done" }]
        })],
    );
    let body = format!("data: {event}\n\ndata: [DONE]\n\n");
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await?;
    stream.shutdown().await?;
    Ok(())
}
