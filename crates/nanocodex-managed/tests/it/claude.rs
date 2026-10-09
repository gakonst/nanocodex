//! Native public-transport journey. The loopback service stands in for the
//! hosted account boundary; no provider credentials or live provider are used.
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
};
use nanocodex_managed::{
    CatalogAvailabilityError, ClaudeLoginCode, ClaudeLoginStatus, ManagedApiKey, ManagedClient,
    ManagedError, ManagedModel,
};
use serde_json::{Value, json};

const PRIVATE_CODE: &str = "synthetic-private-code#sssssssssssssssssssssssssssssssssssssssssss";
const PRIVATE_STATE: &str = "sssssssssssssssssssssssssssssssssssssssssss";
const EXPIRES: u64 = 1_799_999_999_999;

#[derive(Default)]
struct Journey {
    connected: bool,
    partial: bool,
    pending: bool,
    uncertain: bool,
    reject: bool,
    malformed: bool,
    authorization_override: Option<String>,
    writes: Vec<&'static str>,
}

// Source-equivalent native begin_login manual URL: public flag, registered
// client/manual redirect/scopes, and 43-character state and S256 PKCE challenge.
fn authorization_url() -> String {
    let mut url = url::Url::parse("https://claude.com/cai/oauth/authorize").unwrap();
    url.query_pairs_mut().extend_pairs([
        ("code", "true"),
        ("client_id", "9d1c250a-e61b-44d9-88ed-5944d1962f5e"),
        ("response_type", "code"),
        ("redirect_uri", "https://platform.claude.com/oauth/code/callback"),
        ("scope", "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload user:plugins"),
        ("code_challenge", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"),
        ("code_challenge_method", "S256"),
        ("state", PRIVATE_STATE),
    ]);
    url.into()
}

fn auth(headers: &HeaderMap) {
    assert_eq!(
        headers["authorization"],
        format!("Bearer ncx_live_{}_{}", "c".repeat(12), "d".repeat(43))
    );
    assert!(!headers.contains_key("idempotency-key"));
}

fn status(state: &Journey) -> Value {
    if state.malformed {
        return json!({"state":"pending","expires_at":EXPIRES,"access_token":PRIVATE_CODE});
    }
    if state.uncertain {
        return json!({"state":"exchange_uncertain"});
    }
    if state.connected {
        return json!({"state":"authenticated","expires_at":EXPIRES,"account_id":"synthetic-account","organization_id":"synthetic-organization"});
    }
    if state.pending {
        return json!({"state":"pending","expires_at":EXPIRES});
    }
    json!({"state":"signed_out"})
}

async fn login_start(
    State(state): State<Arc<Mutex<Journey>>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    auth(&headers);
    assert!(body.is_empty());
    let mut state = state.lock().unwrap();
    state.writes.push("start");
    if state.reject {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":PRIVATE_CODE,"message":PRIVATE_STATE})),
        );
    }
    state.pending = true;
    (
        StatusCode::OK,
        Json(
            json!({"state":"pending","authorization_url":state.authorization_override.clone().unwrap_or_else(authorization_url),"expires_at":EXPIRES}),
        ),
    )
}

async fn login_status(State(state): State<Arc<Mutex<Journey>>>, headers: HeaderMap) -> Json<Value> {
    auth(&headers);
    Json(status(&state.lock().unwrap()))
}

async fn complete(
    State(state): State<Arc<Mutex<Journey>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    auth(&headers);
    assert_eq!(body, json!({"code":PRIVATE_CODE}));
    let mut state = state.lock().unwrap();
    state.writes.push("complete");
    if state.reject {
        state.pending = false;
        state.uncertain = true;
        return (
            StatusCode::CONFLICT,
            Json(
                json!({"state":"exchange_uncertain","error":PRIVATE_CODE,"message":PRIVATE_STATE}),
            ),
        );
    }
    state.pending = false;
    state.connected = true;
    (StatusCode::OK, Json(status(&state)))
}

async fn disconnect(
    State(state): State<Arc<Mutex<Journey>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Json<Value> {
    auth(&headers);
    assert!(body.is_empty());
    let mut state = state.lock().unwrap();
    state.writes.push("disconnect");
    state.connected = false;
    state.pending = false;
    state.uncertain = false;
    Json(json!({"connected":false,"state":"signed_out"}))
}

async fn models(State(state): State<Arc<Mutex<Journey>>>, headers: HeaderMap) -> Json<Value> {
    auth(&headers);
    let state = state.lock().unwrap();
    let data: Vec<Value> = if state.partial {
        vec![
            json!({"id":"gpt-6.1-sol","name":"Sol","provider":"openai","thinking":["low","medium","high","xhigh","max"],"fast_mode":true,"reasoning_modes":["standard","pro"]}),
        ]
    } else if state.connected {
        ["claude-sonnet-4-6","claude-opus-4-6","claude-sonnet-5-5","claude-opus-5-5"].into_iter().map(|id| json!({"id":id,"name":id,"provider":"claude","thinking":["low","medium","high"],"fast_mode":false,"reasoning_modes":["standard"]})).collect()
    } else {
        vec![]
    };
    Json(
        json!({"object":"list","data":data,"default_model":if state.partial {Some("gpt-6.1-sol")} else if state.connected {Some("claude-sonnet-4-6")} else {None},"partial":state.partial,"availability":{"claude":{"connected":state.connected,"available":state.connected && !state.partial,"error":if state.partial {Some("claude_models_unavailable")} else {None},"future_public_field":true}},"future_public_field":true}),
    )
}

#[tokio::test]
async fn private_subscription_connect_catalog_and_disconnect_journey() {
    let state = Arc::new(Mutex::new(Journey::default()));
    let app = Router::new()
        .route(
            "/v1/credentials/claude/login",
            get(login_status).post(login_start),
        )
        .route("/v1/credentials/claude/login/complete", post(complete))
        .route("/v1/credentials/claude", delete(disconnect))
        .route("/v1/models", get(models))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ManagedClient::new(
        format!("http://{address}"),
        ManagedApiKey::parse(format!("ncx_live_{}_{}", "c".repeat(12), "d".repeat(43))).unwrap(),
    )
    .unwrap();
    assert_eq!(
        client.claude_login_status().await.unwrap(),
        ClaudeLoginStatus::SignedOut
    );
    assert!(client.models().await.unwrap().data.is_empty());
    match nanocodex_agent::Nanocodex::builder(nanocodex_managed::Managed::create(client.clone()))
        .build()
        .await
    {
        Ok(_) => panic!("zero-config create must not invent an OAI default for an empty catalog"),
        Err(error) => assert!(error.to_string().contains("No managed model is available")),
    }
    let login = client.claude_login_start().await.unwrap();
    assert!(
        login
            .authorization_url()
            .starts_with("https://claude.com/cai/oauth/authorize?")
    );
    assert!(!format!("{login:?}").contains(PRIVATE_STATE));
    assert_eq!(login.expires_at, EXPIRES);
    assert_eq!(
        client.claude_login_status().await.unwrap(),
        ClaudeLoginStatus::Pending {
            expires_at: EXPIRES
        }
    );
    let code = ClaudeLoginCode::parse(PRIVATE_CODE).unwrap();
    assert!(!format!("{code:?}").contains(PRIVATE_CODE));
    assert!(matches!(
        client.claude_login_complete(code).await.unwrap(),
        ClaudeLoginStatus::Authenticated {
            expires_at: EXPIRES,
            ..
        }
    ));
    let catalog = client.models().await.unwrap();
    assert_eq!(catalog.data.len(), 4);
    assert_eq!(catalog.default_model, Some(ManagedModel::ClaudeSonnet46));
    assert!(!catalog.partial);
    assert!(catalog.availability["claude"].available);
    for entry in catalog.data {
        assert!(entry.id.oai().is_none() && !entry.fast_mode);
    }
    // Every native Claude identity keeps its own managed wire identity.
    for native in nanocodex_managed::ClaudeModel::ALL {
        let model = ManagedModel::from(native);
        assert_eq!(model.claude(), Some(native));
        assert_eq!(model.to_string(), native.as_str());
        assert_eq!(serde_json::to_value(model).unwrap(), json!(native.as_str()));
        assert_eq!(
            serde_json::from_value::<ManagedModel>(json!(native.as_str())).unwrap(),
            model
        );
        assert_eq!(
            nanocodex_managed::HarnessModel::from(model),
            nanocodex_managed::HarnessModel::Claude(native)
        );
        assert!(model.supports_thinking(model.default_thinking()));
    }
    state.lock().unwrap().partial = true;
    let degraded = client.models().await.unwrap();
    assert!(degraded.partial);
    assert_eq!(
        degraded.default_model,
        Some(ManagedModel::from(nanocodex_managed::Model::Sol))
    );
    assert_eq!(
        degraded.availability["claude"].error,
        Some(CatalogAvailabilityError::ClaudeModelsUnavailable)
    );
    assert!(
        degraded.availability["claude"].connected && !degraded.availability["claude"].available
    );
    assert_eq!(
        client.default_settings().await.unwrap().model,
        ManagedModel::from(nanocodex_managed::Model::Sol)
    );
    state.lock().unwrap().partial = false;
    assert_eq!(
        client.claude_disconnect().await.unwrap(),
        ClaudeLoginStatus::SignedOut
    );
    assert!(client.models().await.unwrap().data.is_empty());
    state.lock().unwrap().reject = true;
    let error = client.claude_login_start().await.unwrap_err();
    assert!(matches!(
        error,
        ManagedError::Http {
            status: StatusCode::SERVICE_UNAVAILABLE,
            ..
        }
    ));
    assert!(!format!("{error:?} {error}").contains(PRIVATE_CODE));
    assert!(!format!("{error:?} {error}").contains(PRIVATE_STATE));
    let error = client
        .claude_login_complete(ClaudeLoginCode::parse(PRIVATE_CODE).unwrap())
        .await
        .unwrap_err();
    assert!(!format!("{error:?} {error}").contains(PRIVATE_CODE));
    assert_eq!(
        client.claude_login_status().await.unwrap(),
        ClaudeLoginStatus::ExchangeUncertain
    );
    assert_eq!(
        state.lock().unwrap().writes,
        ["start", "complete", "disconnect", "start", "complete"]
    );
    state.lock().unwrap().malformed = true;
    let error = client.claude_login_status().await.unwrap_err();
    assert!(!format!("{error:?} {error}").contains(PRIVATE_CODE));
    // Exercise corrupted authorization destinations at the native public API.
    {
        let mut state = state.lock().unwrap();
        state.reject = false;
        state.malformed = false;
    }
    let valid = authorization_url();
    let mut bad_code = url::Url::parse(&valid).unwrap();
    let pairs: Vec<_> = bad_code
        .query_pairs()
        .map(|(name, value)| {
            let name = name.into_owned();
            let value = if name == "code" {
                PRIVATE_CODE.to_owned()
            } else {
                value.into_owned()
            };
            (name, value)
        })
        .collect();
    bad_code.set_query(None);
    bad_code.query_pairs_mut().extend_pairs(pairs);
    let mut bad_redirect = url::Url::parse(&valid).unwrap();
    let pairs: Vec<_> = bad_redirect
        .query_pairs()
        .map(|(name, value)| {
            let name = name.into_owned();
            let value = if name == "redirect_uri" {
                "https://attacker.invalid/callback".to_owned()
            } else {
                value.into_owned()
            };
            (name, value)
        })
        .collect();
    bad_redirect.set_query(None);
    bad_redirect.query_pairs_mut().extend_pairs(pairs);
    let mut missing_pkce = url::Url::parse(&valid).unwrap();
    let pairs: Vec<_> = missing_pkce
        .query_pairs()
        .filter(|(name, _)| name != "code_challenge")
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    missing_pkce.set_query(None);
    missing_pkce.query_pairs_mut().extend_pairs(pairs);
    for destination in [
        valid.replace("https://claude.com/", "https://attacker.invalid/"),
        bad_code.into(),
        bad_redirect.into(),
        missing_pkce.into(),
    ] {
        let count = state.lock().unwrap().writes.len();
        state.lock().unwrap().authorization_override = Some(destination);
        let error = client.claude_login_start().await.unwrap_err();
        assert!(matches!(error, ManagedError::InvalidResponse(_)));
        assert!(!format!("{error:?} {error}").contains(PRIVATE_CODE));
        assert!(!format!("{error:?} {error}").contains(PRIVATE_STATE));
        assert_eq!(
            state.lock().unwrap().writes.len(),
            count + 1,
            "invalid authorization URL must not retry the start write"
        );
    }
    println!(
        "JOURNEY Source-equivalent claude.com manual OAuth URL with public code=true/client/redirect/scopes/S256 PKCE accepted; corrupted origin/private code/redirect/missing PKCE rejected without leaks/retries; empty catalog generic create blocked; Claude private start -> pending -> authenticated -> 4 eligible models -> typed partial Claude outage preserves healthy OAI default -> disconnect -> empty catalog; 503/409 writes single-shot; private response/error material redacted; token-bearing status rejected. expires_at is milliseconds."
    );
    server.abort();
}

#[tokio::test]
async fn private_completion_lost_response_is_not_replayed() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        let mut buffer = [0; 2048];
        loop {
            let len = socket.read(&mut buffer).await.unwrap();
            assert!(len > 0);
            received.extend_from_slice(&buffer[..len]);
            if received
                .windows(PRIVATE_CODE.len())
                .any(|window| window == PRIVATE_CODE.as_bytes())
            {
                break;
            }
        }
        assert!(received.starts_with(b"POST /v1/credentials/claude/login/complete HTTP/1.1"));
        drop(socket); // Exchange may have occurred; response is lost.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), listener.accept())
                .await
                .is_err(),
            "private completion must not reconnect and replay after a lost response"
        );
    });
    let client = ManagedClient::new(
        format!("http://{address}"),
        ManagedApiKey::parse(format!("ncx_live_{}_{}", "c".repeat(12), "d".repeat(43))).unwrap(),
    )
    .unwrap();
    let error = client
        .claude_login_complete(ClaudeLoginCode::parse(PRIVATE_CODE).unwrap())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("outcome unknown"));
    assert!(!format!("{error:?} {error}").contains(PRIVATE_CODE));
    server.await.unwrap();
    println!(
        "JOURNEY Private completion body received once; connection closed before any response; client reports unknown outcome and makes no second connection (automatic protocol retry disabled)."
    );
}

#[tokio::test]
async fn claude_native_compact_validates_acknowledgement_without_write_replay() {
    let calls = Arc::new(Mutex::new(0));
    let app = Router::new().route("/v1/agents/claude-retained", get(|headers: HeaderMap| async move {
        auth(&headers);
        ([("x-nanocodex-access", "ncx_access_v1.synthetic-test-only"), ("x-nanocodex-access-ttl-ms", "120000")], Json(json!({"settings":{}})))
    })).route("/v1/agents/claude-retained/compact", post(|State(calls): State<Arc<Mutex<usize>>>, headers: HeaderMap, body: Bytes| async move {
        auth(&headers);
        assert!(body.is_empty());
        assert!(!headers.contains_key("x-nanocodex-access"), "compact must use full account authority without cached-access retry");
        let mut calls = calls.lock().unwrap();
        *calls += 1;
        match *calls {
            1 => (StatusCode::OK, Json(json!({"compacted":false}))),
            2 => (StatusCode::OK, Json(json!({"compacted":true,"unexpected":true}))),
            _ => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":"compaction_failed","message":"Compaction outcome is uncertain; inspect session history before retrying"}))),
        }
    })).with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ManagedClient::new(
        format!("http://{address}"),
        ManagedApiKey::parse(format!("ncx_live_{}_{}", "c".repeat(12), "d".repeat(43))).unwrap(),
    )
    .unwrap();
    client.routing_status("claude-retained").await.unwrap();
    assert!(client.compact("../unsafe").await.is_err());
    assert_eq!(*calls.lock().unwrap(), 0);
    assert!(client.compact("claude-retained").await.is_err());
    assert!(client.compact("claude-retained").await.is_err());
    assert!(matches!(
        client.compact("claude-retained").await,
        Err(ManagedError::Http {
            status: StatusCode::SERVICE_UNAVAILABLE,
            ..
        })
    ));
    assert_eq!(*calls.lock().unwrap(), 3);
    println!(
        "JOURNEY Native compact authenticated empty-body POST requires exact positive acknowledgement; invalid ID blocked before dispatch; false/extra-field/503 outcomes return errors without write replay."
    );
    server.abort();
}
