//! Composed public OAuth + real SQLite + native Claude tool/compaction journey.
//! Scenarios defined before implementation in output/subscription-integration/scenarios.md.
//! All identities, credentials and HTTP endpoints in this test are synthetic.
#![cfg(all(feature = "claude", feature = "sqlite"))]

use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use futures_util::StreamExt;
use nanocodex_agent::{Nanocodex, PromptRequest};
use nanocodex_claude::{Claude, ClaudeAuthFuture, ClaudeClient, ToolDefinition, subscription::*};
use nanocodex_durability::{DurableAgentExt, DurableSession, SqliteStore};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

// This is a test-only unencrypted store containing synthetic values. Production
// implements the same CAS capability with an account-private encrypted store.
struct SecretHost {
    db: Mutex<Connection>,
    http: reqwest::Client,
    origin: String,
}
impl SecretHost {
    fn open(path: &Path, origin: &str) -> Self {
        let db = Connection::open(path).unwrap();
        db.execute_batch("CREATE TABLE IF NOT EXISTS secret (id TEXT PRIMARY KEY, revision INTEGER NOT NULL, payload TEXT NOT NULL);").unwrap();
        Self {
            db: Mutex::new(db),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            origin: origin.into(),
        }
    }
}
impl ClaudeSubscriptionHost for SecretHost {
    fn load<'a>(
        &'a self,
        key: &'a str,
    ) -> ClaudeAuthFuture<'a, Result<ClaudeSubscriptionStoreValue, ClaudeSubscriptionHostError>>
    {
        Box::pin(async move {
            self.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT revision,payload FROM secret WHERE id=?1",
                    [key],
                    |row| {
                        Ok(ClaudeSubscriptionStoreValue {
                            revision: u64::try_from(row.get::<_, i64>(0)?).unwrap(),
                            payload: Some(row.get(1)?),
                        })
                    },
                )
                .optional()
                .map(Option::unwrap_or_default)
                .map_err(|_| ClaudeSubscriptionHostError)
        })
    }
    fn compare_and_swap<'a>(
        &'a self,
        key: &'a str,
        revision: u64,
        payload: &'a str,
    ) -> ClaudeAuthFuture<'a, Result<ClaudeSubscriptionCommit, ClaudeSubscriptionHostError>> {
        Box::pin(async move {
            let mut db = self.db.lock().unwrap();
            let tx = db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|_| ClaudeSubscriptionHostError)?;
            let current: i64 = tx
                .query_row("SELECT revision FROM secret WHERE id=?1", [key], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(|_| ClaudeSubscriptionHostError)?
                .unwrap_or(0);
            let current = u64::try_from(current).unwrap();
            if current != revision {
                return Ok(ClaudeSubscriptionCommit::Conflict(current));
            }
            tx.execute("INSERT INTO secret VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,payload=excluded.payload", params![key, i64::try_from(revision + 1).unwrap(), payload]).map_err(|_| ClaudeSubscriptionHostError)?;
            tx.commit().map_err(|_| ClaudeSubscriptionHostError)?;
            Ok(ClaudeSubscriptionCommit::Committed(revision + 1))
        })
    }
    fn request(
        &self,
        request: ClaudeSubscriptionHttpRequest,
    ) -> ClaudeAuthFuture<'_, Result<ClaudeSubscriptionHttpResponse, ClaudeSubscriptionHostError>>
    {
        Box::pin(async move {
            // Test networking can never escape its loopback provider fixture.
            if !request.url().starts_with(&format!("{}/", self.origin)) {
                return Err(ClaudeSubscriptionHostError);
            }
            let method = reqwest::Method::from_bytes(request.method().as_bytes())
                .map_err(|_| ClaudeSubscriptionHostError)?;
            let response = self
                .http
                .request(method, request.url())
                .headers(request.headers().clone())
                .header("content-type", request.content_type())
                .timeout(Duration::from_millis(request.timeout_millis()))
                .body(request.body().to_owned())
                .send()
                .await
                .map_err(|_| ClaudeSubscriptionHostError)?;
            let status = response.status().as_u16();
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| ClaudeSubscriptionHostError)?;
                if bytes.len().saturating_add(chunk.len()) > request.max_response_bytes() {
                    return Err(ClaudeSubscriptionHostError);
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(ClaudeSubscriptionHttpResponse {
                status,
                body: String::from_utf8(bytes).map_err(|_| ClaudeSubscriptionHostError)?,
            })
        })
    }
}
#[derive(Default)]
struct Provider {
    exchanges: AtomicUsize,
    revocations: AtomicUsize,
    verifier: Mutex<Option<String>>,
    accept_original: AtomicBool,
    attempts: AtomicUsize,
    accepted: Mutex<Vec<Value>>,
}
fn sse(blocks: Vec<Value>, stop: &str) -> String {
    let mut frames = vec![
        json!({"type":"message_start","message":{"id":"synthetic-reply","role":"assistant","model":"synthetic","content":[],"usage":{"input_tokens":20,"cache_read_input_tokens":40,"cache_creation_input_tokens":10,"output_tokens":0}}}),
    ];
    for (index, block) in blocks.into_iter().enumerate() {
        frames.push(json!({"type":"content_block_start","index":index,"content_block":block}));
        frames.push(json!({"type":"content_block_stop","index":index}));
    }
    frames.push(
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":5}}),
    );
    frames.push(json!({"type":"message_stop"}));
    frames
        .into_iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect()
}
fn subscription(path: &Path, origin: &str) -> Arc<ClaudeSubscription> {
    Arc::new(
        ClaudeSubscription::new(
            Arc::new(SecretHost::open(path, origin)),
            "synthetic-account",
            ClaudeSubscriptionConfig {
                authorize_url: format!("{origin}/authorize"),
                token_url: format!("{origin}/token"),
                profile_url: format!("{origin}/profile"),
                manual_redirect_uri: format!("{origin}/callback"),
                allow_loopback_http: true,
                ..Default::default()
            },
        )
        .unwrap(),
    )
}
async fn build_agent(
    path: &Path,
    origin: &str,
    subscription: Arc<ClaudeSubscription>,
    effects: Arc<AtomicUsize>,
) -> Nanocodex {
    let client = ClaudeClient::with_auth_provider(
        reqwest::Client::new(),
        format!("{origin}/v1/messages?beta=true"),
        subscription,
    )
    .subscription_compatibility();
    Nanocodex::builder(Claude::new(client, "synthetic"))
        .system("Use the synthetic effect; keep its result.")
        .cache_one_hour()
        .keep_thinking()
        .tool(
            ToolDefinition {
                name: "effect".into(),
                description: "Synthetic durable effect".into(),
                input_schema: json!({"type":"object"}),
                strict: None,
                defer_loading: false,
            },
            move |_| {
                effects.fetch_add(1, Ordering::SeqCst);
                async { Ok("synthetic-effect-complete".into()) }
            },
        )
        .durability(
            DurableSession::open(SqliteStore::open(path).unwrap(), "synthetic-session")
                .await
                .unwrap(),
        )
        .await
        .unwrap()
        .build()
        .unwrap()
        .0
}
fn assert_no_secrets(path: &Path, secrets: &[String]) {
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
    ] {
        if let Ok(bytes) = std::fs::read(candidate) {
            for secret in secrets {
                assert!(
                    !bytes
                        .windows(secret.len())
                        .any(|part| part == secret.as_bytes())
                );
            }
        }
    }
}

#[tokio::test]
async fn subscription_rotation_tool_compaction_and_terminal_replay_survive_sqlite_reopen() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let provider = Arc::new(Provider::default());
    provider.accept_original.store(true, Ordering::SeqCst);
    let token_provider = provider.clone();
    let message_provider = provider.clone();
    let revoke_provider = provider.clone();
    let app = Router::new()
        .route("/token", post(move |Json(body): Json<Value>| {
            let provider = token_provider.clone();
            async move {
                let generation = provider.exchanges.fetch_add(1, Ordering::SeqCst) + 1;
                assert_eq!(body["grant_type"], if generation == 1 { "authorization_code" } else { "refresh_token" });
                if generation > 1 { assert_eq!(body["refresh_token"], "synthetic-refresh-1"); }
                else { *provider.verifier.lock().unwrap() = Some(body["code_verifier"].as_str().unwrap().to_owned()); }
                Json(json!({"access_token":format!("synthetic-access-{generation}"),"refresh_token":format!("synthetic-refresh-{generation}"),"expires_in":3600,"refresh_token_expires_in":86400,"scope":"user:profile user:inference user:sessions:claude_code","account":{"uuid":"synthetic-account"},"organization":{"uuid":"synthetic-org"}}))
            }
        }))
        .route("/profile", get(|| async { Json(json!({"account":{"uuid":"synthetic-account","email":"fixture@example.invalid"},"organization":{"uuid":"synthetic-org","organization_type":"claude_pro"}})) }))
        .route("/token/revoke", post(move |Json(body): Json<Value>| {
            let provider = revoke_provider.clone();
            async move {
                assert_eq!(body["token"], "synthetic-refresh-2");
                assert_eq!(body["token_type_hint"], "refresh_token");
                provider.revocations.fetch_add(1, Ordering::SeqCst);
                StatusCode::OK
            }
        }))
        .route("/v1/messages", post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let provider = message_provider.clone();
            async move {
                provider.attempts.fetch_add(1, Ordering::SeqCst);
                assert!(!headers.contains_key("x-api-key"));
                assert_eq!(headers["user-agent"], "claude-cli/2.1.280 (external, cli)");
                assert_eq!(headers["x-app"], "cli");
                assert!(!headers.contains_key("x-claude-code-request-class"));
                assert_eq!(headers["anthropic-dangerous-direct-browser-access"], "true");
                assert!(body["system"][0]["text"].as_str().unwrap().starts_with("x-anthropic-billing-header: cc_version=2.1.280."));
                assert_eq!(body["system"][1]["text"], "You are Claude Code, Anthropic's official CLI for Claude.");
                assert_eq!(body["system"][2]["text"], "Use the synthetic effect; keep its result.");
                assert_eq!(body["system"][2]["cache_control"]["ttl"], "1h");
                let betas = headers["anthropic-beta"].to_str().unwrap();
                assert!(betas.split(',').any(|b| b == "oauth-2025-04-20"));
                assert!(betas.split(',').any(|b| b == "context-management-2025-06-27"));
                for beta in ["claude-code-20250219", "interleaved-thinking-2025-05-14", "fallback-credit-2026-06-01"] {
                    assert_eq!(betas.split(',').filter(|b| *b == beta).count(), 1);
                }
                assert!(!betas.split(',').any(|b|b=="extended-cache-ttl-2025-04-11"));
                assert_eq!(betas.split(',').filter(|b|*b=="effort-2025-11-24").count(),usize::from(body.get("thinking").is_some()));
                // Claude Code shape: no top-level automatic field; the 1h
                // marker sits on the final cacheable block of the request.
                assert!(body.get("cache_control").is_none());
                let tail = body["messages"].as_array().unwrap().last().unwrap()["content"].as_array().unwrap().last().unwrap().clone();
                assert_eq!(tail["cache_control"], json!({"type":"ephemeral","ttl":"1h"}));
                assert_eq!(body.to_string().matches("\"cache_control\"").count(), 3);
                let authorization = headers["authorization"].to_str().unwrap();
                if !provider.accept_original.load(Ordering::SeqCst) && authorization == "Bearer synthetic-access-1" {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                assert_eq!(authorization, if provider.accept_original.load(Ordering::SeqCst) { "Bearer synthetic-access-1" } else { "Bearer synthetic-access-2" });
                let index = { let mut requests = provider.accepted.lock().unwrap(); requests.push(body.clone()); requests.len() };
                let (blocks, reason) = match index {
                    1 => (vec![json!({"type":"thinking","thinking":"","signature":"synthetic-signed"}),json!({"type":"tool_use","id":"effect-1","name":"_effect","input":{}})], "tool_use"),
                    2 => { assert!(body.to_string().contains("synthetic-effect-complete")); (vec![json!({"type":"text","text":"effect stored"})], "end_turn") },
                    3 => (vec![json!({"type":"text","text":"Summary: synthetic effect completed; preserve result."})], "end_turn"),
                    4 => { assert!(body.to_string().contains("Summary: synthetic effect completed")); (vec![json!({"type":"text","text":"continued after reopen"})], "end_turn") },
                    _ => panic!("unexpected provider request"),
                };
                ([("content-type","text/event-stream")], sse(blocks, reason)).into_response()
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let secrets_path = dir.path().join("synthetic-secrets.sqlite");
    let agent_path = dir.path().join("agent.sqlite");
    let auth = subscription(&secrets_path, &origin);
    let login = auth.begin_login(ClaudeLoginMode::Manual).await.unwrap();
    let url = reqwest::Url::parse(&login.authorization_url).unwrap();
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .to_string();
    // Reopen the secret manager while login is pending; PKCE continuity is durable.
    drop(auth);
    let auth = subscription(&secrets_path, &origin);
    auth.complete_login(&format!("synthetic-code#{state}"))
        .await
        .unwrap();
    let effects = Arc::new(AtomicUsize::new(0));
    let agent = build_agent(&agent_path, &origin, auth.clone(), effects.clone()).await;
    let first = agent
        .prompt(PromptRequest::new("run synthetic effect").request_id("first"))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(first.final_message(), "effect stored");
    agent.compact().await.unwrap();
    agent.shutdown().await.unwrap();
    drop(agent);
    drop(auth);
    provider.accept_original.store(false, Ordering::SeqCst);
    let auth = subscription(&secrets_path, &origin);
    let agent = build_agent(&agent_path, &origin, auth.clone(), effects.clone()).await;
    let second = || PromptRequest::new("continue with recorded result").request_id("second");
    let result = agent
        .prompt(second())
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "continued after reopen");
    assert_eq!(provider.exchanges.load(Ordering::SeqCst), 2);
    assert_eq!(provider.attempts.load(Ordering::SeqCst), 5);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    agent.shutdown().await.unwrap();
    drop(agent);
    drop(auth);
    let auth = subscription(&secrets_path, &origin);
    let agent = build_agent(&agent_path, &origin, auth, effects.clone()).await;
    assert_eq!(
        agent
            .prompt(second())
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "continued after reopen"
    );
    assert_eq!(provider.attempts.load(Ordering::SeqCst), 5);
    assert_eq!(provider.exchanges.load(Ordering::SeqCst), 2);
    subscription(&secrets_path, &origin).logout().await.unwrap();
    assert!(
        agent
            .prompt("auth should be required")
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    assert_eq!(provider.attempts.load(Ordering::SeqCst), 5);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    agent.shutdown().await.unwrap();
    assert_eq!(provider.revocations.load(Ordering::SeqCst), 1);
    let verifier = provider.verifier.lock().unwrap().clone().unwrap();
    assert_no_secrets(
        &agent_path,
        &[
            verifier,
            state,
            "synthetic-code".into(),
            "synthetic-access-1".into(),
            "synthetic-access-2".into(),
            "synthetic-refresh-1".into(),
            "synthetic-refresh-2".into(),
        ],
    );
    server.abort();
    eprintln!(
        "subscription+SQLite: login/reopen -> signed tool roundtrip -> compact -> reopen/401/rotation -> terminal replay -> logout; 2 token exchanges, 5 Messages attempts, 1 effect; credentials absent from agent store"
    );
}

// One synthetic storage outage after the real HTTP response leaves the native
// request frozen. All successful persistence still uses actual SQLite.
struct InterruptedStore {
    inner: SqliteStore,
    armed: Arc<AtomicBool>,
}
impl nanocodex_durability::StateStore for InterruptedStore {
    fn read_record<'a>(
        &'a mut self,
        id: &'a str,
        key: &'a str,
    ) -> nanocodex_durability::StoreFuture<
        'a,
        Result<Option<String>, nanocodex_durability::StoreError>,
    > {
        self.inner.read_record(id, key)
    }
    fn acquire<'a>(
        &'a mut self,
        id: &'a str,
        owner: nanocodex_durability::OwnerId,
    ) -> nanocodex_durability::StoreFuture<
        'a,
        Result<nanocodex_durability::OwnedState, nanocodex_durability::StoreError>,
    > {
        self.inner.acquire(id, owner)
    }
    fn replace<'a>(
        &'a mut self,
        id: &'a str,
        owner: &'a nanocodex_durability::OwnerToken,
        revision: u64,
        payload: &'a str,
        records: &'a [nanocodex_durability::StoreRecord],
    ) -> nanocodex_durability::StoreFuture<'a, Result<u64, nanocodex_durability::StoreError>> {
        Box::pin(async move {
            if self.armed.swap(false, Ordering::SeqCst) {
                return Err(nanocodex_durability::StoreError::NotCommitted(
                    "synthetic response receipt storage outage".into(),
                ));
            }
            self.inner
                .replace(id, owner, revision, payload, records)
                .await
        })
    }
}

#[tokio::test]
async fn frozen_subscription_request_survives_sqlite_reopen_with_changed_client_profile() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for originally_enabled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.sqlite");
        let armed = Arc::new(AtomicBool::new(false));
        let arm = armed.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let raw_wires = Arc::new(Mutex::new(Vec::<String>::new()));
        let raw_capture = raw_wires.clone();
        let app = Router::new().route(
            "/v1/messages",
            post(move |headers: HeaderMap, wire: String| {
                let (captured, arm, raw_capture) =
                    (captured.clone(), arm.clone(), raw_capture.clone());
                async move {
                    raw_capture.lock().unwrap().push(wire.clone());
                    let body: Value = serde_json::from_str(&wire).unwrap();
                    assert_eq!(headers["authorization"], "Bearer synthetic-frozen-secret");
                    let index = {
                        let mut log = captured.lock().unwrap();
                        log.push(body);
                        log.len()
                    };
                    if index == 1 {
                        arm.store(true, Ordering::SeqCst);
                    }
                    (
                        [("content-type", "text/event-stream")],
                        sse(
                            vec![json!({"type":"text","text":"frozen answer"})],
                            "end_turn",
                        ),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let generation = AtomicUsize::new(0);
        let client = |enabled: bool| {
            let phase = generation.fetch_add(1, Ordering::SeqCst);

            let mut headers = HeaderMap::new();
            headers.insert(
                "authorization",
                "Bearer synthetic-frozen-secret".parse().unwrap(),
            );
            let client =
                ClaudeClient::with_auth_headers(reqwest::Client::new(), &endpoint, headers);
            let client = if enabled {
                client.subscription_compatibility()
            } else {
                client
            };
            client.with_subscription_identity(nanocodex_claude::SubscriptionIdentity {
                install_id: Some(format!("synthetic-install-{phase}")),
                version: Some(if phase == 0 { "2.1.280" } else { "9.8.7" }.into()),
                platform: Some(if phase == 0 { "darwin" } else { "linux" }.into()),
                ..Default::default()
            })
        };
        let state = DurableSession::open(
            InterruptedStore {
                inner: SqliteStore::open(&path).unwrap(),
                armed,
            },
            "profile-session",
        )
        .await
        .unwrap();
        let caller = json!({"type":"text", "text":"Original caller policy", "cache_control":{"type":"ephemeral","ttl":"1h"}});
        let (agent, events) =
            Nanocodex::builder(Claude::new(client(originally_enabled), "original-model"))
                .system_blocks(vec![caller.clone()])
                .durability(state)
                .await
                .unwrap()
                .build()
                .unwrap();
        let request =
            || PromptRequest::new("retain the original request").request_id("frozen-profile");
        let error = agent
            .prompt(request())
            .await
            .unwrap()
            .result()
            .await
            .unwrap_err();
        assert!(error.execution_policy_disposition().is_some(), "{error}");
        assert_eq!(requests.lock().unwrap().len(), 1);
        let _ = agent.shutdown().await;
        drop((agent, events));
        let state = DurableSession::open(SqliteStore::open(&path).unwrap(), "profile-session")
            .await
            .unwrap();
        let (agent, events) =
            Nanocodex::builder(Claude::new(client(!originally_enabled), "changed-model"))
                .system("Changed caller policy")
                .durability(state)
                .await
                .unwrap()
                .build()
                .unwrap();
        assert_eq!(
            agent
                .prompt(request())
                .await
                .unwrap()
                .result()
                .await
                .unwrap()
                .final_message(),
            "frozen answer"
        );
        assert_eq!(
            agent
                .prompt(request())
                .await
                .unwrap()
                .result()
                .await
                .unwrap()
                .final_message(),
            "frozen answer"
        );
        assert_eq!(
            requests.lock().unwrap().len(),
            2,
            "terminal replay sends no HTTP"
        );
        agent
            .prompt("new request uses the current profile")
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        agent.shutdown().await.unwrap();
        drop((agent, events));
        let log = requests.lock().unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(
            log[0], log[1],
            "resumed HTTP body must match the frozen body despite profile and builder changes"
        );
        let raw = raw_wires.lock().unwrap();
        assert_eq!(
            raw[0], raw[1],
            "frozen exact attested UTF8 bytes survive changed profile/version/install/platform"
        );
        if originally_enabled {
            assert!(
                log[0]["system"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("cc_version=2.1.280.")
            );
        } else {
            assert!(
                log[2]["system"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("cc_version=9.8.7.")
            );
        }
        let expected = if originally_enabled {
            json!([log[0]["system"][0], {"type":"text", "text":"You are Claude Code, Anthropic's official CLI for Claude.","cache_control":{"type":"ephemeral","ttl":"1h"}}, caller])
        } else {
            json!([caller])
        };
        assert_eq!(log[0]["system"], expected);
        assert_eq!(log[0]["model"], "original-model");
        assert_eq!(log[2]["model"], "changed-model");
        if originally_enabled {
            assert_eq!(log[2]["system"], "Changed caller policy");
        } else {
            assert_eq!(
                log[2]["system"][1]["text"],
                "You are Claude Code, Anthropic's official CLI for Claude."
            );
            assert_eq!(log[2]["system"][2]["text"], "Changed caller policy");
        }
        assert_no_secrets(&path, &["synthetic-frozen-secret".into()]);
        server.abort();
        eprintln!(
            "profile SQLite recovery: initial profile={originally_enabled}, changed profile={}, frozen HTTP body identical, cache marker preserved, 2 recovery attempts + 1 new request, terminal replay has zero dispatch; auth absent from DB",
            !originally_enabled
        );
    }
}
