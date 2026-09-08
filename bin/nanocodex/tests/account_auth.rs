//! Exercise the same account journey through each actual CLI entrypoint.
use std::{
    path::PathBuf,
    process::{Output, Stdio},
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::Bytes,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use tokio::{io::AsyncWriteExt, process::Command};

fn key() -> String {
    format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43))
}
fn cookie() -> String {
    format!("nanocodex_account=s_{}", "s".repeat(43))
}

struct Fixture {
    origin: String,
    dir: tempfile::TempDir,
    path: PathBuf,
    seen: Arc<Mutex<Vec<String>>>,
    mint_started: Arc<tokio::sync::Notify>,
    release_mint: Arc<tokio::sync::Notify>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn new(behavior: &'static str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mint_started = Arc::new(tokio::sync::Notify::new());
        let release_mint = Arc::new(tokio::sync::Notify::new());
        let handler_mint_started = mint_started.clone();
        let handler_release_mint = release_mint.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let expected_origin = origin.clone();
        let request_path = path.clone();
        let observed = seen.clone();
        let app = Router::new().fallback(move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let expected_origin = expected_origin.clone();
            let path = request_path.clone();
            let observed = observed.clone();
            let mint_started = handler_mint_started.clone();
            let release_mint = handler_release_mint.clone();
            async move {
                let route = format!("{method} {}", uri.path());
                observed.lock().unwrap().push(route.clone());
                if uri.path() == "/v1/agents" {
                    assert_eq!(headers["authorization"], format!("Bearer {}", key()));
                    assert!(!headers.contains_key("cookie"));
                    return axum::Json(json!({"data": []})).into_response();
                }
                assert_eq!(headers["origin"], expected_origin);
                if uri.path() == "/v1/me" {
                    let authorization = headers.get("authorization").unwrap().to_str().unwrap();
                    assert!(!headers.contains_key("cookie"));
                    if authorization != format!("Bearer {}", key()) || behavior == "unauthorized" {
                        return (StatusCode::UNAUTHORIZED, axum::Json(json!({"error": "unauthorized", "message": key()}))).into_response();
                    }
                    return axum::Json(json!({"authentication": "api_key", "user": {"id": "u_test"}, "organization": {"id": "o_test"}, "team": {"id": "t_test"}, "role": "owner"})).into_response();
                }
                assert!(!headers.contains_key("authorization"));
                match route.as_str() {
                    "POST /v1/auth/sms/start" => {
                        assert!(!headers.contains_key("cookie"));
                        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!({"phone": "+14155550123"}));
                        if behavior == "redirect" {
                            return Response::builder().status(302).header("location", format!("{expected_origin}/redirected")).body(axum::body::Body::empty()).unwrap();
                        }
                        if behavior == "rate_limited" {
                            return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "123")], axum::Json(json!({"error": key(), "message": key()}))).into_response();
                        }
                        if behavior == "malformed" { return axum::Json(json!({"challenge_id": "bad", "expires_in": 300, "resend_after": 60})).into_response(); }
                        axum::Json(json!({"challenge_id": "c".repeat(43), "expires_in": 300, "resend_after": 60})).into_response()
                    }
                    "POST /v1/auth/sms/verify" => {
                        assert!(!headers.contains_key("cookie"));
                        let body: Value = serde_json::from_slice(&body).unwrap();
                        assert_eq!(body["phone"], "+14155550123");
                        assert_eq!(body["challenge_id"], "c".repeat(43));
                        if body["code"] == "000000" {
                            return (StatusCode::BAD_REQUEST, axum::Json(json!({"error": "invalid_or_expired_otp"}))).into_response();
                        }
                        assert_eq!(body["code"], "123456");
                        let cookie = if behavior == "bad_cookie" { "nanocodex_account=anonymous".to_owned() } else { format!("{}; Path=/; HttpOnly", cookie()) };
                        ([("set-cookie", cookie)], axum::Json(json!({"ok": true}))).into_response()
                    }
                    "POST /v1/api-keys" => {
                        assert_eq!(headers["cookie"], cookie());
                        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["label"], "Nanocodex CLI");
                        if matches!(behavior, "save_failure" | "revoke_failure") { std::fs::create_dir(&path).unwrap(); }
                        if behavior == "cancel_mint" {
                            mint_started.notify_one();
                            release_mint.notified().await;
                        }
                        // An unrelated metadata ID must never select the revoked key.
                        axum::Json(json!({"api_key": key(), "key": {"id": "wrongwrong12"}})).into_response()
                    }
                    "DELETE /v1/api-keys/aaaaaaaaaaaa" => {
                        assert_eq!(headers["cookie"], cookie());
                        if behavior == "revoke_failure" { return StatusCode::INTERNAL_SERVER_ERROR.into_response(); }
                        StatusCode::NO_CONTENT.into_response()
                    }
                    "POST /v1/auth/logout" => {
                        assert_eq!(headers["cookie"], cookie());
                        if behavior == "logout_failure" { StatusCode::INTERNAL_SERVER_ERROR.into_response() }
                        else { StatusCode::NO_CONTENT.into_response() }
                    }
                    _ => panic!("unexpected request {route}"),
                }
            }
        });
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            origin,
            dir,
            path,
            seen,
            mint_started,
            release_mint,
            server,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let binary = option_env!("CARGO_BIN_EXE_nanocodex2")
            .or(option_env!("CARGO_BIN_EXE_nanocodex"))
            .unwrap();
        let mut command = Command::new(binary);
        if option_env!("CARGO_BIN_EXE_nanocodex2").is_none() {
            command.arg("account");
        }
        command
            .args(args)
            .current_dir(self.dir.path())
            .env_remove("NANOCODEX_API_KEY")
            .env_remove("NC_API_KEY")
            .env("CODEX_HOME", self.dir.path())
            .env("NANOCODEX_ACCOUNT_FILE", &self.path)
            .env("NANOCODEX_MANAGED_URL", &self.origin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    async fn run(&self, args: &[&str], input: &str) -> Output {
        let mut child = self.command(args).spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(20), child.wait_with_output())
            .await
            .unwrap()
            .unwrap()
    }
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains(&key()), "credential leaked");
        assert!(!text.contains(&cookie()), "session leaked");
    }
}

#[tokio::test]
async fn sms_login_retry_status_and_logout_use_the_same_account_contract() {
    let fixture = Fixture::new("ok").await;
    success(
        &fixture
            .run(
                &["login", "--phone", "+1 (415) 555-0123"],
                "000000\n123456\n",
            )
            .await,
    );
    let stored = std::fs::read_to_string(&fixture.path).unwrap();
    assert!(stored.contains(&key()));
    assert!(!stored.contains("s_"));
    assert!(!stored.contains("14155550123"));
    let status = fixture.run(&["status"], "").await;
    success(&status);
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["source"], "saved");
    assert_eq!(status["account"]["user"], "u_test");
    if option_env!("CARGO_BIN_EXE_nanocodex2").is_some() {
        success(&fixture.run(&["list"], "").await);
    }
    success(&fixture.run(&["logout"], "").await);
    assert!(
        !std::fs::read_to_string(&fixture.path)
            .unwrap()
            .contains(&key())
    );
    let status = fixture.run(&["status"], "").await;
    success(&status);
    assert_eq!(
        serde_json::from_slice::<Value>(&status.stdout).unwrap()["authenticated"],
        false
    );
    let seen = fixture.seen.lock().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|route| *route == "POST /v1/auth/sms/verify")
            .count(),
        2
    );
    assert_eq!(
        seen.iter()
            .filter(|route| *route == "POST /v1/auth/logout")
            .count(),
        1
    );
    assert!(!seen.iter().any(|route| route.starts_with("DELETE")));
}

#[tokio::test]
async fn api_key_import_validates_and_env_credentials_override_saved_login() {
    let fixture = Fixture::new("ok").await;
    success(
        &fixture
            .run(&["login", "--with-api-key"], &format!("{}\n", key()))
            .await,
    );
    let status = fixture
        .command(&["status"])
        .env(
            "NANOCODEX_API_KEY",
            format!("ncx_live_{}_{}", "z".repeat(12), "x".repeat(43)),
        )
        .output()
        .await
        .unwrap();
    assert!(!status.status.success());
    assert!(String::from_utf8_lossy(&status.stderr).contains("expired"));
    let status = fixture
        .command(&["status"])
        .env("NANOCODEX_API_KEY", "")
        .env("NC_API_KEY", key())
        .output()
        .await
        .unwrap();
    assert!(!status.status.success());
    assert!(String::from_utf8_lossy(&status.stderr).contains("must not be empty"));
    let status = fixture
        .command(&["status"])
        .env("NC_API_KEY", key())
        .output()
        .await
        .unwrap();
    success(&status);
    assert_eq!(
        serde_json::from_slice::<Value>(&status.stdout).unwrap()["source"],
        "NC_API_KEY"
    );
    let status = fixture
        .run(&["status", "--managed-url", "https://other.example"], "")
        .await;
    success(&status);
    assert_eq!(
        serde_json::from_slice::<Value>(&status.stdout).unwrap()["authenticated"],
        false
    );
    let stored = std::fs::read(&fixture.path).unwrap();
    let failed = fixture
        .run(&["login", "--with-api-key"], "not-an-account-key\n")
        .await;
    assert!(!failed.status.success());
    assert_eq!(std::fs::read(&fixture.path).unwrap(), stored);
}

#[tokio::test]
async fn failed_save_revokes_the_exact_minted_key_and_logs_out_the_temporary_session() {
    let fixture = Fixture::new("save_failure").await;
    let output = fixture
        .run(&["login", "--phone", "+14155550123"], "123456\n")
        .await;
    assert!(!output.status.success());
    assert_eq!(
        *fixture.seen.lock().unwrap(),
        [
            "POST /v1/auth/sms/start",
            "POST /v1/auth/sms/verify",
            "POST /v1/api-keys",
            "DELETE /v1/api-keys/aaaaaaaaaaaa",
            "POST /v1/auth/logout"
        ]
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains(&key()));
}

#[tokio::test]
async fn temporary_logout_failure_preserves_the_successful_login() {
    let fixture = Fixture::new("logout_failure").await;
    success(
        &fixture
            .run(&["login", "--phone", "+14155550123"], "123456\n")
            .await,
    );
    success(&fixture.run(&["status"], "").await);
    assert!(
        !fixture
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|route| route.starts_with("DELETE"))
    );
}

#[tokio::test]
async fn redirects_bad_challenges_cookies_and_rate_limits_do_not_persist_credentials() {
    for behavior in ["redirect", "malformed", "bad_cookie", "rate_limited"] {
        let fixture = Fixture::new(behavior).await;
        let output = fixture
            .run(&["login", "--phone", "+14155550123"], "123456\n")
            .await;
        assert!(!output.status.success(), "{behavior}");
        assert!(!fixture.path.exists());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains(&key()));
        if behavior == "rate_limited" {
            assert!(stderr.contains("123 seconds"));
        }
        assert!(
            !fixture
                .seen
                .lock()
                .unwrap()
                .iter()
                .any(|route| route == "POST /v1/api-keys" || route.contains("redirected"))
        );
    }
}

#[tokio::test]
async fn closed_input_does_not_issue_a_key() {
    let fixture = Fixture::new("ok").await;
    let output = fixture.run(&["login", "--phone", "+14155550123"], "").await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("input closed"));
    assert!(!fixture.path.exists());
    assert_eq!(*fixture.seen.lock().unwrap(), ["POST /v1/auth/sms/start"]);
}

#[tokio::test]
async fn failed_rollback_is_reported_and_still_ends_the_temporary_session() {
    let fixture = Fixture::new("revoke_failure").await;
    let output = fixture
        .run(&["login", "--phone", "+14155550123"], "123456\n")
        .await;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Could not revoke the unused CLI key"),
        "{stderr}"
    );
    assert!(!stderr.contains(&key()));
    assert_eq!(
        fixture.seen.lock().unwrap().last().unwrap(),
        "POST /v1/auth/logout"
    );
}

#[tokio::test]
async fn revoked_credential_is_reported_without_silently_changing_accounts() {
    let fixture = Fixture::new("unauthorized").await;
    success(
        &fixture
            .run(&["login", "--phone", "+14155550123"], "123456\n")
            .await,
    );
    let stored = std::fs::read(&fixture.path).unwrap();
    let output = fixture.run(&["status"], "").await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("expired"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(&key()));
    assert_eq!(std::fs::read(&fixture.path).unwrap(), stored);
}

#[cfg(unix)]
#[tokio::test]
async fn ctrl_c_during_mint_waits_for_the_result_and_revokes_without_saving() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let fixture = Fixture::new("cancel_mint").await;
    let mut child = fixture
        .command(&["login", "--phone", "+14155550123"])
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"123456\n")
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        fixture.mint_started.notified(),
    )
    .await
    .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().unwrap().to_string()])
            .status()
            .await
            .unwrap()
            .success()
    );
    // Wait for the child's signal handler acknowledgement before releasing
    // the response. This deterministically exercises the in-flight mint race.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let mut line = String::new();
            assert!(stderr.read_line(&mut line).await.unwrap() > 0);
            if line.contains("Cancelling sign-in") {
                break;
            }
        }
    })
    .await
    .unwrap();
    fixture.release_mint.notify_one();
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(!output.status.success());
    assert!(!fixture.path.exists());
    assert_eq!(
        *fixture.seen.lock().unwrap(),
        [
            "POST /v1/auth/sms/start",
            "POST /v1/auth/sms/verify",
            "POST /v1/api-keys",
            "DELETE /v1/api-keys/aaaaaaaaaaaa",
            "POST /v1/auth/logout"
        ]
    );
}
