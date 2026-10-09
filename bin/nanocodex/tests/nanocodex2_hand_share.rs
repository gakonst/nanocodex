//! CLI journey over HTTP with a synthetic service and account credential.
//! Backend ownership and revocation semantics are covered by service journeys.
use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn hand_share_cli_journey() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let app = Router::new()
        .route("/v1/account/hand-shares", get({
            let calls = calls.clone();
            move |headers: HeaderMap| { let calls = calls.clone(); async move {
                assert!(headers.get("authorization").is_some());
                calls.lock().unwrap().push("list".into());
                Json(json!({"data":[{"id":"share-1","machine_id":"machine-1","created_at":123,"revoked_at":null,"url":"must-not-list-bearer"}]}))
            }}
        }).post({
            let calls = calls.clone();
            move |headers: HeaderMap, Json(body): Json<Value>| { let calls = calls.clone(); async move {
                assert!(headers.get("authorization").is_some());
                calls.lock().unwrap().push(format!("create:{}", body["machine_id"].as_str().unwrap()));
                if body["machine_id"] == "denied" {
                    return (StatusCode::FORBIDDEN, Json(json!({"error":"private-server-detail"})));
                }
                if body["machine_id"] == "uncertain" {
                    return (StatusCode::BAD_GATEWAY, Json(json!({"error":"private-server-detail"})));
                }
                assert_eq!(body, json!({"machine_id":"machine-1"}));
                (StatusCode::CREATED, Json(json!({"id":"share-1","url":"https://managed.example/hand-share/00000000-0000-4000-8000-000000000001#token=nhs_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"})))
            }}
        }))
        .route("/v1/account/hand-shares/share-1", delete({
            let calls = calls.clone();
            move |headers: HeaderMap| { let calls = calls.clone(); async move {
                assert!(headers.get("authorization").is_some());
                calls.lock().unwrap().push("revoke".into());
                Json(json!({"revoked":true}))
            }}
        }))
        .route("/v1/account/hand-shares/redeem", post({
            let calls = calls.clone();
            move |headers: HeaderMap, Json(body): Json<Value>| { let calls = calls.clone(); async move {
                assert!(headers.get("authorization").is_some());
                assert_eq!(body, json!({"url":"https://managed.example/hand-share/00000000-0000-4000-8000-000000000001#token=nhs_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}));
                calls.lock().unwrap().push("redeem".into());
                Json(json!({"machine_id":"machine-1"}))
            }}
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = tempfile::tempdir().unwrap();
    for (args, expected) in [
        (
            vec!["create", "machine-1"],
            Some(
                json!({"id":"share-1","url":"https://managed.example/hand-share/00000000-0000-4000-8000-000000000001#token=nhs_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}),
            ),
        ),
        (
            vec!["list"],
            Some(
                json!({"data":[{"id":"share-1","machine_id":"machine-1","created_at":123,"revoked_at":null}]}),
            ),
        ),
        (
            vec![
                "redeem",
                "https://managed.example/hand-share/00000000-0000-4000-8000-000000000001#token=nhs_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ],
            Some(json!({"machine_id":"machine-1"})),
        ),
        (
            vec!["revoke", "share-1"],
            Some(json!({"status":"revoked","id":"share-1"})),
        ),
        (vec!["create", "denied"], None),
        (vec!["create", "uncertain"], None),
    ] {
        let output = run(&home, &origin, &args).await;
        let transcript = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        eprintln!(
            "hand-share {}: exit={} {transcript}",
            args[0], output.status
        );
        assert_eq!(output.status.success(), expected.is_some(), "{transcript}");
        assert!(!transcript.contains("private-server-detail"));
        assert!(!transcript.contains("must-not-list-bearer"));
        if let Some(expected) = expected {
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                expected
            );
        } else {
            assert!(transcript.contains("Hand share request failed"));
        }
    }
    for args in [
        vec![],
        vec!["create"],
        vec!["revoke"],
        vec!["redeem"],
        vec!["unknown"],
    ] {
        let output = run(&home, &origin, &args).await;
        assert!(!output.status.success());
    }
    assert_eq!(
        *calls.lock().unwrap(),
        [
            "create:machine-1",
            "list",
            "redeem",
            "revoke",
            "create:denied",
            "create:uncertain"
        ]
    );
    server.abort();
}
async fn run(home: &tempfile::TempDir, origin: &str, args: &[&str]) -> std::process::Output {
    tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex"))
        .env_clear()
        .env("HOME", home.path())
        .env("NANOCODEX_HOME", home.path())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("NANOCODEX_MANAGED_URL", origin)
        .env(
            "NANOCODEX_API_KEY",
            format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)),
        )
        .arg("hand-share")
        .args(args)
        .output()
        .await
        .unwrap()
}
