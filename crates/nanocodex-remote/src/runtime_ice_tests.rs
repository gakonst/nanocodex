//! Real broker + HTTP boundary tests for publication-triggered ICE prefetch.
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
type Wire = tokio_tungstenite::WebSocketStream<TcpStream>;
async fn send_wire(wire: &mut Wire, value: Value) {
    wire.send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}
async fn read_wire(wire: &mut Wire) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(2), wire.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}
async fn fixture() -> (Publisher, Wire, TcpListener, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = PublisherTarget::from_attachment(
        &format!(
            "ws://{}/v1/account/tool-host",
            listener.local_addr().unwrap()
        ),
        "fixture-token",
    )
    .unwrap();
    let machine = Machine::new("fixture", "Fixture").unwrap();
    let backend: Backend =
        Arc::new(|_| Box::pin(async { Ok(json!({"status":"ok","width":1,"height":1})) }));
    let video: VideoSource = Arc::new(|| {
        Box::pin(async {
            Ok(crate::capture::Capture::packets(
                futures_util::stream::once(async {
                    Ok(bytes::Bytes::from_static(&[0, 0, 0, 1, 0x65, 1]))
                })
                .chain(futures_util::stream::pending()),
                None,
            ))
        })
    });
    let options = Options {
        video: Some(video),
        require_video: true,
        ..Options::default()
    };
    let peer = async {
        let (stream, _) = listener.accept().await.unwrap();
        let mut wire = tokio_tungstenite::accept_async(stream).await.unwrap();
        send_wire(&mut wire, json!({"type":"ready","connection_id":"fixture"})).await;
        let catalog = read_wire(&mut wire).await;
        assert_eq!(catalog["type"], "catalog");
        assert!(catalog["surfaces"][0].get("transport").is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "must not prefetch before publication"
        );
        send_wire(
            &mut wire,
            json!({"type":"published","generation":"fixture"}),
        )
        .await;
        // No viewer exists yet: only publication can have initiated this fetch.
        let (mut http, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(http.read_u8().await.unwrap());
            assert!(headers.len() < 8192);
        }
        let headers = String::from_utf8(headers).unwrap().to_lowercase();
        assert!(headers.starts_with("post /v1/account/hands/ice http/1.1\r\n"));
        assert!(headers.contains("\r\nauthorization: bearer fixture-token\r\n"));
        (wire, http)
    };
    let (publisher, (wire, http)) =
        tokio::join!(Publisher::start(&target, &machine, backend, options), peer);
    (publisher.unwrap(), wire, listener, http)
}

#[tokio::test]
async fn publication_prefetch_serves_multiple_real_viewer_offers_with_one_http_request() {
    let (publisher, mut wire, listener, mut http) = fixture().await;
    let body = json!({"iceServers":[],"expires_at":now_ms()+3_600_000}).to_string();
    http.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    drop(http);
    // Wait for each offer before admitting the next viewer, exercising both
    // pending-prefetch sharing and a subsequent cache hit through the runtime.
    for viewer in ["first", "second"] {
        send_wire(
            &mut wire,
            json!({"type":"viewer","viewer_id":viewer,"surface_id":"desktop"}),
        )
        .await;
        loop {
            let event = tokio::select! {
                biased;
                _ = listener.accept() => panic!("a cached viewer issued another HTTP request"),
                event = read_wire(&mut wire) => event,
            };
            if event["signal"]["type"] == "offer" {
                assert_eq!(event["viewer_id"], viewer);
                break;
            }
            assert_ne!(event["type"], "close_viewer");
        }
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
    publisher.shutdown().await.unwrap();
}

#[tokio::test]
async fn host_replacement_cancels_prefetch_and_does_not_reconnect() {
    use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};
    let (publisher, mut wire, listener, mut http) = fixture().await;
    wire.close(Some(CloseFrame {
        code: CloseCode::Normal,
        reason: "Host replaced".into(),
    }))
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !publisher.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), http.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
    publisher.shutdown().await.unwrap();
}
