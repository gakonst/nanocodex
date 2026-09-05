use eyre::{Result, eyre};
use nanocodex_oai_api::{OpenAi, transport::ResponsesTransport};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

const TURN_STATE_HEADER: &str = "x-codex-turn-state";

#[tokio::test]
async fn https_turn_state_is_scoped_to_one_logical_turn_and_survives_retry() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let api_base_url = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let first = read_http_json(&listener).await?;
        assert!(first.body.get("previous_response_id").is_none());
        let mut observed = vec![turn_state(&first.headers)];
        send_http_events(
            first.stream,
            Some("sticky-turn-1"),
            [completed_response("resp-first", "first")],
        )
        .await?;

        let continuation = read_http_json(&listener).await?;
        assert_eq!(
            continuation.body["previous_response_id"].as_str(),
            Some("resp-first")
        );
        assert!(
            !continuation.body.to_string().contains("sticky-turn-1"),
            "HTTPS turn state belongs in the private request header, not the JSON body"
        );
        observed.push(turn_state(&continuation.headers));
        send_http_status(continuation.stream, 500, "Internal Server Error").await?;

        let retry = read_http_json(&listener).await?;
        assert!(
            retry.body.get("previous_response_id").is_none(),
            "the SDK-owned retry must still switch to full-history replay"
        );
        assert!(!retry.body.to_string().contains("sticky-turn-1"));
        observed.push(turn_state(&retry.headers));
        send_http_events(
            retry.stream,
            None,
            [completed_response("resp-second", "second")],
        )
        .await?;

        let next_turn = read_http_json(&listener).await?;
        assert_eq!(
            next_turn.body["previous_response_id"].as_str(),
            Some("resp-second"),
            "starting a new logical turn must not clear provider continuation state"
        );
        observed.push(turn_state(&next_turn.headers));
        send_http_events(
            next_turn.stream,
            Some("sticky-turn-2"),
            [completed_response("resp-third", "third")],
        )
        .await?;
        Result::<Vec<Option<String>>>::Ok(observed)
    });

    let openai = OpenAi::builder("test-key")
        .transport(ResponsesTransport::Https)
        .store(true)
        .api_base_url(api_base_url)
        .build()?;
    let mut session = openai
        .instructions("Answer each request with one word.")
        .build()?;

    {
        let mut turn = session.turn();
        assert_eq!(turn.create("first").await?.output_text(), "first");
        assert_eq!(turn.create("continue").await?.output_text(), "second");
    }
    assert_eq!(
        session.turn().create("new turn").await?.output_text(),
        "third"
    );

    let observed = timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock HTTPS turn-state server did not finish"))???;
    assert_eq!(
        observed,
        [
            None,
            Some("sticky-turn-1".to_owned()),
            Some("sticky-turn-1".to_owned()),
            None,
        ]
    );
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
