use super::*;
use crate::preparation::Preparations;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
};

struct Request {
    reply: oneshot::Sender<(u16, String)>,
    disconnected: oneshot::Receiver<()>,
}
struct Server {
    url: Url,
    requests: mpsc::UnboundedReceiver<Request>,
    count: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!(
            "http://{}/publisher",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let (tx, requests) = mpsc::unbounded_channel();
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    headers.push(stream.read_u8().await.unwrap());
                    assert!(headers.len() < 8192);
                }
                let headers = String::from_utf8(headers).unwrap().to_lowercase();
                assert!(headers.starts_with("post /publisher/ice http/1.1\r\n"));
                assert!(headers.contains("\r\nauthorization: bearer synthetic-session-token\r\n"));
                observed.fetch_add(1, Ordering::SeqCst);
                let (reply, response) = oneshot::channel();
                let (closed, disconnected) = oneshot::channel();
                tx.send(Request {
                    reply,
                    disconnected,
                })
                .unwrap_or_else(|_| panic!("test receiver gone"));
                tokio::select! {
                    response = response => {
                        if let Ok((status, body)) = response {
                            stream.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                        }
                    }
                    eof = stream.read_u8() => {
                        assert!(eof.is_err());
                        let _ = closed.send(());
                    }
                }
            }
        });
        Self {
            url,
            requests,
            count,
            task,
        }
    }
    fn cache(&self) -> IceCache {
        let _ = rustls::crypto::ring::default_provider().install_default();
        IceCache::new(
            reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            self.url.clone(),
            "synthetic-session-token",
        )
    }
    async fn respond(&mut self, status: u16, body: Value) {
        self.requests
            .recv()
            .await
            .unwrap()
            .reply
            .send((status, body.to_string()))
            .unwrap();
    }
    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}
fn credentials() -> Value {
    json!({"iceServers":[{"urls":"turn:relay.example:3478", "username":"synthetic", "credential":"fixture"}],"expires_at":wall_ms()+3_600_000})
}
async fn fetch(cache: &mut IceCache, server: &mut Server, status: u16, body: Value) -> Response {
    let request = cache.request();
    let (response, ()) = tokio::join!(request, server.respond(status, body));
    response
}
fn snapshot(value: Value, received: Instant, wall_ms: u64) -> Fetched {
    Fetched {
        response: Ok(Arc::new(value)),
        received,
        wall_ms,
    }
}

#[test]
fn expiry_uses_both_monotonic_cap_and_absolute_wall_with_margin() {
    let now = Instant::now();
    let wall = 1_000_000;
    let cached = Cached::new(&snapshot(
        json!({"iceServers":[],"expires_at":wall+3_600_000}),
        now,
        wall,
    ))
    .unwrap();
    assert!(cached.valid(now + Duration::from_secs(299), wall));
    assert!(!cached.valid(now + MAX_REUSE, wall)); // wall rolled back: monotonic still bounds reuse
    assert!(cached.valid(now, wall + 3_539_999));
    assert!(!cached.valid(now, wall + 3_540_000)); // clock jumped forward
    assert_eq!(cached.refresh_at, now + Duration::from_secs(240));
    let short = Cached::new(&snapshot(
        json!({"iceServers":[],"expires_at":wall+90_000}),
        now,
        wall,
    ))
    .unwrap();
    assert_eq!(short.until, now + Duration::from_secs(30));
    assert!(!short.valid(now + Duration::from_secs(30), wall));
    assert_eq!(short.refresh_at, now + Duration::from_secs(24));
}

#[tokio::test]
async fn real_http_concurrent_viewers_share_one_authenticated_request_and_cache_it() {
    let mut server = Server::new().await;
    let mut cache = server.cache();
    let a = cache.request();
    let b = cache.request();
    let (a, b, ()) = tokio::join!(a, b, server.respond(200, credentials()));
    let a = a.unwrap();
    assert!(Arc::ptr_eq(&a, &b.unwrap()));
    // request() must settle a flight completed by a viewer before next() ran.
    assert!(Arc::ptr_eq(&a, &cache.request().await.unwrap()));
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn real_http_prefetch_is_reused_without_a_viewer_at_fetch_time() {
    let mut server = Server::new().await;
    let mut cache = server.cache();
    cache.prefetch();
    tokio::join!(cache.next(), server.respond(200, credentials()));
    assert_eq!(server.count(), 1);
    assert!(cache.request().await.is_ok());
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn missing_invalid_and_expired_expiry_and_invalid_configuration_are_not_cached() {
    let mut server = Server::new().await;
    let mut cache = server.cache();
    let now = wall_ms();
    let invalid = [
        json!(null),
        json!("9999999999999"),
        json!(-1),
        json!(now - 1),
        json!(now + 60_000),
        json!(2.5),
    ];
    let mut bodies = vec![
        json!({"iceServers":[]}),
        json!({"expires_at":now+3_600_000}),
    ];
    bodies.extend(
        invalid
            .into_iter()
            .map(|expiry| json!({"iceServers":[],"expires_at":expiry})),
    );
    for (index, body) in bodies.into_iter().enumerate() {
        assert!(fetch(&mut cache, &mut server, 200, body).await.is_ok());
        cache.settle();
        assert!(cache.cached.is_none());
        assert_eq!(server.count(), index + 1);
    }
}

#[tokio::test]
async fn http_and_decode_failures_are_not_shared_with_later_viewers() {
    let mut server = Server::new().await;
    let mut cache = server.cache();
    assert!(
        fetch(&mut cache, &mut server, 503, json!({}))
            .await
            .is_err()
    );
    let request = cache.request();
    let (failed, ()) = tokio::join!(request, async {
        server
            .requests
            .recv()
            .await
            .unwrap()
            .reply
            .send((200, "invalid json".into()))
            .unwrap();
    });
    assert!(failed.is_err());
    assert!(
        fetch(&mut cache, &mut server, 200, credentials())
            .await
            .is_ok()
    );
    assert_eq!(server.count(), 3);
}

#[tokio::test]
async fn failed_refresh_keeps_original_bounds_retries_once_and_never_serves_stale() {
    let mut server = Server::new().await;
    let mut cache = server.cache();
    let original = fetch(&mut cache, &mut server, 200, credentials())
        .await
        .unwrap();
    cache.settle();
    let until = cache.cached.as_ref().unwrap().until;
    cache.cached.as_mut().unwrap().refresh_at = Instant::now();
    cache.refresh();
    // Valid old credentials remain immediately usable during refresh.
    assert!(Arc::ptr_eq(&original, &cache.request().await.unwrap()));
    tokio::join!(cache.next(), server.respond(503, json!({})));
    assert_eq!(cache.cached.as_ref().unwrap().until, until);
    cache.refresh();
    assert!(cache.pending.is_none(), "retry must back off");
    assert!(cache.retry_at.unwrap() > Instant::now());
    cache.retry_at = Some(Instant::now());
    cache.refresh();
    tokio::join!(cache.next(), server.respond(503, json!({})));
    cache.retry_at = Some(Instant::now());
    cache.refresh();
    assert!(cache.pending.is_none(), "background retry is bounded");
    assert_eq!(server.count(), 3);
    cache.cached.as_mut().unwrap().until = Instant::now();
    let mut fresh = credentials();
    fresh["iceServers"][0]["username"] = json!("replacement");
    let response = fetch(&mut cache, &mut server, 200, fresh).await.unwrap();
    assert!(!Arc::ptr_eq(&response, &original));
    assert_eq!(server.count(), 4);
}

#[tokio::test]
async fn uncached_prefetch_failure_does_not_create_a_background_retry_loop() {
    let mut server = Server::new().await;
    let mut cache = server.cache();
    cache.prefetch();
    tokio::join!(cache.next(), server.respond(503, json!({})));
    cache.retry_at = Some(Instant::now());
    for _ in 0..10 {
        cache.refresh();
        assert!(cache.pending.is_none());
    }
    assert!(
        fetch(&mut cache, &mut server, 200, credentials())
            .await
            .is_ok()
    );
    assert_eq!(server.count(), 2);
}

#[tokio::test]
async fn queued_completed_response_cannot_extend_its_original_reuse_window() {
    let now = Instant::now();
    let fetched = snapshot(credentials(), now - MAX_REUSE, wall_ms());
    let cached = Cached::new(&fetched).unwrap();
    assert!(!cached.valid(now, wall_ms()));
}

#[tokio::test]
async fn viewer_removal_and_id_reuse_cannot_deliver_old_preparation() {
    let mut server = Server::new().await;
    let mut cache = server.cache();
    let mut preparations = Preparations::new();
    let old_deadline = Instant::now() + Duration::from_secs(7);
    preparations
        .insert("v", old_deadline, cache.request())
        .unwrap();
    let request = tokio::select! {
        _ = preparations.next() => panic!("HTTP is held"),
        request = server.requests.recv() => request.unwrap(),
    };
    preparations.remove("v");
    let deadline = Instant::now() + Duration::from_secs(8);
    preparations.insert("v", deadline, cache.request()).unwrap();
    preparations
        .insert("other", deadline, cache.request())
        .unwrap();
    request
        .reply
        .send((200, credentials().to_string()))
        .unwrap();
    let (id, actual, response) = preparations.next().await;
    assert_eq!(id, "v");
    assert_eq!(actual, deadline);
    assert!(response.unwrap().is_ok());
    assert_eq!(preparations.next().await.0, "other");
    assert!(preparations.is_empty());
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn original_viewer_deadline_is_not_extended_by_shared_prefetch() {
    let mut server = Server::new().await;
    let mut cache = server.cache();
    // Isolate the outer viewer deadline from the production HTTP limit of 5s.
    cache.http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    cache.prefetch();
    let mut preparations = Preparations::new();
    let deadline = Instant::now() + Duration::from_secs(8);
    preparations.insert("v", deadline, cache.request()).unwrap();
    let request = tokio::select! {
        _ = preparations.next() => panic!("HTTP is held"),
        request = server.requests.recv() => request.unwrap(),
    };
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(8)).await;
    let (_, actual, response) = preparations.next().await;
    assert_eq!(actual, deadline);
    assert!(response.is_err());
    drop(preparations);
    drop(cache);
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(1), request.disconnected)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn session_drop_cancels_inflight_http_even_with_multiple_viewer_waiters() {
    let mut server = Server::new().await;
    let mut cache = server.cache();
    cache.prefetch();
    let mut preparations = Preparations::new();
    for id in ["a", "b"] {
        preparations
            .insert(id, Instant::now() + Duration::from_secs(8), cache.request())
            .unwrap();
    }
    let request = tokio::select! {
        _ = cache.next() => panic!("HTTP is held"),
        request = server.requests.recv() => request.unwrap(),
    };
    drop(cache);
    drop(preparations);
    tokio::time::timeout(Duration::from_secs(1), request.disconnected)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.count(), 1);
}
