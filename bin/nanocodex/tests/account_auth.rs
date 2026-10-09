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

fn cli_binary() -> PathBuf {
    std::env::var_os("NANOCODEX_TEST_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_nanocodex")))
}

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
        // `nanocodex login|status|logout` are the managed account commands.
        let mut command = Command::new(cli_binary());
        command.args(args);
        // Account fixtures must never enroll or reconfigure the GUI user's Hand.
        if args.first() == Some(&"login") {
            command.arg("--no-hand");
        }
        command
            .current_dir(self.dir.path())
            .env("HOME", self.dir.path())
            .env("NANOCODEX_DIR", self.dir.path().join("install"))
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
    success(&fixture.run(&["list"], "").await);
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

// The standalone menu consumes the shipped full CLI, not a test-only status API.
#[tokio::test]
async fn hand_menu_status_observes_real_http_without_mutations_or_secret_output() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let binary = cli_binary();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("account.json");
    // A project-scoped credential must not turn a signed-out menu into another
    // account. The real command runs from this directory for every observation.
    std::fs::write(
        dir.path().join(".env"),
        format!("NANOCODEX_API_KEY={}\n", key()),
    )
    .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mode = Arc::new(AtomicUsize::new(0));
    let requests = seen.clone();
    let selected_mode = mode.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(move |method: Method, uri: Uri, headers: HeaderMap| {
        let requests = requests.clone();
        let mode = selected_mode.load(Ordering::SeqCst);
        async move {
            assert_eq!(method, Method::GET, "Status must not mutate any remote resource");
            assert_eq!(headers["authorization"], format!("Bearer {}", key()));
            assert!(!headers.contains_key("cookie"));
            requests.lock().unwrap().push(uri.path().to_owned());
            if uri.path() == "/v1/me" {
                if mode == 1 { return (StatusCode::UNAUTHORIZED, key()).into_response(); }
                if mode == 4 { return (StatusCode::OK, key()).into_response(); }
                if mode == 5 {
                    return Response::builder().status(302).header("location", "/redirected").body(axum::body::Body::empty()).unwrap();
                }
                return axum::Json(json!({"authentication": "api_key", "user": {"id": "synthetic-user"},
                    "organization": {"id": "synthetic-org"}, "team": {"id": "synthetic-team"}, "role": "owner"})).into_response();
            }
            if uri.path() == "/v1/account/hands/inventory" {
                if mode == 2 { return (StatusCode::FORBIDDEN, key()).into_response(); }
                if mode == 3 { return StatusCode::SERVICE_UNAVAILABLE.into_response(); }
                if mode == 6 { tokio::time::sleep(std::time::Duration::from_secs(30)).await; }
                let mut data = vec![
                    json!({"id": "synthetic-mac", "name": "Synthetic Mac", "kind": "hand", "online": true, "health": "connected",
                        "workspace": "/private/path-that-must-not-leak", "route_token": key()}),
                    json!({"id": "offline-laptop", "name": "Sleeping laptop", "kind": "hand", "online": false, "health": "offline"}),
                    json!({"id": "workspace", "name": "Project workspace", "kind": "workspace", "online": true, "health": "connected"}),
                    json!({"id": "vm:build", "name": "Build VM", "kind": "vm", "online": true, "health": "connected"}),
                ];
                if mode == 7 { data[0]["name"] = key().into(); }
                if mode == 8 { data[0]["online"] = Value::Null; data[0]["health"] = "unknown".into(); }
                if mode == 9 { data[1]["online"] = true.into(); }
                return axum::Json(json!({"data": data, "coverage": "known_account_and_workspace", "complete": mode != 8})).into_response();
            }
            assert_eq!(uri.path(), "/v1/account/hands/screens");
            axum::Json(json!({"surfaces": [{"machine_id": "synthetic-screen", "machine_name": "Synthetic screen",
                "id": "display", "generation": key(), "transport": "frames-v1", "controllable": true},
                {"machine_id": "offline-laptop", "machine_name": "Sleeping laptop", "transport": "frames-v1"}]})).into_response()
        }
    });
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let command = || {
        let mut command = Command::new(&binary);
        command
            .args(["hand", "menu-status"])
            .current_dir(dir.path())
            .env_remove("NANOCODEX_API_KEY")
            .env_remove("NC_API_KEY")
            .env("HOME", dir.path())
            .env("CODEX_HOME", dir.path())
            .env("NANOCODEX_DIR", dir.path().join("install"))
            .env("NANOCODEX_ACCOUNT_FILE", &path)
            .env("NANOCODEX_MANAGED_URL", &origin)
            .kill_on_drop(true);
        command
    };
    let run = |mut command: Command| async move {
        let output = tokio::time::timeout(std::time::Duration::from_secs(15), command.output())
            .await
            .unwrap()
            .unwrap();
        success(&output);
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(!text.contains("path-that-must-not-leak"));
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schema_version"], 1);
        value
    };
    let signed_out = run(command()).await;
    assert_eq!(signed_out["account"]["state"], "signed_out");
    assert_eq!(signed_out["inventory"]["state"], "signed_out");
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "Polling created local state"
    );
    let credential = serde_json::to_vec(
        &json!({"version": 1, "accounts": {origin.clone(): {"api_key": key()}}}),
    )
    .unwrap();
    std::fs::write(&path, &credential).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    for (scenario, expected_account, expected_inventory) in [
        (0, "verified", "ready"),
        (1, "expired", "expired"),
        (2, "verified", "permission_denied"),
        (3, "verified", "network_error"),
        (4, "unknown", "unknown"),
        (5, "unknown", "unknown"),
        (6, "verified", "network_error"),
        (7, "verified", "unknown"),
        (8, "verified", "partial"),
        (9, "verified", "unknown"),
        (0, "verified", "ready"), // Recovery reads current state; no sticky cache.
    ] {
        mode.store(scenario, Ordering::SeqCst);
        seen.lock().unwrap().clear();
        let value = run(command()).await;
        assert_eq!(
            value["account"]["state"], expected_account,
            "scenario {scenario}: {value}"
        );
        assert_eq!(
            value["inventory"]["state"], expected_inventory,
            "scenario {scenario}: {value}"
        );
        assert!(
            !value["inventory"]["hands"]
                .as_array()
                .unwrap()
                .iter()
                .any(|hand| hand["kind"] == "workspace")
        );
        let observed = seen.lock().unwrap().clone();
        assert_eq!(observed[0], "/v1/me");
        assert!(!observed.iter().any(|path| path == "/redirected"));
        if expected_account != "verified" {
            assert_eq!(observed.len(), 1);
        }
        if scenario == 0 {
            let hands = value["inventory"]["hands"].as_array().unwrap();
            assert_eq!(hands.len(), 4);
            let offline = hands
                .iter()
                .find(|hand| hand["id"] == "offline-laptop")
                .unwrap();
            assert_eq!(offline["health"], "offline");
            assert_eq!(offline["online"], false);
            assert_eq!(offline["detail"], "Screen also advertised");
            assert!(!hands.iter().any(|hand| hand["id"] == "workspace"));
            assert_eq!(
                hands
                    .iter()
                    .find(|hand| hand["id"] == "synthetic-mac")
                    .unwrap()["health"],
                "connected"
            );
            let screen = hands
                .iter()
                .find(|hand| hand["kind"] == "screen_only")
                .unwrap();
            assert_eq!(screen["online"], Value::Null);
            assert_eq!(screen["health"], "screen_advertised");
            assert_eq!(screen["transport"], "screen_frames");
        }
        if scenario == 8 {
            let hand = value["inventory"]["hands"]
                .as_array()
                .unwrap()
                .iter()
                .find(|hand| hand["id"] == "synthetic-mac")
                .unwrap();
            assert_eq!(hand["online"], Value::Null);
            assert_eq!(hand["health"], "unknown");
        }
        assert_eq!(std::fs::read(&path).unwrap(), credential);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            2,
            "Polling created updater or service state"
        );
        eprintln!(
            "scenario={scenario} account={expected_account} inventory={expected_inventory} requests={observed:?}"
        );
    }
    server.abort();
    let disconnected = run(command()).await;
    assert_eq!(disconnected["account"]["state"], "network_error");
    std::fs::remove_file(&path).unwrap();
    assert_eq!(run(command()).await["account"]["state"], "signed_out");
}

#[tokio::test]
async fn hand_registry_lists_and_removes_through_authenticated_http() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("account.json");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let requests = seen.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let expected_origin = origin.clone();
    let app = Router::new().fallback(move |method: Method, uri: Uri, headers: HeaderMap| {
        let seen = requests.clone();
        let origin = expected_origin.clone();
        async move {
            assert_eq!(headers["authorization"], format!("Bearer {}", key()));
            assert_eq!(headers["origin"], origin);
            assert!(!headers.contains_key("cookie"));
            seen.lock().unwrap().push(format!("{method} {uri}"));
            let (status, body) = match (method, uri.path(), uri.query()) {
                (Method::GET, "/v1/account/hands/inventory", None) => (
                    200,
                    json!({"data":[
                    {"id":"vm:build", "name":"Build machine", "kind":"vm", "health":"offline"}
                ],"complete":false}),
                ),
                (Method::DELETE, "/v1/account/hands/vm:build", None) => {
                    (200, json!({"forgotten":true}))
                }
                (Method::DELETE, "/v1/account/hands/online", None) => {
                    (409, json!({"error":"hand_online"}))
                }
                (Method::DELETE, "/v1/account/hands/unknown", None) => {
                    (409, json!({"error":"hand_unknown"}))
                }
                (Method::DELETE, "/v1/account/hands/online", Some("force=1")) => {
                    (200, json!({"forgotten":true}))
                }
                (Method::DELETE, "/v1/account/hands/revoked", None) => {
                    (401, json!({"error":"unauthorized", "message":key()}))
                }
                (Method::POST, "/v1/account/hands/prune", None) => {
                    (200, json!({"forgotten":["offline"],"complete":false}))
                }
                _ => panic!("Unexpected registry request: {uri}"),
            };
            (StatusCode::from_u16(status).unwrap(), axum::Json(body))
        }
    });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({"version":1,"accounts":{&origin:{"api_key":key()}}})).unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let cases: &[(&[&str], bool, &str)] = &[
        (&["list"], true, "Build machine"),
        (&["forget", "vm:build"], true, "Forgot Hand vm:build"),
        (&["forget", "online"], false, "connected right now"),
        (
            &["forget", "unknown"],
            false,
            "unconfirmed connection status",
        ),
        (&["forget", "online", "--force"], true, "Forgot Hand online"),
        (&["forget", "revoked"], false, "401"),
        (&["prune"], true, "Removed 1 offline Hand"),
        (&["forget", "../invalid"], false, "Invalid Hand identifier"),
    ];
    for (args, succeeds, expected) in cases {
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            Command::new(cli_binary())
                .arg("hand")
                .args(*args)
                .current_dir(dir.path())
                .env("HOME", dir.path())
                .env("CODEX_HOME", dir.path())
                .env("NANOCODEX_DIR", dir.path().join("install"))
                .env("NANOCODEX_ACCOUNT_FILE", &path)
                .env("NANOCODEX_MANAGED_URL", &origin)
                .env_remove("NANOCODEX_API_KEY")
                .env_remove("NC_API_KEY")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.status.success(), *succeeds, "{args:?}: {text}");
        assert!(text.contains(expected), "{args:?}: {text}");
        assert!(!text.contains(&key()), "credential leaked");
        if *args == ["list"] {
            assert!(text.contains("partial"));
        }
        if *args == ["prune"] {
            assert!(text.contains("could not be read"));
        }
    }
    assert_eq!(
        seen.lock().unwrap().len(),
        7,
        "No writes may be retried; invalid ID stays local"
    );
    assert!(
        !dir.path().join("install").exists(),
        "Registry commands must not install a Hand"
    );
    server.abort();
}
