//! Real executable and HTTP boundary with synthetic account receipts.
use axum::{Json, Router, http::StatusCode, routing::post};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[tokio::test]
async fn connectors_cli_projects_receipts_and_never_retries_failed_writes() {
    for status in [StatusCode::OK, StatusCode::BAD_GATEWAY] {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let app=Router::new().route("/v1/connectors/github",post(move || { let calls=observed.clone(); async move { calls.fetch_add(1,Ordering::SeqCst); (status,Json(serde_json::json!({"authorization_url":"https://github.com/login/oauth/authorize?state=synthetic","secret":"synthetic-private-value","error":"synthetic-private-value"}))) } }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let home = tempfile::tempdir().unwrap();
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex2"))
            .env_clear()
            .env("HOME", home.path())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("NANOCODEX_HOME", home.path())
            .env("NANOCODEX_MANAGED_URL", origin)
            .env(
                "NANOCODEX_API_KEY",
                format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)),
            )
            .args(["connectors", "start", "github"])
            .output()
            .await
            .unwrap();
        assert_eq!(output.status.success(), status.is_success());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let transcript = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!transcript.contains("synthetic-private-value"));
        if status.is_success() {
            assert!(transcript.contains("github.com"));
        } else {
            assert!(transcript.contains("Connector request failed"));
        }
        server.abort();
    }
}

#[tokio::test]
async fn whatsapp_cli_preserves_the_real_start_shape_but_never_pairing_material() {
    let operation = "11111111-1111-4111-8111-111111111111";
    let app=Router::new().route("/v1/connectors/whatsapp/start",post(move |Json(body):Json<serde_json::Value>| async move {
        assert_eq!(body["operation_id"],operation);
        Json(serde_json::json!({"connected":false,"state":"connecting","connection_id":"a".repeat(43),"attempt":{"operation_id":operation,"state":"ready","expires_at":1800000000000_u64},"code":"synthetic-private-pairing"}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let home = tempfile::tempdir().unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex2"))
        .env_clear()
        .env("HOME", home.path())
        .env("NANOCODEX_HOME", home.path())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("NANOCODEX_MANAGED_URL", origin)
        .env(
            "NANOCODEX_API_KEY",
            format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)),
        )
        .args([
            "connectors",
            "whatsapp-start",
            "+15555550123",
            "--operation-id",
            operation,
        ])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["attempt"]["operation_id"], operation);
    assert_eq!(receipt["attempt"]["state"], "ready");
    assert_eq!(receipt["connected"], false);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-private-pairing"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-private-pairing"));
    server.abort();
}
