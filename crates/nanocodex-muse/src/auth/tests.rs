//! Public auth journeys through loopback HTTP; all credentials are synthetic.
use super::*;
use crate::{Model, Muse, Nanocodex, Tools, tools::ToolExposure};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

struct Reply {
    status: u16,
    body: String,
    headers: &'static str,
}
fn json(status: u16, value: Value) -> Reply {
    Reply {
        status,
        body: value.to_string(),
        headers: "Content-Type: application/json\r\n",
    }
}
fn answer() -> Reply {
    let value = serde_json::json!({"type":"response.completed","response":{
        "id":"synthetic-response","status":"completed","output":[{"type":"message","role":"assistant",
        "content":[{"type":"output_text","text":"hello"}]}],"usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}});
    Reply {
        status: 200,
        body: format!("data: {value}\n\ndata: [DONE]\n\n"),
        headers: "Content-Type: text/event-stream\r\n",
    }
}
async fn request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0; 4096];
        let length = stream.read(&mut buffer).await.unwrap();
        assert_ne!(length, 0);
        bytes.extend_from_slice(&buffer[..length]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            let content_length: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + content_length {
                break;
            }
        }
    }
    String::from_utf8(bytes).unwrap()
}
async fn serve(replies: Vec<Reply>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for reply in replies {
            let (mut stream, _) = timeout(Duration::from_secs(10), listener.accept())
                .await
                .unwrap()
                .unwrap();
            requests.push(request(&mut stream).await);
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {} Test\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                        reply.status,
                        reply.headers,
                        reply.body.len(),
                        reply.body
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
        requests
    });
    (origin, task)
}
fn device() -> Value {
    serde_json::json!({"device_code":"synthetic-device", "user_code":"TEST-CODE",
        "verification_uri":"https://auth.meta.com/device", "expires_in":30, "interval":1})
}
fn credentials() -> MuseCredential {
    MuseCredential {
        api_key: "synthetic-old-key".into(),
        access_token: Some("synthetic-oauth".into()),
    }
}
fn manager(origin: &str) -> MuseAuth {
    MuseAuth::with_endpoint(credentials(), format!("{origin}/muse-code/key")).unwrap()
}
async fn agent(auth: &MuseAuth, origin: &str, model: Model) -> Nanocodex {
    let provider = Muse::builder(auth.authorization())
        .model(model)
        .api_base_url(format!("{origin}/v1"))
        .build()
        .unwrap();
    let tools = Tools::builder()
        .without_defaults()
        .exposure(ToolExposure::CodeModeOnly)
        .build()
        .unwrap();
    Nanocodex::builder(provider).tools(tools).build().unwrap().0
}
async fn turn(agent: &Nanocodex) -> eyre::Result<String> {
    timeout(Duration::from_secs(10), async {
        Ok(agent
            .prompt("hello")
            .await?
            .result()
            .await?
            .final_message()
            .to_owned())
    })
    .await?
}

#[tokio::test]
async fn device_login_returns_both_credentials_without_a_store() {
    let (origin, server) = serve(vec![
        json(200, device()),
        json(400, serde_json::json!({"error":"authorization_pending"})),
        json(400, serde_json::json!({"error":"slow_down"})),
        json(
            200,
            serde_json::json!({"access_token":"synthetic-oauth", "refresh_token":"unused-refresh"}),
        ),
        json(
            200,
            serde_json::json!({"api_key":"synthetic-inference", "is_subs_active":true}),
        ),
    ])
    .await;
    let login = MuseLogin::start_with_endpoints(&origin, &origin)
        .await
        .unwrap();
    assert_eq!(login.verification_url(), "https://auth.meta.com/device");
    assert_eq!(login.user_code(), "TEST-CODE");
    assert!(!format!("{login:?}").contains("synthetic-device"));
    let started = Instant::now();
    let credentials = login.complete().await.unwrap();
    assert!(started.elapsed() >= Duration::from_secs(8));
    assert_eq!(credentials.access_token.as_deref(), Some("synthetic-oauth"));
    assert_eq!(credentials.api_key, "synthetic-inference");
    let serialized = serde_json::to_value(&credentials).unwrap();
    assert!(serialized.get("refresh_token").is_none());
    let restored: MuseCredential = serde_json::from_value(serialized).unwrap();
    let auth = MuseAuth::new(restored).unwrap();
    let snapshot = auth.authorization().snapshot().await.unwrap();
    assert_eq!(snapshot.bearer(), "synthetic-inference");
    assert!(!format!("{credentials:?}{auth:?}{snapshot:?}").contains("synthetic-"));
    let requests = server.await.unwrap();
    assert!(requests[0].contains("client_id=1031625952748946"));
    assert!(requests.iter().all(|request| {
        request.contains(concat!("user-agent: nanocodex/", env!("CARGO_PKG_VERSION")))
    }));
    assert!(requests[1].contains("device_code=synthetic-device"));
    assert!(requests[4].contains("Bearer synthetic-oauth"));
    assert!(requests[4].contains(r#"{"onboard":true}"#));
}

#[tokio::test]
async fn denied_expired_and_cancelled_login_return_no_credentials() {
    let (origin, server) = serve(vec![
        json(200, device()),
        json(400, serde_json::json!({"error":"access_denied"})),
    ])
    .await;
    let login = MuseLogin::start_with_endpoints(&origin, &origin)
        .await
        .unwrap();
    assert!(matches!(
        login.complete().await,
        Err(MuseAuthError::LoginRejected)
    ));
    server.await.unwrap();
    for expired in [false, true] {
        let (origin, server) = serve(vec![json(200, device())]).await;
        let mut login = MuseLogin::start_with_endpoints(&origin, &origin)
            .await
            .unwrap();
        if expired {
            login.deadline = Instant::now();
            assert!(matches!(
                login.complete().await,
                Err(MuseAuthError::LoginRejected)
            ));
        } else {
            assert!(
                timeout(Duration::from_millis(10), login.complete())
                    .await
                    .is_err()
            );
        }
        assert_eq!(server.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn responses_401_exchanges_once_retries_and_reuses_the_key_for_both_models() {
    for model in [
        crate::MuseModel::Spark.into(),
        crate::MuseModel::Contributor.into(),
    ] {
        let (origin, server) = serve(vec![
            json(401, serde_json::json!({"error":"expired"})),
            json(200, serde_json::json!({"api_key":"synthetic-new-key"})),
            answer(),
            answer(),
        ])
        .await;
        let auth = manager(&origin);
        let agent = agent(&auth, &origin, model).await;
        assert_eq!(turn(&agent).await.unwrap(), "hello");
        assert_eq!(turn(&agent).await.unwrap(), "hello");
        agent.shutdown().await.unwrap();
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("POST /v1/responses"));
        assert!(requests[0].contains("Bearer synthetic-old-key"));
        assert!(requests[1].starts_with("POST /muse-code/key"));
        assert!(requests[1].contains("Bearer synthetic-oauth"));
        assert!(requests[1].ends_with("{}"));
        for request in &requests[2..] {
            assert!(request.contains("Bearer synthetic-new-key"));
            assert!(!request.contains("synthetic-oauth"));
        }
        let credentials = auth.credentials().await;
        assert_eq!(credentials.api_key, "synthetic-new-key");
        assert_eq!(credentials.access_token.as_deref(), Some("synthetic-oauth"));
    }
}

#[tokio::test]
async fn concurrent_and_late_401s_share_one_exchange_even_if_meta_returns_the_same_key() {
    for key in ["synthetic-new-key", "synthetic-old-key"] {
        let (origin, server) = serve(vec![json(200, serde_json::json!({"api_key":key}))]).await;
        let auth = manager(&origin).authorization();
        let rejected = auth.snapshot().await.unwrap();
        let mut tasks = Vec::new();
        for _ in 0..24 {
            let auth = auth.clone();
            let rejected = rejected.clone();
            tasks.push(tokio::spawn(async move {
                auth.recover_unauthorized(&rejected).await
            }));
        }
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        auth.recover_unauthorized(&rejected).await.unwrap();
        let current = auth.snapshot().await.unwrap();
        assert_eq!(current.bearer(), key);
        assert_ne!(current.revision(), rejected.revision());
        assert_eq!(server.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn revoked_oauth_or_subscription_requires_login_without_an_exchange_storm() {
    for reply in [
        json(401, serde_json::json!({"error":"synthetic-oauth"})),
        json(403, serde_json::json!({"error":"synthetic-oauth"})),
        json(
            200,
            serde_json::json!({"api_key":"new-key", "is_subs_active":false}),
        ),
    ] {
        let (origin, server) = serve(vec![reply]).await;
        let manager = manager(&origin);
        let auth = manager.authorization();
        let snapshot = auth.snapshot().await.unwrap();
        for _ in 0..3 {
            let error = auth.recover_unauthorized(&snapshot).await.unwrap_err();
            let message = format!("{error}{error:?}");
            assert!(!message.contains("synthetic-"));
            assert!(message.contains("Muse login required") || message.contains("subscription"));
        }
        assert_eq!(manager.credentials().await.api_key, "synthetic-old-key");
        assert_eq!(server.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn exchange_backoff_honors_retry_after_and_preserves_the_previous_pair() {
    let mut limited = json(429, serde_json::json!({"error":"synthetic-oauth"}));
    limited.headers = "Content-Type: application/json\r\nRetry-After: 120\r\n";
    let (origin, server) = serve(vec![
        limited,
        json(200, serde_json::json!({"api_key":"synthetic-new-key"})),
    ])
    .await;
    let manager = manager(&origin);
    let auth = manager.authorization();
    let snapshot = auth.snapshot().await.unwrap();
    for _ in 0..3 {
        assert!(auth.recover_unauthorized(&snapshot).await.is_err());
    }
    assert_eq!(manager.credentials().await.api_key, "synthetic-old-key");
    {
        let mut state = manager.source.state.lock().await;
        let (_, deadline) = state.failure.as_mut().unwrap();
        assert!(deadline.unwrap().duration_since(Instant::now()) > Duration::from_secs(110));
        *deadline = Some(Instant::now());
    }
    auth.recover_unauthorized(&snapshot).await.unwrap();
    assert_eq!(manager.credentials().await.api_key, "synthetic-new-key");
    assert_eq!(server.await.unwrap().len(), 2);
}

#[tokio::test]
async fn cancelled_exchange_keeps_both_previous_credentials_and_unlocks_the_manager() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let manager = manager(&format!("http://{}", listener.local_addr().unwrap()));
    let auth = manager.authorization();
    let snapshot = auth.snapshot().await.unwrap();
    let recovery = tokio::spawn(async move { auth.recover_unauthorized(&snapshot).await });
    let (mut stream, _) = listener.accept().await.unwrap();
    request(&mut stream).await;
    recovery.abort();
    assert!(recovery.await.unwrap_err().is_cancelled());
    let credentials = timeout(Duration::from_secs(1), manager.credentials())
        .await
        .unwrap();
    assert_eq!(credentials.api_key, "synthetic-old-key");
    assert_eq!(credentials.access_token.as_deref(), Some("synthetic-oauth"));
}

#[tokio::test]
async fn api_key_only_auth_works_but_cannot_exchange_a_rejected_key() {
    let credentials: MuseCredential =
        serde_json::from_value(serde_json::json!({"api_key":"synthetic-old-key"})).unwrap();
    let manager = MuseAuth::new(credentials).unwrap();
    let auth = manager.authorization();
    let snapshot = auth.snapshot().await.unwrap();
    assert!(
        auth.recover_unauthorized(&snapshot)
            .await
            .unwrap_err()
            .to_string()
            .contains("Muse login required")
    );
    let (origin, server) = serve(vec![answer()]).await;
    let agent = agent(&manager, &origin, crate::MuseModel::Spark.into()).await;
    assert_eq!(turn(&agent).await.unwrap(), "hello");
    agent.shutdown().await.unwrap();
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn responses_retry_is_bounded_and_other_rejections_do_not_exchange() {
    for status in [401, 403] {
        let mut replies = vec![json(status, serde_json::json!({"error":"rejected"}))];
        if status == 401 {
            replies.push(json(
                200,
                serde_json::json!({"api_key":"synthetic-new-key"}),
            ));
            replies.push(json(401, serde_json::json!({"error":"still rejected"})));
        }
        let (origin, server) = serve(replies).await;
        let auth = manager(&origin);
        let agent = agent(&auth, &origin, crate::MuseModel::Spark.into()).await;
        assert!(turn(&agent).await.is_err());
        agent.shutdown().await.unwrap();
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), if status == 401 { 3 } else { 1 });
    }
}

#[tokio::test]
async fn invalid_exchange_results_preserve_credentials_and_redact_errors() {
    for reply in [
        json(500, serde_json::json!({"error":"synthetic-oauth"})),
        json(200, serde_json::json!({"api_key":""})),
        json(200, serde_json::json!({"api_key":"invalid\nkey"})),
    ] {
        let (origin, server) = serve(vec![reply]).await;
        let manager = manager(&origin);
        let auth = manager.authorization();
        let rejected = auth.snapshot().await.unwrap();
        for _ in 0..2 {
            let error = auth.recover_unauthorized(&rejected).await.unwrap_err();
            assert!(!format!("{error:?}{error}").contains("synthetic-oauth"));
        }
        assert_eq!(manager.credentials().await.api_key, "synthetic-old-key");
        assert_eq!(server.await.unwrap().len(), 1);
    }
}
