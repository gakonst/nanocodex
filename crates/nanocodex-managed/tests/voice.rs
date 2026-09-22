#![cfg(feature = "voice")]
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use nanocodex_managed::{ManagedApiKey, ManagedClient};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
const SESSION: &str = "019fc927-b280-79a7-8445-1b9996ad2fb0";
const OPERATION: &str = "019fc927-b280-49a7-8445-1b9996ad2fb0";
#[tokio::test]
async fn retries_lifecycle_with_same_identity_and_account_authority() {
    let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let app = Router::new().route("/v1/agents/agent-test/realtime/start", post(|State(calls): State<Arc<Mutex<Vec<Value>>>>, headers: HeaderMap, Json(body): Json<Value>| async move {
        assert_eq!(headers["authorization"], "Bearer ncx_live_eeeeeeeeeeee_fffffffffffffffffffffffffffffffffffffffffff");
        assert!(headers.contains_key("x-nanocodex-client-context"));
        let mut calls = calls.lock().unwrap(); calls.push(body.clone());
        if calls.len() == 1 { (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":"retry"}))) }
        else { (StatusCode::OK, Json(json!({"voice_session_id":body["voice_session_id"],"operation_id":body["operation_id"],"context":{}}))) }
    })).with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ManagedClient::new(
        format!("http://{address}"),
        ManagedApiKey::parse("ncx_live_eeeeeeeeeeee_fffffffffffffffffffffffffffffffffffffffffff")
            .unwrap(),
    )
    .unwrap()
    .with_request_origin("nanocodex2", None, None)
    .unwrap();
    client
        .voice_operation(
            "agent-test",
            SESSION,
            "start",
            json!({"operation_id":OPERATION}),
        )
        .await
        .unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
    task.abort();
}
#[tokio::test]
async fn rejects_mismatched_receipt_and_invalid_paths_before_dispatch() {
    let app = Router::new().route(
        "/v1/agents/agent-test/realtime/start",
        post(|| async {
            Json(json!({"voice_session_id":SESSION,"operation_id":"another-operation"}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ManagedClient::new(
        format!("http://{address}"),
        ManagedApiKey::parse("ncx_live_eeeeeeeeeeee_fffffffffffffffffffffffffffffffffffffffffff")
            .unwrap(),
    )
    .unwrap();
    assert!(
        client
            .voice_operation(
                "agent-test",
                SESSION,
                "start",
                json!({"operation_id":OPERATION})
            )
            .await
            .is_err()
    );
    assert!(
        client
            .voice_operation(
                "../other",
                SESSION,
                "start",
                json!({"operation_id":OPERATION})
            )
            .await
            .is_err()
    );
    assert!(
        client
            .voice_operation(
                "agent-test",
                OPERATION,
                "start",
                json!({"operation_id":OPERATION})
            )
            .await
            .is_err()
    );
    assert!(
        client
            .voice_operation("agent-test", SESSION, "../delete", json!({}))
            .await
            .is_err()
    );
    assert!(
        client
            .voice_sideband("agent-test", SESSION, "rtc_invalid/path")
            .await
            .is_err()
    );
    task.abort();
}

#[tokio::test]
async fn native_call_and_sideband_keep_auth_and_bound_frames() {
    use axum::{
        extract::ws::{Message, WebSocketUpgrade},
        response::{IntoResponse, Response},
        routing::get,
    };
    use futures_util::StreamExt;
    let key = "ncx_live_eeeeeeeeeeee_fffffffffffffffffffffffffffffffffffffffffff";
    let app = Router::new()
        .route(
            "/v1/agents/agent-test/realtime/calls",
            post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(headers["x-nanocodex-voice-session-id"], SESSION);
                assert!(
                    headers["authorization"]
                        .to_str()
                        .unwrap()
                        .starts_with("Bearer ncx_live_")
                );
                assert_eq!(body["sdp"], "test-offer");
                Response::builder()
                    .header(
                        "x-nanocodex-realtime-location",
                        "/realtime/calls/rtc_test-1",
                    )
                    .body(axum::body::Body::from("test-answer"))
                    .unwrap()
            }),
        )
        .route(
            "/v1/agents/agent-test/realtime/sideband",
            get(|headers: HeaderMap, ws: WebSocketUpgrade| async move {
                assert!(
                    headers["authorization"]
                        .to_str()
                        .unwrap()
                        .starts_with("Bearer ncx_live_")
                );
                ws.on_upgrade(|mut socket| async move {
                    let message = socket.next().await.unwrap().unwrap();
                    assert_eq!(message.into_text().unwrap(), "{\"type\":\"test\"}");
                    socket
                        .send(Message::Text("{\"type\":\"session.started\"}".into()))
                        .await
                        .unwrap();
                })
                .into_response()
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ManagedClient::new(
        format!("http://{address}"),
        ManagedApiKey::parse(key).unwrap(),
    )
    .unwrap();
    let call = client
        .voice_call("agent-test", SESSION, "test-offer", json!({}))
        .await
        .unwrap();
    assert_eq!(call.sdp, "test-answer");
    assert_eq!(call.call_id, "rtc_test-1");
    let mut socket = client
        .voice_sideband("agent-test", SESSION, &call.call_id)
        .await
        .unwrap();
    assert!(socket.send(&"x".repeat(65_537)).await.is_err());
    socket.send("{\"type\":\"test\"}").await.unwrap();
    assert_eq!(socket.next().await.unwrap()["type"], "session.started");
    task.abort();
}
