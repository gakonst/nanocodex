use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn server(responses: Vec<String>) -> (String, tokio::task::JoinHandle<usize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut count = 0;
        for response in responses {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
                .await
                .expect("expected account request before fixture deadline")
                .unwrap();
            let mut request = Vec::new();
            loop {
                let byte = tokio::time::timeout(Duration::from_secs(1), stream.read_u8())
                    .await
                    .expect("complete account request headers")
                    .unwrap();
                request.push(byte);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
                assert!(request.len() < 8192);
            }
            assert!(request.starts_with(b"GET /v1/me HTTP/1.1\r\n"));
            stream.write_all(response.as_bytes()).await.unwrap();
            count += 1;
        }
        count
    });
    (origin, task)
}
fn response(status: u16, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    )
}
fn client() -> Client {
    // Unit tests bypass ManagedClient startup, which normally installs TLS crypto.
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap()
}
#[tokio::test]
async fn permanent_failures_are_classified_without_response_body_or_credentials() {
    for (status, message) in [
        (401, "run nanocodex2 login"),
        (403, "check account permissions"),
        (302, "request rejected"),
    ] {
        let (origin, server) = server(vec![response(
            status,
            "x-request-id: test-123\r\n",
            "SECRET_RESPONSE",
        )])
        .await;
        let error = identify(&client(), &origin, "SECRET_KEY")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(message), "{error}");
        assert!(error.contains(&format!("status={status}")));
        assert!(error.contains("request_id=test-123"));
        assert!(error.contains("version="));
        assert!(error.contains("time="));
        assert!(!error.contains("SECRET"));
        assert!(!error.contains(&origin));
        if status != 401 {
            assert!(!error.contains("login"));
        }
        assert_eq!(server.await.unwrap(), 1);
    }
}
#[tokio::test]
async fn transient_failures_retry_then_recover() {
    let (origin, server) = server(vec![
        response(503, "Retry-After: 0\r\n", "private"),
        response(429, "Retry-After: 0\r\n", "private"),
        response(200, "", r#"{"user":{"id":"synthetic-user"}}"#),
    ])
    .await;
    assert_eq!(
        identify(&client(), &origin, "synthetic-key").await.unwrap()["user"]["id"],
        "synthetic-user"
    );
    assert_eq!(server.await.unwrap(), 3);
}
#[tokio::test]
async fn retry_budget_and_long_retry_after_are_respected() {
    for (status, delay, count) in [(503, "0", 3), (429, "60", 1)] {
        let (origin, server) = server(vec![
            response(
                status,
                &format!("Retry-After: {delay}\r\nx-request-id: unsafe/value\r\n"),
                "SECRET"
            );
            count
        ])
        .await;
        let error = identify(&client(), &origin, "key")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(&format!("attempts={count}")), "{error}");
        assert!(error.contains("request_id=unavailable"));
        assert!(!error.contains("login"));
        assert!(!error.contains("SECRET"));
        assert_eq!(server.await.unwrap(), count);
    }
}
#[tokio::test]
async fn transport_failure_is_bounded_and_does_not_suggest_login() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let error = identify(&client(), &origin, "key")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("status=transport"));
    assert!(error.contains("attempts=3"));
    assert!(!error.contains("login"));
    assert!(!error.contains(&origin));
}
#[test]
fn retry_after_accepts_http_dates_and_rejects_invalid_values() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "retry-after",
        (chrono::Utc::now() + chrono::Duration::seconds(60))
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string()
            .parse()
            .unwrap(),
    );
    assert!(retry_after(&headers).unwrap() > MAX_DELAY);
    headers.insert("retry-after", "invalid".parse().unwrap());
    assert_eq!(retry_after(&headers), None);
}

#[tokio::test]
async fn transient_exhaustion_has_specific_safe_diagnostics() {
    for (status, message) in [
        (429, "rate limited"),
        (500, "service unavailable"),
        (502, "service unavailable"),
        (503, "service unavailable"),
        (599, "service unavailable"),
    ] {
        let (origin, server) = server(vec![
            response(
                status,
                "Retry-After: 0\r\nx-request-id: unsafe/value\r\ncf-ray: fixture-123\r\n",
                "SECRET_BODY"
            );
            ATTEMPTS
        ])
        .await;
        let error = identify(&client(), &origin, "SECRET_KEY")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(message), "{error}");
        assert!(error.contains(&format!("status={status}")), "{error}");
        assert!(error.contains("attempts=3"), "{error}");
        assert!(error.contains("request_id=fixture-123"), "{error}");
        assert!(
            !error.contains("SECRET")
                && !error.contains("unsafe/value")
                && !error.contains(&origin)
        );
        assert!(!error.contains("login"));
        assert_eq!(server.await.unwrap(), ATTEMPTS);
    }
}

#[tokio::test]
async fn invalid_success_body_is_not_logged_or_retried() {
    let (origin, server) = server(vec![response(200, "", "SECRET_INVALID_JSON")]).await;
    let error = identify(&client(), &origin, "SECRET_KEY")
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Invalid Hand account response");
    assert_eq!(server.await.unwrap(), 1);
}
