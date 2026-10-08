//! Accepted SSE errors must not write reflected authorization into durable history.
#![cfg(all(feature = "claude", feature = "sqlite"))]
use axum::{Router, http::HeaderMap, routing::post};
use nanocodex_agent::{Nanocodex, PromptRequest};
use nanocodex_claude::{Claude, ClaudeClient};
use nanocodex_durability::{DurableAgentExt, DurableSession, SqliteStore};
use serde_json::json;

#[tokio::test]
async fn reflected_credential_in_accepted_sse_error_is_not_persisted() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let secret = "synthetic-sse-reflection-secret";
    let app = Router::new().route("/v1/messages", post(|headers: HeaderMap| async move {
        let echoed = headers.get("x-api-key").unwrap().to_str().unwrap();
        let frame = json!({"type":"error","error":{"type":"authentication_error","message":format!("gateway diagnostic: credential={echoed}")}});
        ([ ("content-type", "text/event-stream") ], format!("data: {frame}\n\n"))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let state = DurableSession::open(SqliteStore::open(&path).unwrap(), "redaction-session")
        .await
        .unwrap();
    let client = ClaudeClient::new(reqwest::Client::new(), endpoint, secret);
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .durability(state.clone())
        .await
        .unwrap()
        .build()
        .unwrap();
    let error = agent
        .prompt(PromptRequest::new("safe prompt").request_id("reflected-error"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err();
    let durable = state.state().await.unwrap();
    let serialized =
        serde_json::to_string(&durable.operation("reflected-error").unwrap().status).unwrap();
    eprintln!(
        "accepted SSE failure: {error}; credential present in durable state={}",
        serialized.contains(secret)
    );
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
    assert!(
        !serialized.contains(secret),
        "an accepted SSE error must not persist echoed credentials"
    );
    assert!(
        !error.to_string().contains(secret),
        "an accepted SSE error must redact echoed credentials"
    );
}

#[tokio::test]
async fn accepted_sse_error_scrubs_both_rejected_and_current_auth_generations() {
    use nanocodex_claude::{
        ClaudeAccessToken, ClaudeAuthFuture, ClaudeAuthUnavailable, ClaudeTokenSource,
        RefreshingClaudeAuth,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::{Duration, SystemTime};
    struct Source(AtomicUsize);
    impl ClaudeTokenSource for Source {
        fn refresh(
            &self,
        ) -> ClaudeAuthFuture<'_, Result<ClaudeAccessToken, ClaudeAuthUnavailable>> {
            Box::pin(async move {
                let generation = self.0.fetch_add(1, Ordering::SeqCst);
                ClaudeAccessToken::new(
                    if generation == 0 {
                        "synthetic-old-sse"
                    } else {
                        "synthetic-new-sse"
                    },
                    SystemTime::now() + Duration::from_secs(3600),
                )
            })
        }
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(AtomicUsize::new(0));
    let log = requests.clone();
    let app = Router::new().route("/v1/messages", post(move |headers: HeaderMap| {
        let log = log.clone();
        async move {
            let generation = log.fetch_add(1, Ordering::SeqCst);
            if generation == 0 {
                assert_eq!(headers["authorization"], "Bearer synthetic-old-sse");
                return (axum::http::StatusCode::UNAUTHORIZED, [("content-type", "text/plain")], "expired synthetic-old-sse".to_owned());
            }
            assert_eq!(headers["authorization"], "Bearer synthetic-new-sse");
            let frame = json!({"type":"error","error":{"type":"synthetic-old-sse","message":"gateway rejected Bearer synthetic-old-sse; accepted Bearer synthetic-new-sse; token=synthetic-new-sse"}});
            (axum::http::StatusCode::OK, [("content-type", "text/event-stream")], format!("data: {frame}\n\n"))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let state = DurableSession::open(SqliteStore::open(&path).unwrap(), "rotation-redaction")
        .await
        .unwrap();
    let auth = Arc::new(RefreshingClaudeAuth::new(
        Arc::new(Source(AtomicUsize::new(0))),
        Duration::ZERO,
    ));
    let client = ClaudeClient::with_auth_provider(reqwest::Client::new(), endpoint, auth);
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .durability(state.clone())
        .await
        .unwrap()
        .build()
        .unwrap();
    let error = agent
        .prompt(PromptRequest::new("safe prompt").request_id("rotated-error"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err();
    let durable = state.state().await.unwrap();
    let status =
        serde_json::to_string(&durable.operation("rotated-error").unwrap().status).unwrap();
    let diagnostic = format!("{error} {error:?}");
    agent.shutdown().await.unwrap();
    drop((agent, events));
    server.abort();
    assert_eq!(
        requests.load(Ordering::SeqCst),
        2,
        "one explicit HTTP401 recovery only"
    );
    for token in ["synthetic-old-sse", "synthetic-new-sse"] {
        assert!(
            !diagnostic.contains(token),
            "returned diagnostic reflects token generation"
        );
        assert!(
            !status.contains(token),
            "durable failure reflects token generation"
        );
    }
    assert!(
        diagnostic.contains("gateway"),
        "safe provider diagnostics remain useful"
    );
    eprintln!(
        "401 recovery then accepted SSE: requests=2; both token generations redacted from error and durable receipt"
    );
}
