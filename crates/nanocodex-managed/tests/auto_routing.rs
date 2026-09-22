use axum::{
    Router,
    body::Bytes,
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use nanocodex_managed::{ManagedApiKey, ManagedClient, ManagedError, Model};
use serde_json::json;

#[tokio::test]
async fn automatic_routing_uses_explicit_post_and_validates_receipt() {
    let api_key = format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43));
    let authorization = format!("Bearer {api_key}");
    let app = Router::new().route(
        "/v1/agents/{agent_id}/routing",
        post(
            move |Path(id): Path<String>, headers: HeaderMap, body: Bytes| {
                let authorization = authorization.clone();
                async move {
                    assert_eq!(
                        headers.get("authorization").unwrap(),
                        authorization.as_str()
                    );
                    assert!(body.is_empty());
                    if id == "existing-thread" {
                        return (StatusCode::CONFLICT, axum::Json(json!({
                        "error": "routing_requires_new_thread", "message": "already accepted"
                    }))).into_response();
                    }
                    axum::Json(json!({
                        "enabled": id != "invalid-receipt",
                        "model_routing": { "strategy": "direct" },
                        "settings": { "model": "gpt-6-sol", "thinking": "high",
                            "reasoning_mode": "standard", "fast_mode": false }
                    }))
                    .into_response()
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ManagedClient::new(
        format!("http://{address}"),
        ManagedApiKey::parse(api_key).unwrap(),
    )
    .unwrap();
    for _ in 0..2 {
        let receipt = client.enable_auto_routing("empty-thread").await.unwrap();
        assert!(receipt.enabled);
        assert_eq!(receipt.settings.model, Model::Sol);
    }
    assert!(matches!(
        client.enable_auto_routing("invalid-receipt").await,
        Err(ManagedError::InvalidResponse(_))
    ));
    assert!(
        client
            .enable_auto_routing("existing-thread")
            .await
            .unwrap_err()
            .to_string()
            .contains("routing_requires_new_thread")
    );
    assert!(client.enable_auto_routing("invalid/id").await.is_err());
    server.abort();
}

#[tokio::test]
async fn routing_status_reads_actual_provider_and_model_with_legacy_fallback() {
    use axum::routing::get;
    use nanocodex_managed::RouteProvider;
    let app = Router::new().route("/v1/agents/{agent_id}", get(|Path(id): Path<String>| async move {
        axum::Json(match id.as_str() {
            "legacy" => json!({"settings": {}}),
            "pending" => json!({"model_routing_enabled": true, "model_route": null}),
            "bad" => json!({"model_route": {"backend": "unknown", "model": "gpt-6-astra", "thinking": "high"}}),
            _ => json!({"model_routing_enabled": true, "model_route": {
                "backend": "vercel", "model": "@cf/zai-org/glm-5.3", "thinking": "high", "audit": {"ignored": true}
            }}),
        })
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ManagedClient::new(
        format!("http://{address}"),
        ManagedApiKey::parse(format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43))).unwrap(),
    )
    .unwrap();
    assert!(!client.routing_status("legacy").await.unwrap().enabled);
    let pending = client.routing_status("pending").await.unwrap();
    assert!(pending.enabled && pending.route.is_none());
    let chosen = client
        .routing_status("chosen")
        .await
        .unwrap()
        .route
        .unwrap();
    assert_eq!(chosen.model, Model::Glm53);
    assert_eq!(chosen.backend, RouteProvider::Vercel);
    assert_eq!(chosen.backend.label(), "Vercel");
    assert!(client.routing_status("bad").await.is_err());
    server.abort();
}
