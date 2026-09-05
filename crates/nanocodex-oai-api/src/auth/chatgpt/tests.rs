use std::sync::{
    Arc, RwLock,
    atomic::{AtomicUsize, Ordering},
};

use super::{
    ManagedChatGptAuth, ManagedState, StoredCredentials, authorize_url, jwt_expiration,
    personal_access_token_auth, read_store, refresh_error_code, rfc3339_utc, unix_now, write_store,
};
use crate::auth::{OpenAiAuth, OpenAiAuthError, OpenAiAuthMode};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};

fn jwt(payload: &serde_json::Value) -> String {
    format!(
        "header.{}.signature",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).unwrap())
    )
}

fn credentials(access: &str, refresh: &str) -> StoredCredentials {
    StoredCredentials {
        id_token: jwt(&serde_json::json!({
            "email": "user@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-1",
                "chatgpt_plan_type": "plus"
            }
        })),
        access_token: access.to_owned(),
        refresh_token: refresh.to_owned(),
        account_id: "account-1".into(),
        email: Some("user@example.com".into()),
        plan: Some("plus".into()),
        fedramp: false,
    }
}

#[test]
fn formats_unix_timestamps_as_codex_compatible_utc() {
    assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(rfc3339_utc(2_147_483_647), "2038-01-19T03:14:07Z");
}

fn temp_auth_file() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "nanocodex-auth-test-{}.json",
        super::random_urlsafe().unwrap()
    ))
}

fn managed_source(
    auth_file: &std::path::Path,
    issuer: impl Into<Arc<str>>,
    credentials: StoredCredentials,
) -> Arc<ManagedChatGptAuth> {
    let auth_record = super::read_document(auth_file).unwrap();
    Arc::new(ManagedChatGptAuth {
        auth_file: auth_file.to_path_buf(),
        issuer: issuer.into(),
        client: super::auth_client().unwrap(),
        state: RwLock::new(ManagedState {
            credentials,
            auth_record,
            revision: 0,
            permanent_failure: None,
        }),
        refresh: Mutex::new(()),
    })
}

fn managed(
    auth_file: &std::path::Path,
    issuer: impl Into<Arc<str>>,
    credentials: StoredCredentials,
) -> OpenAiAuth {
    OpenAiAuth::managed_chatgpt(managed_source(auth_file, issuer, credentials))
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut request = Vec::new();
    loop {
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk).await.unwrap();
        assert_ne!(read, 0);
        request.extend_from_slice(&chunk[..read]);
        let Some(headers_end) = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|position| position + 4)
        else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..headers_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(str::trim)
                    .and_then(|length| length.parse::<usize>().ok())
            })
            .unwrap_or(0);
        if request.len() >= headers_end + content_length {
            return String::from_utf8(request).unwrap();
        }
    }
}

async fn reply_json(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    response: &serde_json::Value,
) {
    let response = response.to_string();
    stream
        .write_all(
            format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
}

#[test]
fn parses_account_and_expiry_without_exposing_tokens() {
    let id_token = jwt(&serde_json::json!({
        "email": "user@example.com",
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "account-1",
            "chatgpt_plan_type": "plus",
            "chatgpt_account_is_fedramp": true
        }
    }));
    let credentials = StoredCredentials::from_tokens(super::TokenResponse {
        id: id_token,
        access: jwt(&serde_json::json!({ "exp": 12345 })),
        refresh: "refresh-secret".into(),
    })
    .unwrap();
    assert_eq!(credentials.account_id, "account-1");
    assert_eq!(credentials.status().plan.as_deref(), Some("plus"));
    assert_eq!(jwt_expiration(&credentials.access_token), Some(12345));
}

#[test]
fn recognizes_nested_refresh_error_codes() {
    assert_eq!(
        refresh_error_code(br#"{"error":{"code":"refresh_token_reused"}}"#).as_deref(),
        Some("refresh_token_reused")
    );
}

#[test]
fn codex_auth_document_round_trip_preserves_unrelated_fields() {
    let auth_file = temp_auth_file();
    let original = credentials("access-1", "refresh-1");
    let document = serde_json::json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": "preserved-api-key",
        "tokens": {
            "id_token": original.id_token,
            "access_token": original.access_token,
            "refresh_token": original.refresh_token,
            "account_id": original.account_id,
            "future_token_field": {"preserved": true}
        },
        "last_refresh": "2026-01-01T00:00:00Z",
        "agent_identity": {"future": "preserved"}
    });
    std::fs::write(&auth_file, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

    let loaded = read_store(&auth_file).unwrap();
    assert_eq!(loaded.email.as_deref(), Some("user@example.com"));
    let rotated = credentials("access-2", "refresh-2");
    write_store(&auth_file, &rotated).unwrap();

    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&auth_file).unwrap()).unwrap();
    assert_eq!(stored["auth_mode"], "chatgpt");
    assert_eq!(stored["OPENAI_API_KEY"], "preserved-api-key");
    assert_eq!(stored["agent_identity"]["future"], "preserved");
    assert_eq!(stored["tokens"]["access_token"], "access-2");
    assert_eq!(stored["tokens"]["refresh_token"], "refresh-2");
    assert_eq!(stored["tokens"]["future_token_field"]["preserved"], true);
    std::fs::remove_file(auth_file).unwrap();
}

#[test]
fn authorization_url_contains_the_pkce_and_offline_access_contract() {
    let url = authorize_url(
        "https://auth.openai.com",
        "http://localhost:1455/auth/callback",
        "state-value",
        "challenge-value",
    )
    .unwrap();
    let url = url::Url::parse(&url).unwrap();
    let query = url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(url.path(), "/oauth/authorize");
    assert_eq!(query.get("response_type").unwrap(), "code");
    assert_eq!(query.get("client_id").unwrap(), super::OAUTH_CLIENT_ID);
    assert_eq!(query.get("code_challenge_method").unwrap(), "S256");
    assert_eq!(query.get("code_challenge").unwrap(), "challenge-value");
    assert_eq!(query.get("state").unwrap(), "state-value");
    assert!(query.get("scope").unwrap().contains("offline_access"));
}

#[test]
fn loads_codex_personal_access_token_auth_files_without_oauth_tokens() {
    let auth_file = temp_auth_file();
    std::fs::write(&auth_file, br#"{"personal_access_token":"at-file-token"}"#).unwrap();

    let auth = super::load_chatgpt_auth(&auth_file).unwrap();

    assert_eq!(auth.mode(), OpenAiAuthMode::ChatGpt);
    std::fs::remove_file(auth_file).unwrap();
}

#[tokio::test]
async fn persistent_access_token_hydrates_account_metadata_once_and_never_refreshes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let server_requests = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        server_requests.fetch_add(1, Ordering::AcqRel);
        let request = read_http_request(&mut stream).await;
        assert!(request.starts_with("GET /v1/user-auth-credential/whoami HTTP/1.1"));
        assert!(request.contains("authorization: Bearer at-persistent-token\r\n"));
        reply_json(
            &mut stream,
            "200 OK",
            &serde_json::json!({
                "email": "business@example.com",
                "chatgpt_user_id": "user-1",
                "chatgpt_account_id": "account-business",
                "chatgpt_plan_type": "business",
                "chatgpt_account_is_fedramp": true
            }),
        )
        .await;
    });
    let auth = personal_access_token_auth(Arc::from("at-persistent-token"), &base_url).unwrap();

    let first = auth.snapshot().await.unwrap();
    let second = auth.snapshot().await.unwrap();

    assert_eq!(first.bearer(), "at-persistent-token");
    assert_eq!(first.account_id(), Some("account-business"));
    assert!(first.is_fedramp());
    assert_eq!(second.account_id(), first.account_id());
    assert!(matches!(
        auth.recover_unauthorized(&first).await,
        Err(OpenAiAuthError::LoginRequired(_))
    ));
    server.await.unwrap();
    assert_eq!(requests.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn unauthorized_recovery_reloads_a_rotated_credential_from_disk() {
    let auth_file = temp_auth_file();
    let original = credentials("access-1", "refresh-1");
    write_store(&auth_file, &original).unwrap();
    let auth = managed(&auth_file, "http://127.0.0.1:1", original);
    let rejected = auth.snapshot().await.unwrap();

    let rotated = credentials("access-2", "refresh-2");
    write_store(&auth_file, &rotated).unwrap();
    auth.recover_unauthorized(&rejected).await.unwrap();

    let recovered = auth.snapshot().await.unwrap();
    assert_eq!(recovered.bearer(), "access-2");
    assert_eq!(recovered.revision(), 1);
    std::fs::remove_file(auth_file).unwrap();
}

#[tokio::test]
async fn unauthorized_recovery_compares_the_complete_shared_auth_record() {
    let auth_file = temp_auth_file();
    let original = credentials("same-access", "refresh-1");
    write_store(&auth_file, &original).unwrap();
    let manager_a = managed_source(&auth_file, "http://127.0.0.1:1", original.clone());
    let auth_b = managed(&auth_file, "http://127.0.0.1:1", original);
    let rejected = auth_b.snapshot().await.unwrap();

    manager_a
        .apply_refresh(super::RefreshResponse {
            id: None,
            access: Some("same-access".into()),
            refresh: Some("refresh-2".into()),
        })
        .unwrap();
    auth_b.recover_unauthorized(&rejected).await.unwrap();

    let recovered = auth_b.snapshot().await.unwrap();
    assert_eq!(recovered.bearer(), "same-access");
    assert_eq!(recovered.revision(), 1);
    assert_eq!(read_store(&auth_file).unwrap().refresh_token, "refresh-2");
    std::fs::remove_file(auth_file).unwrap();
}

#[tokio::test]
async fn unauthorized_recovery_refuses_a_different_stored_account() {
    let auth_file = temp_auth_file();
    let original = credentials("access-1", "refresh-1");
    write_store(&auth_file, &original).unwrap();
    let auth_b = managed(&auth_file, "http://127.0.0.1:1", original);
    let rejected = auth_b.snapshot().await.unwrap();

    let mut changed = credentials("access-2", "refresh-2");
    changed.account_id = "account-2".into();
    write_store(&auth_file, &changed).unwrap();
    let _manager_a = managed(&auth_file, "http://127.0.0.1:1", changed);
    assert!(matches!(
        auth_b.recover_unauthorized(&rejected).await,
        Err(OpenAiAuthError::AccountChanged)
    ));
    std::fs::remove_file(auth_file).unwrap();
}

#[tokio::test]
async fn proactive_refresh_adopts_another_managers_rotation_without_refreshing() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let server_requests = Arc::clone(&requests);
    let fresh_access = jwt(&serde_json::json!({ "exp": unix_now() + 3_600 }));
    let server_access = fresh_access.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let call = server_requests.fetch_add(1, Ordering::AcqRel);
            let request = read_http_request(&mut stream).await;
            assert!(request.contains(r#""refresh_token":"refresh-1""#));
            reply_json(
                &mut stream,
                "200 OK",
                &serde_json::json!({
                    "access_token": if call == 0 {
                        server_access.clone()
                    } else {
                        format!("unexpected-refresh-{call}")
                    },
                    "refresh_token": format!("refresh-{}", call + 2)
                }),
            )
            .await;
        }
    });

    let auth_file = temp_auth_file();
    let expired = jwt(&serde_json::json!({ "exp": unix_now() - 1 }));
    let original = credentials(&expired, "refresh-1");
    write_store(&auth_file, &original).unwrap();
    let auth_a = managed(&auth_file, issuer.clone(), original.clone());
    let auth_b = managed(&auth_file, issuer, original);

    assert_eq!(auth_a.snapshot().await.unwrap().bearer(), fresh_access);
    assert_eq!(auth_b.snapshot().await.unwrap().bearer(), fresh_access);
    assert_eq!(
        requests.load(Ordering::Acquire),
        1,
        "the second manager must adopt the first manager's persisted rotation"
    );

    server.abort();
    let _ = server.await;
    std::fs::remove_file(auth_file).unwrap();
}

#[tokio::test]
async fn stale_refresh_rejection_adopts_a_concurrent_same_account_rotation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let auth_file = temp_auth_file();
    let original = credentials("access-1", "refresh-1");
    write_store(&auth_file, &original).unwrap();
    let manager_a = managed_source(&auth_file, issuer.clone(), original.clone());
    let auth_b = managed(&auth_file, issuer, original);
    let rejected = auth_b.snapshot().await.unwrap();

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        assert!(request.contains(r#""refresh_token":"refresh-1""#));
        manager_a
            .apply_refresh(super::RefreshResponse {
                id: None,
                access: Some("access-2".into()),
                refresh: Some("refresh-2".into()),
            })
            .unwrap();
        reply_json(
            &mut stream,
            "401 Unauthorized",
            &serde_json::json!({
                "error": {"code": "refresh_token_reused"}
            }),
        )
        .await;
    });

    auth_b.recover_unauthorized(&rejected).await.unwrap();
    assert_eq!(auth_b.snapshot().await.unwrap().bearer(), "access-2");
    assert_eq!(read_store(&auth_file).unwrap().refresh_token, "refresh-2");

    server.await.unwrap();
    std::fs::remove_file(auth_file).unwrap();
}

#[tokio::test]
async fn changed_auth_recovers_after_a_cached_permanent_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let auth_file = temp_auth_file();
    let original = credentials("access-1", "refresh-1");
    write_store(&auth_file, &original).unwrap();
    let manager_a = managed_source(&auth_file, issuer.clone(), original.clone());
    let auth_b = managed(&auth_file, issuer, original);
    let rejected = auth_b.snapshot().await.unwrap();

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        assert!(request.contains(r#""refresh_token":"refresh-1""#));
        reply_json(
            &mut stream,
            "401 Unauthorized",
            &serde_json::json!({
                "error": {"code": "refresh_token_reused"}
            }),
        )
        .await;
    });

    assert!(matches!(
        auth_b.recover_unauthorized(&rejected).await,
        Err(OpenAiAuthError::LoginRequired(_))
    ));
    server.await.unwrap();

    manager_a
        .apply_refresh(super::RefreshResponse {
            id: None,
            access: Some("access-2".into()),
            refresh: Some("refresh-2".into()),
        })
        .unwrap();

    assert_eq!(auth_b.snapshot().await.unwrap().bearer(), "access-2");
    std::fs::remove_file(auth_file).unwrap();
}

#[tokio::test]
async fn expired_access_token_is_refreshed_and_rotated_atomically() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        assert!(request.starts_with("POST /oauth/token HTTP/1.1"));
        assert!(request.contains(r#""refresh_token":"refresh-1""#));
        reply_json(
            &mut stream,
            "200 OK",
            &serde_json::json!({
                "access_token": "fresh-access",
                "refresh_token": "refresh-2"
            }),
        )
        .await;
    });

    let auth_file = temp_auth_file();
    let expired = jwt(&serde_json::json!({ "exp": unix_now() - 1 }));
    let original = credentials(&expired, "refresh-1");
    write_store(&auth_file, &original).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&auth_file).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    let auth = managed(&auth_file, issuer, original);

    let snapshot = auth.snapshot().await.unwrap();
    assert_eq!(snapshot.bearer(), "fresh-access");
    let stored = read_store(&auth_file).unwrap();
    assert_eq!(stored.access_token, "fresh-access");
    assert_eq!(stored.refresh_token, "refresh-2");
    server.await.unwrap();
    std::fs::remove_file(auth_file).unwrap();
}
