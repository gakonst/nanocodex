//! Real CLI/HTTP journeys; all identities and request templates are synthetic.
use std::{
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Response, StatusCode},
    routing::{get, post},
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ID: &str = "abcdefghijklmnopqrstuv";
const PRIVATE: &str = "synthetic-secret-must-not-be-reflected";

fn command(home: &std::path::Path, origin: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_nanocodex"));
    command
        .env_clear()
        .current_dir(home)
        .env("HOME", home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("NANOCODEX_HOME", home)
        .env("NANOCODEX_DISABLE_HAND", "1")
        .env("NANOCODEX_COMPUTER", "off")
        .env("NANOCODEX_MANAGED_URL", origin)
        .env(
            "NANOCODEX_API_KEY",
            format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)),
        )
        .arg("vault")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn invoke(origin: &str, input: &str, from_file: bool) -> std::process::Output {
    let home = tempfile::tempdir().unwrap();
    let mut command = command(home.path(), origin);
    command.arg("request");
    if from_file {
        let file = home.path().join("request.json");
        std::fs::write(&file, input).unwrap();
        command.arg("--file").arg(file);
    } else {
        command.arg("--stdin").stdin(Stdio::piped());
    }
    let mut child = command.spawn().unwrap();
    if !from_file {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.as_bytes()).await.unwrap();
        stdin.shutdown().await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}

fn template() -> Value {
    json!({"vault_id": ID, "url": "https://example.com/authorized", "method": "POST",
        "headers": {"authorization": "Bearer {{NANOCODEX_VAULT_API_KEY}}"},
        "body": "public-payload"})
}

#[tokio::test]
async fn vault_cli_status_errors_privacy_and_single_dispatch() {
    let input = template();
    let cases = [
        (
            400,
            json!({"error":"invalid_vault_signing_placeholder"}).to_string(),
            false,
            "invalid_vault_signing_placeholder",
        ),
        (200, json!({"status":201,"ok":true}).to_string(), true, ""),
        (200, json!({"status":403,"ok":false}).to_string(), true, ""),
        (
            403,
            json!({"error":"forbidden"}).to_string(),
            false,
            "forbidden",
        ),
        (
            502,
            json!({"error":"vault_request_outcome_unknown"}).to_string(),
            false,
            "outcome_unknown",
        ),
        (
            503,
            json!({"error": PRIVATE, "message": PRIVATE}).to_string(),
            false,
            "outcome_unknown",
        ),
        (
            200,
            json!({"status":200,"ok":true,"password":PRIVATE}).to_string(),
            false,
            "outcome_unknown",
        ),
        (
            200,
            json!({"status":403,"ok":true}).to_string(),
            false,
            "outcome_unknown",
        ),
        (
            200,
            json!({"status":600,"ok":false}).to_string(),
            false,
            "outcome_unknown",
        ),
        (200, "x".repeat(4097), false, "outcome_unknown"),
        (200, PRIVATE.into(), false, "outcome_unknown"),
        (302, "".into(), false, "outcome_unknown"),
    ];
    for (http_status, body, success, expected_error) in cases {
        let expected_receipt = success.then(|| serde_json::from_str::<Value>(&body).unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let expected = input.clone();
        let app = Router::new()
            .route(
                "/v1/vault/request",
                post(
                    move |headers: HeaderMap, axum::Json(value): axum::Json<Value>| {
                        let count = count.clone();
                        let body = body.clone();
                        let expected = expected.clone();
                        async move {
                            count.fetch_add(1, Ordering::SeqCst);
                            assert_eq!(
                                headers["authorization"],
                                format!("Bearer ncx_live_{}_{}", "a".repeat(12), "b".repeat(43))
                            );
                            assert_eq!(value, expected);
                            Response::builder()
                                .status(http_status)
                                .header("location", "/must-not-follow")
                                .body(Body::from(body))
                                .unwrap()
                        }
                    },
                ),
            )
            .fallback(|| async {
                panic!("CLI followed a redirect or used a different endpoint");
                #[allow(unreachable_code)]
                StatusCode::INTERNAL_SERVER_ERROR
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let output = invoke(&origin, &input.to_string(), false).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1, "HTTP {http_status}");
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stderr.contains(PRIVATE) && !stdout.contains(PRIVATE));
        assert!(!stderr.contains("ncx_live_") && !stdout.contains("ncx_live_"));
        if success {
            let receipt: Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(receipt.as_object().unwrap().len(), 2);
            assert_eq!(Some(receipt), expected_receipt);
            assert!(stderr.is_empty());
        } else {
            assert!(stdout.is_empty());
            assert!(stderr.contains(expected_error), "{stderr}");
        }
        println!("vault CLI HTTP {http_status}: success={success}, dispatches=1, output projected");
        server.abort();
    }
}

#[tokio::test]
async fn vault_cli_file_signing_defaults_and_local_rejection() {
    let mut input = json!({"vault_id": ID, "url":"https://example.com/authorized", "headers":{
        "authorization":"Bearer {{NANOCODEX_VAULT_JWT}}"},
        "signing":{"algorithm":"ES256","jwt":{"header":{},"payload":{"aud":"public-audience"}}}});
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut expected = input.clone();
    expected["method"] = "GET".into();
    let app = Router::new().route(
        "/v1/vault/request",
        post(move |axum::Json(value): axum::Json<Value>| {
            let count = count.clone();
            let expected = expected.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                assert_eq!(value, expected);
                axum::Json(json!({"status":204,"ok":true}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let output = invoke(&origin, &input.to_string(), true).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"status":204,"ok":true})
    );
    input["signing"]["message"] = "incompatible".into();
    for invalid in [
        input.to_string(),
        PRIVATE.into(),
        json!({"vault_id":ID,"url":"https://example.com", "unexpected":PRIVATE}).to_string(),
        "x".repeat(96 * 1024 + 1),
    ] {
        let output = invoke(&origin, &invalid, true).await;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains(PRIVATE));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    println!(
        "vault CLI --file: ES256 JWT template transmitted with GET default; four invalid inputs rejected before dispatch"
    );
    server.abort();
}

#[tokio::test]
async fn vault_cli_dropped_connection_is_not_retried() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            let mut buffer = [0; 8192];
            let _ = socket.read(&mut buffer).await;
            socket.shutdown().await.unwrap();
        }
    });
    let output = invoke(&origin, &template().to_string(), false).await;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("outcome_unknown"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    println!("vault CLI dropped response: outcome_unknown, one connection, no retry");
    server.abort();
}

#[tokio::test]
async fn vault_cli_totp_template_is_broker_owned_and_status_only() {
    let input = json!({"vault_id": ID, "url": "https://example.com/verify", "method": "POST",
        "body_encoding": "json", "body": "{\"code\":\"{{NANOCODEX_VAULT_TOTP}}\"}"});
    let expected = input.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let app = Router::new().route(
        "/v1/vault/request",
        post(
            move |headers: HeaderMap, axum::Json(body): axum::Json<Value>| {
                let expected = expected.clone();
                let seen = seen.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    assert!(headers.contains_key("authorization"));
                    assert_eq!(body["vault_id"], expected["vault_id"]);
                    assert_eq!(body["body"], expected["body"]);
                    assert_eq!(body["body_encoding"], "json");
                    axum::Json(json!({"status": 204, "ok": true}))
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let output = invoke(&origin, &input.to_string(), false).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"status": 204, "ok": true})
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn vault_cli_ssh_targets_public_projection_and_sanitized_errors() {
    let target = json!({
        "reference": "example-server", "hostname": "server.example.com", "port": 22,
        "username": "operator", "host_key_sha256": format!("SHA256:{}", "a".repeat(43)),
        "public_key": "ssh-rsa AAAA"
    });
    let mut legacy = target.clone();
    legacy["reference"] = "legacy-server".into();
    legacy.as_object_mut().unwrap().remove("public_key");
    let expected = json!([target, legacy]);
    let mut targets = expected.clone();
    for entry in targets.as_array_mut().unwrap() {
        entry["private_key"] = PRIVATE.into();
        entry["unexpected"] = json!({"nested_secret": PRIVATE});
    }
    let cases = [
        (
            200,
            json!({"ssh": targets, "vault": [{"password": PRIVATE}], "token": PRIVATE}).to_string(),
            Some(expected),
            "",
        ),
        (200, json!({"ssh": []}).to_string(), Some(json!([])), ""),
        (
            403,
            json!({"error": PRIVATE, "message": PRIVATE}).to_string(),
            None,
            "vault_request_failed",
        ),
        (503, PRIVATE.into(), None, "vault_request_failed"),
        (
            200,
            json!({"ssh": [{"reference": PRIVATE}]}).to_string(),
            None,
            "invalid Vault metadata",
        ),
        (200, PRIVATE.into(), None, "invalid Vault metadata"),
    ];
    for (http_status, body, expected, expected_error) in cases {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let app = Router::new()
            .route(
                "/v1/credentials",
                get(move |headers: HeaderMap| {
                    let count = count.clone();
                    let body = body.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(
                            headers["authorization"],
                            format!("Bearer ncx_live_{}_{}", "a".repeat(12), "b".repeat(43))
                        );
                        Response::builder()
                            .status(http_status)
                            .body(Body::from(body))
                            .unwrap()
                    }
                }),
            )
            .fallback(|| async {
                panic!("SSH target listing used a non-public endpoint");
                #[allow(unreachable_code)]
                StatusCode::INTERNAL_SERVER_ERROR
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let home = tempfile::tempdir().unwrap();
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            command(home.path(), &origin).arg("ssh-targets").output(),
        )
        .await
        .unwrap()
        .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(output.status.success(), expected.is_some(), "{stderr}");
        assert!(!stdout.contains(PRIVATE) && !stderr.contains(PRIVATE));
        assert!(!stdout.contains("ncx_live_") && !stderr.contains("ncx_live_"));
        if let Some(expected) = expected {
            assert_eq!(serde_json::from_str::<Value>(&stdout).unwrap(), expected);
            assert!(stderr.is_empty());
        } else {
            assert!(stdout.is_empty());
            assert!(stderr.contains(expected_error), "{stderr}");
        }
        println!(
            "vault ssh-targets HTTP {http_status}: one authenticated public GET; exact public projection or sanitized failure"
        );
        server.abort();
    }
}

#[tokio::test]
async fn native_vault_management_commands_preserve_safe_receipts_and_exact_operations() {
    let calls = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let captured = calls.clone();
    let app = Router::new().fallback(move |request: axum::extract::Request| {
        let captured = captured.clone();
        async move {
            assert!(request.headers().contains_key("authorization"));
            let path = request.uri().path().to_owned();
            let method = request.method().clone();
            let bytes = axum::body::to_bytes(request.into_body(), 1024 * 96).await.unwrap();
            let body: Value = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
            captured.lock().unwrap().push(json!({"path":path,"method":method.as_str(),"body":body}));
            let response = match path.as_str() {
                "/v1/credentials" => json!({"vault":[{"id":ID,"kind":"api_key","name":"Synthetic service","api_key":PRIVATE}],"ssh":[],"openai":{"api_key":PRIVATE}}),
                "/v1/vault/store" => { assert_eq!(body["capture_id"],ID); assert_eq!(body["operation_id"],"11111111-1111-4111-8111-111111111111"); json!({"status":"saved","capture_id":ID,"vault_id":ID,"kind":"card","card_number":PRIVATE}) },
                "/v1/vault/card" => { assert_eq!(body["vault_id"],ID); json!({"status":"saved","vault_id":ID,"balance":4.25,"currency":"USD","freshness":"current","observed_at":1234,"password":PRIVATE}) },
                _ if method == axum::http::Method::DELETE => return Response::builder().status(204).body(Body::empty()).unwrap(),
                _ if path.starts_with("/v1/credentials/ssh/") => { assert_eq!(body["generate"],true); assert!(body.get("private_key").is_none()); return Response::builder().status(204).body(Body::empty()).unwrap(); },
                _ => panic!("unexpected endpoint"),
            };
            Response::builder().status(200).header("content-type","application/json").body(Body::from(response.to_string())).unwrap()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = tempfile::tempdir().unwrap();
    let journeys: Vec<Vec<&str>> = vec![
        vec!["list"],
        vec!["delete", "api_key", ID],
        vec![
            "store",
            ID,
            "--operation-id",
            "11111111-1111-4111-8111-111111111111",
        ],
        vec!["card", "balance", ID],
        vec![
            "ssh-save",
            "synthetic",
            "--hostname",
            "example.com",
            "--username",
            "synthetic",
            "--host-key-sha256",
            "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ],
        vec!["ssh-remove", "synthetic"],
    ];
    let mut evidence = Vec::new();
    for args in journeys {
        let output = command(home.path(), &origin)
            .args(&args)
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        let public = String::from_utf8(output.stdout).unwrap();
        assert!(!public.contains(PRIVATE));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(PRIVATE));
        let value: Value = serde_json::from_str(&public).unwrap();
        evidence.push(json!({"command":args,"receipt":value}));
    }
    let count = calls.lock().unwrap().len();
    let missing_operation = command(home.path(), &origin)
        .args(["card", "refresh", ID])
        .output()
        .await
        .unwrap();
    assert!(!missing_operation.status.success());
    assert_eq!(
        calls.lock().unwrap().len(),
        count,
        "invalid refresh does not dispatch"
    );
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../output/native-account-api");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("rust-cli-management.json"),
        serde_json::to_vec_pretty(&json!({"commands":evidence,"requests":*calls.lock().unwrap()}))
            .unwrap(),
    )
    .unwrap();
    server.abort();
}
