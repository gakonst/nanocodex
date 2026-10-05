//! Public native HTTP journeys, including a branch longer than the old 256-request cap.
#![cfg(all(feature = "claude", feature = "sqlite"))]

use axum::{Json, Router, response::IntoResponse, routing::post};
use nanocodex_agent::{
    ClaudeModel, HarnessModel, Model, Nanocodex, OpenAi, PromptRequest, Thinking,
};
use nanocodex_claude::{Claude, ClaudeClient};
use nanocodex_durability::{
    DurableSession, OwnedState, OwnerId, OwnerToken, SqliteStore, StateStore, StoreError,
    StoreFuture, StoreRecord,
    cache_warm::CacheWarmPolicy,
    request_policy::{
        ConfigurationPatch, DurableClaudeRequestExt, DurableOpenAiRequestExt, PhysicalModel,
        PromptSection, RequestPolicySettings, RoutingChoice, RoutingInput,
    },
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

pub(super) fn settings(
    model: HarnessModel,
    section: &str,
    calls: Arc<AtomicUsize>,
) -> RequestPolicySettings {
    RequestPolicySettings {
        selection: "virtual-reviewer".into(),
        patches: vec![ConfigurationPatch::SetSection {
            section: PromptSection {
                name: "task-policy".into(),
                text: section.into(),
            },
        }],
        models: vec![PhysicalModel {
            model,
            context_tokens: 4_000_000,
            max_output_tokens: 32,
            switch_group: Some("explicit-text-only-group".into()),
        }],
        router: Arc::new(move |input: RoutingInput<'_>| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(RoutingChoice {
                dispatched: model,
                state: json!({"turns":input.state["turns"].as_u64().unwrap_or(0) + 1}),
            })
        }),
    }
}

pub(super) fn claude_sse(blocks: Vec<Value>, stop: &str) -> String {
    let mut frames = vec![json!({"type":"message_start","message":{
        "id":"local-response","role":"assistant","model":"claude-sonnet-5-5",
        "content":[],"usage":{"input_tokens":10,"output_tokens":0}}})];
    for (index, block) in blocks.into_iter().enumerate() {
        frames.push(json!({"type":"content_block_start","index":index,"content_block":block}));
        frames.push(json!({"type":"content_block_stop","index":index}));
    }
    frames.push(
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":2}}),
    );
    frames.push(json!({"type":"message_stop"}));
    frames
        .into_iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect()
}

fn openai_sse(index: usize) -> String {
    let item = json!({"id":format!("msg-{index}"),"type":"message","role":"assistant",
        "status":"completed","content":[{"type":"output_text","text":"recorded answer","annotations":[]}]});
    [json!({"type":"response.output_item.added","output_index":0,
            "item":{"id":format!("msg-{index}"),"type":"message","role":"assistant","status":"in_progress","content":[]}}),
        json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"recorded answer"}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":{"id":format!("resp-{index}"),"status":"completed",
            "output":[item],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12,
            "input_tokens_details":{"cached_tokens":0},"output_tokens_details":{"reasoning_tokens":0}}}})]
        .into_iter().map(|frame| format!("data: {frame}\n\n")).collect()
}

#[derive(Default)]
struct JournalAudit {
    prepared: BTreeMap<String, Value>,
    max_head_bytes: usize,
    history: BTreeMap<String, String>,
    last_head: String,
}
struct AuditedSqlite {
    inner: SqliteStore,
    audit: Arc<Mutex<JournalAudit>>,
}
impl StateStore for AuditedSqlite {
    fn read_record<'a>(
        &'a mut self,
        id: &'a str,
        key: &'a str,
    ) -> StoreFuture<'a, Result<Option<String>, StoreError>> {
        self.inner.read_record(id, key)
    }
    fn acquire<'a>(
        &'a mut self,
        id: &'a str,
        owner: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedState, StoreError>> {
        self.inner.acquire(id, owner)
    }
    fn replace<'a>(
        &'a mut self,
        id: &'a str,
        owner: &'a OwnerToken,
        revision: u64,
        payload: &'a str,
        records: &'a [StoreRecord],
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move {
            let revision = self
                .inner
                .replace(id, owner, revision, payload, records)
                .await?;
            let mut audit = self.audit.lock().unwrap();
            audit.max_head_bytes = audit.max_head_bytes.max(payload.len());
            audit.last_head = payload.to_owned();
            for record in records {
                // Chunk records contain raw text, not payload envelopes.
                if record.key.contains('/') {
                    continue;
                }
                // StateStore journals use an inline '=' envelope or a '+' chunk
                // manifest. Inspect the committed public record representation.
                let json = if let Some(content) = record.value.strip_prefix('=') {
                    content.to_owned()
                } else if let Some(count) = record.value.strip_prefix('+') {
                    let count: usize = count.parse().expect("chunk manifest count");
                    (0..count)
                        .map(|index| {
                            let key = format!("{}/{index}", record.key);
                            records
                                .iter()
                                .find(|part| part.key == key)
                                .map(|part| part.value.as_str())
                                .expect("staged payload chunk")
                        })
                        .collect::<String>()
                } else {
                    continue;
                };
                if let Ok(value) = serde_json::from_str::<Value>(&json)
                    && value.get("request").is_some()
                    && let Some(receipt) = value["state"]["requests"]
                        .as_array()
                        .and_then(|items| items.last())
                    && let Some(id) = receipt["request"]["request_id"].as_str()
                {
                    audit
                        .prepared
                        .entry(id.into())
                        .or_insert_with(|| receipt.clone());
                    for retained in records.iter().filter(|part| {
                        part.key == record.key || part.key.starts_with(&format!("{}/", record.key))
                    }) {
                        if let Some(previous) = audit
                            .history
                            .insert(retained.key.clone(), retained.value.clone())
                        {
                            assert_eq!(
                                previous, retained.value,
                                "an immutable preparation record was rewritten"
                            );
                        }
                    }
                }
            }
            Ok(revision)
        })
    }
}

fn assert_tip(policy: &Value, turns: u64) {
    let requests = policy["requests"]
        .as_array()
        .expect("native routing checkpoint");
    assert!(!requests.is_empty());
    assert!(
        requests.len() <= 16,
        "current policy must retain only a bounded recent window"
    );
    assert_eq!(requests.last().unwrap()["route"]["state"]["turns"], turns);
    assert!(policy["configuration"]["entries"].as_array().unwrap().len() <= 16);
}

#[tokio::test]
async fn request_policy_public_openai_and_claude_http_survive_270_boundaries_and_retained_replay()
-> eyre::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for is_claude in [false, true] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.sqlite");
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let log = requests.clone();
        let app = Router::new().route(
            if is_claude {
                "/v1/messages"
            } else {
                "/responses"
            },
            post(move |Json(body): Json<Value>| {
                let log = log.clone();
                async move {
                    let index = {
                        let mut log = log.lock().unwrap();
                        log.push(body);
                        log.len()
                    };
                    let output = if is_claude {
                        claude_sse(
                            vec![json!({"type":"text","text":"recorded answer"})],
                            "end_turn",
                        )
                    } else {
                        openai_sse(index)
                    };
                    ([("content-type", "text/event-stream")], output).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let route_calls = Arc::new(AtomicUsize::new(0));
        let audit = Arc::new(Mutex::new(JournalAudit::default()));
        let model = if is_claude {
            HarnessModel::Claude(ClaudeModel::Sonnet55)
        } else {
            HarnessModel::Codex(Model::Sol)
        };
        for epoch in 0..5 {
            let state = DurableSession::open_with_terminal_receipt_limit(
                AuditedSqlite {
                    inner: SqliteStore::open(&path)?,
                    audit: audit.clone(),
                },
                "native-policy-journey",
                8,
            )
            .await?;
            let policy = settings(
                model,
                &format!("Named section at epoch {epoch}"),
                route_calls.clone(),
            );
            if is_claude {
                let client = ClaudeClient::new(
                    reqwest::Client::new(),
                    format!("http://{address}/v1/messages"),
                    "test-key",
                );
                let (agent, events) = Nanocodex::builder(Claude::new(client, "claude-opus-5-5"))
                    .max_tokens(32)
                    .system("Permanent base instruction")
                    .durability_with_request_policy(state.clone(), policy)
                    .await?
                    .build()?;
                for index in epoch * 54..(epoch + 1) * 54 {
                    let result = agent
                        .prompt(
                            PromptRequest::new(format!("ordered user turn {index}"))
                                .request_id(format!("turn-{index}")),
                        )
                        .await?
                        .result()
                        .await?;
                    assert_eq!(result.final_message(), "recorded answer");
                    assert_eq!(result.usage().unwrap().total_tokens(), 12);
                }
                if epoch == 4 {
                    let before = requests.lock().unwrap().len();
                    assert_eq!(
                        agent
                            .prompt(
                                PromptRequest::new("ordered user turn 269").request_id("turn-269")
                            )
                            .await?
                            .result()
                            .await?
                            .final_message(),
                        "recorded answer"
                    );
                    let conflict = agent
                        .prompt(PromptRequest::new("changed retained turn").request_id("turn-269"))
                        .await;
                    assert!(match conflict {
                        Ok(turn) => turn.result().await.is_err(),
                        Err(_) => true,
                    });
                    assert_eq!(requests.lock().unwrap().len(), before);
                }
                agent.shutdown().await?;
                drop((agent, events));
                let checkpoint: Value = state.latest_checkpoint().await?.unwrap().decode()?;
                assert_tip(
                    &checkpoint["conversation"]["request_policy"],
                    ((epoch + 1) * 54) as u64,
                );
            } else {
                let openai = OpenAi::builder("test-key")
                    .transport(nanocodex_oai_api::transport::ResponsesTransport::Https)
                    .api_base_url(format!("http://{address}"))
                    .build()?;
                let (agent, events) = Nanocodex::builder(openai)
                    .workspace(directory.path())
                    .instructions("Permanent base instruction")
                    .model(Model::Astra)
                    .thinking(Thinking::Low)
                    .durability_with_request_policy(state.clone(), policy)
                    .await?
                    .build()?;
                for index in epoch * 54..(epoch + 1) * 54 {
                    let result = agent
                        .prompt(
                            PromptRequest::new(format!("ordered user turn {index}"))
                                .request_id(format!("turn-{index}")),
                        )
                        .await?
                        .result()
                        .await?;
                    assert_eq!(result.final_message(), "recorded answer");
                    assert_eq!(result.usage().unwrap().total_tokens(), 12);
                }
                if epoch == 4 {
                    let before = requests.lock().unwrap().len();
                    assert_eq!(
                        agent
                            .prompt(
                                PromptRequest::new("ordered user turn 269").request_id("turn-269")
                            )
                            .await?
                            .result()
                            .await?
                            .final_message(),
                        "recorded answer"
                    );
                    let conflict = agent
                        .prompt(PromptRequest::new("changed retained turn").request_id("turn-269"))
                        .await;
                    assert!(match conflict {
                        Ok(turn) => turn.result().await.is_err(),
                        Err(_) => true,
                    });
                    assert_eq!(requests.lock().unwrap().len(), before);
                }
                agent.shutdown().await?;
                drop((agent, events));
                let checkpoint = serde_json::to_value(state.agent_snapshot().await?.unwrap())?;
                assert_tip(&checkpoint["request_policy"], ((epoch + 1) * 54) as u64);
            }
            assert!(state.state().await?.operations().len() <= 8);
            assert!(state.state().await?.pending_operations().is_empty());
        }
        assert_eq!(
            route_calls.load(Ordering::SeqCst),
            270,
            "restart/replay may not evaluate the router again"
        );
        {
            let log = requests.lock().unwrap();
            assert_eq!(log.len(), 270);
            for (index, request) in log.iter().enumerate() {
                assert_eq!(request["model"], model.as_str());
                if !is_claude {
                    assert_eq!(request["prompt_cache_key"], "native-policy-journey");
                }
                assert_eq!(
                    request[if is_claude {
                        "max_tokens"
                    } else {
                        "max_output_tokens"
                    }],
                    32
                );
                let encoded = request.to_string();
                assert!(encoded.contains(&format!("Named section at epoch {}", index / 54)));
                assert_eq!(
                    encoded.matches("Named section at epoch").count(),
                    1,
                    "flattened configuration must not accumulate stale sections"
                );
                if index >= 54 {
                    assert!(
                        encoded.contains("ordered user turn 0"),
                        "native original transcript survives reopen"
                    );
                }
            }
        }
        let historical_records = audit.lock().unwrap().history.clone();
        let mut stored = SqliteStore::open(&path)?;
        for (key, expected) in &historical_records {
            assert_eq!(
                stored
                    .read_record("native-policy-journey", key)
                    .await?
                    .as_ref(),
                Some(expected),
                "historical native preparation {key} must remain readable after 270 turns"
            );
        }
        let audit = audit.lock().unwrap();
        assert_eq!(
            audit.prepared.len(),
            270,
            "all historical decisions remain in immutable journal records"
        );
        assert!(
            audit.max_head_bytes < 64 * 1024,
            "current durable head grew with history: {}",
            audit.max_head_bytes
        );
        let mut receipts: Vec<_> = audit.prepared.values().collect();
        receipts.sort_by_key(|receipt| receipt["route"]["state"]["turns"].as_u64().unwrap());
        for (index, receipt) in receipts.into_iter().enumerate() {
            assert_eq!(receipt["request"]["selection"], "virtual-reviewer");
            assert_eq!(
                receipt["route"]["dispatched"].as_str(),
                Some(model.as_str())
            );
            assert_eq!(
                receipt["configuration"]["sections"][0]["text"],
                format!("Named section at epoch {}", index / 54)
            );
        }
        eprintln!(
            "native-policy evidence: provider={} turns={} router_calls={} immutable_preparations={} max_head_bytes={} recent_receipts<=16 terminal_receipts<=8 retained_replay=no_HTTP",
            model.as_str(),
            requests.lock().unwrap().len(),
            route_calls.load(Ordering::SeqCst),
            audit.prepared.len(),
            audit.max_head_bytes
        );
        server.abort();
    }
    Ok(())
}

const fn warm_policy() -> CacheWarmPolicy {
    CacheWarmPolicy {
        ttl_seconds: 300,
        max_spend_usd: 0.0007,
        estimated_write_usd: 0.0003,
        estimated_read_usd: 0.0001,
        estimated_uncached_usd: 0.001,
        reuse_probability: 1.0,
        expected_reuse_count: 3,
        expected_reuse_within_seconds: 120,
        input_usd_per_million: 2.0,
        output_usd_per_million: 20.0,
        cache_write_usd_per_million: 4.0,
        cache_read_usd_per_million: 1.0,
    }
}

#[tokio::test]
async fn request_policy_claude_http_warm_accounts_actual_usage_replays_and_enforces_spend()
-> eyre::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("state.sqlite");
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let audit = Arc::new(Mutex::new(JournalAudit::default()));
    let app = Router::new().route("/v1/messages", post({
        let requests = requests.clone();
        let audit = audit.clone();
        move |Json(body): Json<Value>| {
            let requests = requests.clone();
            let audit = audit.clone();
            async move {
                let warm = body["stream"] != true;
                if warm {
                    let head = audit.lock().unwrap().last_head.clone();
                    assert!(head.contains("cache_warm_http") && head.contains("effect_pending"),
                        "warm charge must be journaled before the provider receives HTTP");
                }
                requests.lock().unwrap().push(body);
                if warm {
                    Json(json!({"id":"warm","role":"assistant","model":"claude-sonnet-5-5",
                        "content":[{"type":"text","text":"."}],"stop_reason":"max_tokens",
                        "usage":{"input_tokens":10,"output_tokens":1,"cache_creation_input_tokens":100,
                            "cache_read_input_tokens":0}})).into_response()
                } else {
                    ([("content-type", "text/event-stream")],
                        claude_sse(vec![json!({"type":"text","text":"recorded answer"})], "end_turn"))
                        .into_response()
                }
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let calls = Arc::new(AtomicUsize::new(0));
    let model = HarnessModel::Claude(ClaudeModel::Sonnet55);
    for epoch in 0..2 {
        let state = DurableSession::open(
            AuditedSqlite {
                inner: SqliteStore::open(&path)?,
                audit: audit.clone(),
            },
            "warm-spend",
        )
        .await?;
        let client = ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "test-key",
        );
        let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "claude-opus-5-5"))
            .max_tokens(32)
            .automatic_cache(true)
            .system("Stable cached instruction")
            .durability_with_request_policy_and_cache_warm(
                state.clone(),
                settings(model, "Named cached section", calls.clone()),
                client,
                warm_policy(),
            )
            .await?
            .build()?;
        let before = requests.lock().unwrap().len();
        let result = agent
            .prompt(PromptRequest::new("reuse this prefix").request_id("warm-first"))
            .await?
            .result()
            .await?;
        assert_eq!(result.final_message(), "recorded answer");
        assert_eq!(
            requests.lock().unwrap().len(),
            if epoch == 0 { before + 2 } else { before },
            "completed replay cannot charge another warm or generation"
        );
        let checkpoint: Value = state.latest_checkpoint().await?.unwrap().decode()?;
        let policy = &checkpoint["conversation"]["request_policy"];
        let actual = policy["last_cache_warm"]["actual_usd"].as_f64().unwrap();
        assert!(
            (actual - 0.00044).abs() < 1e-12,
            "bill must use observed provider usage and supplied rates"
        );
        assert_eq!(
            policy["last_cache_warm"]["usage"]["cache_creation_input_tokens"],
            100
        );
        assert_eq!(policy["cache_warm_budget"]["actual_usd"], actual);
        assert_eq!(policy["cache_warm_budget"]["reserved_usd"], actual);
        if epoch == 1 {
            let before = requests.lock().unwrap().len();
            let failure = agent
                .prompt(PromptRequest::new("would exceed observed spend").request_id("warm-second"))
                .await?
                .result()
                .await
                .unwrap_err();
            assert!(failure.to_string().contains("spend limit"), "{failure}");
            assert_eq!(
                requests.lock().unwrap().len(),
                before,
                "actual spend gates the next HTTP charge"
            );
        }
        agent.shutdown().await?;
        drop((agent, events));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0]["model"], "claude-sonnet-5-5");
    assert_eq!(log[0]["max_tokens"], 1);
    assert_eq!(log[0]["tool_choice"], json!({"type":"none"}));
    assert_eq!(log[0]["cache_control"], json!({"type":"ephemeral"}));
    assert_eq!(log[1]["model"], log[0]["model"]);
    assert_eq!(log[1]["max_tokens"], 32);
    assert_eq!(log[1]["system"], log[0]["system"]);
    assert_eq!(log[1]["messages"], log[0]["messages"]);
    eprintln!(
        "native-warm evidence: dispatched={} HTTP=1_warm+1_generation actual_usd=0.00044 cache_write_tokens=100 reopened_replay=no_HTTP next_charge=spend_limit",
        log[0]["model"]
    );
    server.abort();
    Ok(())
}

#[tokio::test]
async fn request_policy_claude_http_unknown_warm_charge_is_not_repeated_after_reopen()
-> eyre::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("state.sqlite");
    let charges = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/messages",
        post({
            let charges = charges.clone();
            move |Json(body): Json<Value>| {
                let charges = charges.clone();
                async move {
                    assert_ne!(
                        body["stream"], true,
                        "generation cannot cross an uncertain warm charge"
                    );
                    charges.fetch_add(1, Ordering::SeqCst);
                    // The provider received the charge, but its reply is unreadable.
                    (
                        [("content-type", "application/json")],
                        "incomplete provider response",
                    )
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let calls = Arc::new(AtomicUsize::new(0));
    let mut safe_checkpoint = Value::Null;
    for epoch in 0..3 {
        let state = DurableSession::open(SqliteStore::open(&path)?, "uncertain-warm").await?;
        if epoch == 2 {
            // An embedding may explicitly abandon an uncertain operation using
            // its last safe native checkpoint. The charge reservation must
            // survive that terminal failure before accepting a different turn.
            let input = state
                .state()
                .await?
                .operation("uncertain")
                .unwrap()
                .input
                .clone();
            let input: Value = state.resolve(&input).await?.decode()?;
            state.admit("uncertain", &input).await?;
            state.begin_attempt("uncertain").await?;
            state
                .fail(
                    "uncertain",
                    &safe_checkpoint,
                    "host abandoned the turn with its charge still reserved",
                )
                .await?;
        }
        let client = ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "test-key",
        );
        let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "claude-opus-5-5"))
            .max_tokens(32)
            .automatic_cache(true)
            .system("Stable cached instruction")
            .durability_with_request_policy_and_cache_warm(
                state.clone(),
                settings(
                    HarnessModel::Claude(ClaudeModel::Sonnet55),
                    "Named cached section",
                    calls.clone(),
                ),
                client,
                CacheWarmPolicy {
                    max_spend_usd: 0.0003,
                    ..warm_policy()
                },
            )
            .await?
            .build()?;
        if epoch == 0 {
            let nanocodex_agent::ChildSnapshot::Native { payload, .. } =
                agent.runtime_snapshot().await?
            else {
                panic!("expected native Claude snapshot");
            };
            safe_checkpoint = serde_json::from_str::<Value>(&payload)?["snapshot"].clone();
        }
        let failure = agent
            .prompt(PromptRequest::new("charge once").request_id(if epoch < 2 {
                "uncertain"
            } else {
                "new-turn"
            }))
            .await?
            .result()
            .await
            .unwrap_err();
        if epoch == 1 {
            assert!(failure.to_string().contains("uncertain"), "{failure}");
        } else if epoch == 2 {
            assert!(failure.to_string().contains("spend limit"), "{failure}");
        }
        let budget = state
            .document("nanocodex.cache-warm.budget")
            .await?
            .unwrap();
        assert_eq!(budget.value["reserved_usd"], 0.0003);
        assert_eq!(budget.value["actual_usd"], 0.0);
        assert_eq!(
            charges.load(Ordering::SeqCst),
            1,
            "recovery may not issue a second possibly charged request"
        );
        let retained = state.state().await?;
        let operation = retained.operation("uncertain").unwrap();
        if epoch < 2 {
            assert!(
                operation
                    .steps
                    .values()
                    .any(|step| step.kind == "cache_warm_http"
                        && matches!(step.status, nanocodex_durability::StepStatus::EffectPending))
            );
        } else {
            assert!(matches!(
                operation.status,
                nanocodex_durability::OperationStatus::Failed { .. }
            ));
        }
        agent.shutdown().await?;
        drop((agent, events));
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "retry consumes the frozen route, while a new turn prepares once before the retained budget refuses dispatch"
    );
    eprintln!(
        "native-unknown-warm evidence: HTTP_charges={} route_evaluations={} unresolved_effect=pending reopen=no_second_charge new_turn=retained_spend_limit",
        charges.load(Ordering::SeqCst),
        calls.load(Ordering::SeqCst)
    );
    server.abort();
    Ok(())
}

#[tokio::test]
async fn request_policy_public_http_failure_replays_frozen_route_after_reopen() -> eyre::Result<()>
{
    let _ = rustls::crypto::ring::default_provider().install_default();
    for is_claude in [false, true] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.sqlite");
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let app = Router::new().route(
            if is_claude {
                "/v1/messages"
            } else {
                "/responses"
            },
            post({
                let requests = requests.clone();
                move |Json(body): Json<Value>| {
                    let requests = requests.clone();
                    async move {
                        let index = {
                            let mut log = requests.lock().unwrap();
                            log.push(body);
                            log.len()
                        };
                        if index == 1 {
                            (
                                axum::http::StatusCode::BAD_REQUEST,
                                Json(json!({"error":{"type":"invalid_request_error",
                            "message":"one deterministic rejected request"}})),
                            )
                                .into_response()
                        } else {
                            let sse = if is_claude {
                                claude_sse(
                                    vec![json!({"type":"text","text":"recorded answer"})],
                                    "end_turn",
                                )
                            } else {
                                openai_sse(index)
                            };
                            ([("content-type", "text/event-stream")], sse).into_response()
                        }
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let original = if is_claude {
            HarnessModel::Claude(ClaudeModel::Sonnet55)
        } else {
            HarnessModel::Codex(Model::Sol)
        };
        let replacement = if is_claude {
            HarnessModel::Claude(ClaudeModel::Opus55)
        } else {
            HarnessModel::Codex(Model::Astra)
        };
        let first_calls = Arc::new(AtomicUsize::new(0));
        let changed_calls = Arc::new(AtomicUsize::new(0));
        for epoch in 0..2 {
            let state = DurableSession::open(SqliteStore::open(&path)?, "sticky-route").await?;
            let mut policy = settings(
                if epoch == 0 { original } else { replacement },
                if epoch == 0 {
                    "Frozen named instruction"
                } else {
                    "Changed host instruction"
                },
                if epoch == 0 {
                    first_calls.clone()
                } else {
                    changed_calls.clone()
                },
            );
            policy.models = [original, replacement]
                .into_iter()
                .map(|model| PhysicalModel {
                    model,
                    context_tokens: 4_000_000,
                    max_output_tokens: 32,
                    switch_group: Some("explicit-text-only-group".into()),
                })
                .collect();
            if is_claude {
                let client = ClaudeClient::new(
                    reqwest::Client::new(),
                    format!("http://{address}/v1/messages"),
                    "test-key",
                );
                let (agent, events) = Nanocodex::builder(Claude::new(client, "claude-opus-5-5"))
                    .max_tokens(32)
                    .system("Permanent base instruction")
                    .durability_with_request_policy(state.clone(), policy)
                    .await?
                    .build()?;
                let result = agent
                    .prompt(PromptRequest::new("recover exact request").request_id("retry"))
                    .await?
                    .result()
                    .await;
                let failure = result.unwrap_err();
                assert!(
                    failure
                        .to_string()
                        .contains("one deterministic rejected request"),
                    "{failure}"
                );
                agent.shutdown().await?;
                drop((agent, events));
            } else {
                let openai = OpenAi::builder("test-key")
                    .transport(nanocodex_oai_api::transport::ResponsesTransport::Https)
                    .api_base_url(format!("http://{address}"))
                    .build()?;
                let (agent, events) = Nanocodex::builder(openai)
                    .workspace(directory.path())
                    .instructions("Permanent base instruction")
                    .model(Model::Astra)
                    .thinking(Thinking::Low)
                    .durability_with_request_policy(state.clone(), policy)
                    .await?
                    .build()?;
                let result = agent
                    .prompt(PromptRequest::new("recover exact request").request_id("retry"))
                    .await?
                    .result()
                    .await;
                let failure = result.unwrap_err();
                assert!(
                    failure
                        .to_string()
                        .contains("one deterministic rejected request"),
                    "{failure}"
                );
                agent.shutdown().await?;
                drop((agent, events));
            }
        }
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            changed_calls.load(Ordering::SeqCst),
            0,
            "reopened retry cannot reevaluate a frozen route"
        );
        let log = requests.lock().unwrap();
        assert_eq!(
            log.len(),
            1,
            "a settled rejected request replays without another HTTP effect"
        );
        assert_eq!(log[0]["model"], original.as_str());
        assert!(log[0].to_string().contains("Frozen named instruction"));
        assert!(!log[0].to_string().contains("Changed host instruction"));
        eprintln!(
            "native-retry evidence: provider={} HTTP={} first_router_calls={} changed_router_calls={} settled_failure_replay=no_HTTP frozen_instruction=true",
            original.as_str(),
            log.len(),
            first_calls.load(Ordering::SeqCst),
            changed_calls.load(Ordering::SeqCst)
        );
        server.abort();
    }
    Ok(())
}
