use axum::{Json, Router, body::Bytes, extract::Path, http::HeaderMap, routing::post};
use nanocodex_managed::{ManagedApiKey, ManagedClient};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Observed {
    calls: Vec<(String, String, Bytes)>,
    created: Option<Value>,
}

#[tokio::test]
async fn fork_posts_empty_body_with_stable_key_and_replays_child_without_parent_turn() {
    let seen = Arc::new(Mutex::new(Observed::default()));
    let app = Router::new()
        .route(
            "/v1/agents/{parent}/forks",
            post({
                let seen = Arc::clone(&seen);
                move |Path(parent): Path<String>, headers: HeaderMap, body: Bytes| {
                    let seen = Arc::clone(&seen);
                    async move {
                        let key = headers
                            .get("idempotency-key")
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_owned();
                        let mut seen = seen.lock().unwrap();
                        seen.calls.push((parent, key, body));
                        let receipt = seen.created.get_or_insert_with(|| {
                            json!({
                                "agent_id": "child", "session_id": "child",
                                "parent_agent_id": "parent",
                                "events_url": "http://127.0.0.1/v1/agents/child/events",
                                "websocket_url": "ws://127.0.0.1/v1/agents/child/ws"
                            })
                        });
                        Json(receipt.clone())
                    }
                }
            }),
        )
        .fallback(|uri: axum::http::Uri| async move {
            eprintln!("unexpected non-fork request: {uri}");
            axum::http::StatusCode::NOT_FOUND
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let key =
        ManagedApiKey::parse(format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43))).unwrap();
    let client = ManagedClient::new(format!("http://{address}"), key).unwrap();
    let first = client.fork("parent", "fork-request-123").await.unwrap();
    let replay = client.fork("parent", "fork-request-123").await.unwrap();
    assert_eq!(first.agent_id, "child");
    assert_eq!(replay.agent_id, first.agent_id);
    assert_eq!(first.session_id, replay.session_id);
    assert_eq!(first.parent_agent_id.as_deref(), Some("parent"));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.calls.len(), 2);
    for (parent, key, body) in &seen.calls {
        assert_eq!(parent, "parent");
        assert_eq!(key, "fork-request-123");
        assert!(body.is_empty(), "fork request must have no JSON payload");
    }
    server.abort();
}

#[tokio::test]
async fn fork_rejects_unsafe_parent_and_key_before_http() {
    let key =
        ManagedApiKey::parse(format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43))).unwrap();
    let client = ManagedClient::new("http://127.0.0.1:1", key).unwrap();
    assert!(
        client
            .fork("../escape", "safe")
            .await
            .unwrap_err()
            .to_string()
            .contains("agent id")
    );
    assert!(
        client
            .fork("parent", "bad\nkey")
            .await
            .unwrap_err()
            .to_string()
            .contains("idempotency key")
    );
}

#[tokio::test]
async fn fork_rejects_a_response_that_reuses_the_parent_agent_id() {
    let app = Router::new().route(
        "/v1/agents/{parent}/forks",
        post(|| async {
            Json(json!({
                "agent_id": "parent", "session_id": "parent", "parent_agent_id": "parent",
                "events_url": "http://127.0.0.1/v1/agents/parent/events",
                "websocket_url": "ws://127.0.0.1/v1/agents/parent/ws"
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let key =
        ManagedApiKey::parse(format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43))).unwrap();
    let client = ManagedClient::new(format!("http://{address}"), key).unwrap();
    assert!(
        client
            .fork("parent", "safe-key")
            .await
            .unwrap_err()
            .to_string()
            .contains("separate child")
    );
    server.abort();
}
