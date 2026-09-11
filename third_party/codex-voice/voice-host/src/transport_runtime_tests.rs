//! Exercise admission and slot reuse with real TCP sockets across multiple listeners.

use super::*;
use pretty_assertions::assert_eq;
use tokio::time::timeout;

#[tokio::test]
async fn caps_inbound_streams_and_reuses_slots_across_listeners() {
    timeout(Duration::from_secs(/*secs*/ 10), async {
        let runtime = VoiceRuntime::default();
        let first = runtime
            .wrap_tcp_listener(std::net::TcpListener::bind("127.0.0.1:0").unwrap())
            .unwrap();
        let second = runtime
            .wrap_tcp_listener(std::net::TcpListener::bind("127.0.0.1:0").unwrap())
            .unwrap();
        let mut clients = Vec::new();
        let mut streams = Vec::new();
        for _ in 0..MAX_INBOUND_STREAMS {
            clients.push(
                runtime
                    .connect_tcp(first.local_addr().unwrap())
                    .await
                    .unwrap(),
            );
            streams.push(first.accept().await.unwrap().0);
        }
        // A second listener shares the budget; rejecting a connection closes its socket.
        let rejected = runtime
            .connect_tcp(second.local_addr().unwrap())
            .await
            .unwrap();
        assert_eq!(
            second.accept().await.unwrap_err().to_string(),
            "voice TCP connection limit reached"
        );
        assert_eq!(rejected.read(&mut [0]).await.unwrap(), 0);
        assert_eq!(runtime.accepted.lock().unwrap().len(), MAX_INBOUND_STREAMS);

        drop(streams.pop());
        let replacement = runtime
            .connect_tcp(second.local_addr().unwrap())
            .await
            .unwrap();
        let (accepted, _) = second.accept().await.unwrap();
        replacement.write_all(b"x").await.unwrap();
        let mut byte = [0];
        assert_eq!(accepted.read(&mut byte).await.unwrap(), 1);
        assert_eq!(byte, *b"x");
        assert_eq!(runtime.accepted.lock().unwrap().len(), MAX_INBOUND_STREAMS);
        drop((clients, streams, accepted, replacement));
    })
    .await
    .unwrap();
}
