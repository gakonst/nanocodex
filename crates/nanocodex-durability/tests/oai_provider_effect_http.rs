//! Provider-executed native tool requests cross real Responses HTTP and SQLite.
#![cfg(feature = "sqlite")]
use axum::{Json, Router, routing::post};
use nanocodex_agent::{Nanocodex, OpenAi, PromptRequest, transport::ResponsesTransport};
use nanocodex_durability::{
    DurableAgentExt, DurableSession, OwnedState, OwnerId, OwnerToken, SqliteStore, StateStore,
    StoreError, StoreFuture, StoreRecord,
};
use nanocodex_oai_tools::{
    Tool, ToolContext, ToolDefinition, ToolExposure, ToolInput, ToolResult, Tools,
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct NativeSearch;
#[async_trait::async_trait]
impl Tool for NativeSearch {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::tool_search(
            "server",
            "Provider-executed synthetic capability",
            json!({"type":"object"}),
        )
    }
    async fn execute(&self, _: ToolInput, _: ToolContext<'_>) -> ToolResult {
        panic!("provider-owned capability must never dispatch locally")
    }
}
struct RecoveryStore {
    inner: SqliteStore,
    armed: Arc<AtomicBool>,
    after_commit: bool,
    format4: bool,
}
impl StateStore for RecoveryStore {
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
        Box::pin(async move {
            let mut state = self.inner.acquire(id, owner).await?;
            if self.format4 {
                let mut head: Value =
                    serde_json::from_str(state.state.payload.as_ref().expect("persisted head"))
                        .unwrap();
                head["nanocodex_durable_state"]["format"] = json!(4);
                let operations = head["nanocodex_durable_state"]["operations"]
                    .as_object_mut()
                    .unwrap();
                // Operation IDs and model-call indices are host-owned. Select the
                // sole pending model effect by its semantics, not a guessed key.
                let mut migrated = 0;
                for operation in operations.values_mut() {
                    for model in operation["steps"].as_object_mut().unwrap().values_mut() {
                        if model["kind"] == "model_call" && model["status"] == "effect_pending" {
                            model.as_object_mut().unwrap().remove("replay_safety");
                            migrated += 1;
                        }
                    }
                }
                assert_eq!(
                    migrated, 1,
                    "fixture must migrate the pending provider effect"
                );
                state.state.payload = Some(head.to_string());
            }
            Ok(state)
        })
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
            if self.armed.swap(false, Ordering::SeqCst) {
                if self.after_commit {
                    self.inner
                        .replace(id, owner, revision, payload, records)
                        .await?;
                    return Err(StoreError::Backend("synthetic lost acknowledgement".into()));
                }
                return Err(StoreError::NotCommitted(
                    "synthetic precommit interruption".into(),
                ));
            }
            self.inner
                .replace(id, owner, revision, payload, records)
                .await
        })
    }
}
fn completed() -> String {
    let created = json!({"type":"response.created","response":{"id":"resp-native-effect"}});
    let item = json!({"type":"response.output_item.done","output_index":0,"item":{"id":"msg-native-effect","type":"message","role":"assistant","content":[{"type":"output_text","text":"native effect receipt"}]}});
    let done = json!({"type":"response.completed","response":{"id":"resp-native-effect","status":"completed","end_turn":true,"output":[],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}});
    format!("data: {created}\n\ndata: {item}\n\ndata: {done}\n\ndata: [DONE]\n\n")
}
#[tokio::test]
async fn provider_tool_http_effect_is_never_redispatched_after_uncertain_settlement()
-> eyre::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for (after_commit, format4) in [(false, false), (true, false), (false, true)] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite");
        let armed = Arc::new(AtomicBool::new(false));
        let effects = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let router = Router::new().route(
            "/responses",
            post({
                let (arm, effects, requests) = (armed.clone(), effects.clone(), requests.clone());
                move |Json(body): Json<Value>| {
                    let (arm, effects, requests) = (arm.clone(), effects.clone(), requests.clone());
                    async move {
                        assert!(
                            body["input"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|item| item["type"] == "additional_tools"
                                    && item["tools"]
                                        .as_array()
                                        .unwrap()
                                        .iter()
                                        .any(|tool| tool["type"] == "tool_search"
                                            && tool["execution"] == "server")),
                            "{body}"
                        );
                        requests.lock().unwrap().push(body);
                        effects.fetch_add(1, Ordering::SeqCst);
                        arm.store(true, Ordering::SeqCst);
                        ([("content-type", "text/event-stream")], completed())
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let backend = || {
            OpenAi::builder("synthetic-key")
                .transport(ResponsesTransport::Https)
                .store(true)
                .api_base_url(&url)
                .build()
        };
        let tools = || {
            Tools::builder()
                .tool_with_exposure(NativeSearch, ToolExposure::DirectOnly)
                .build()
        };
        let request =
            || PromptRequest::new("perform provider effect once").request_id("native-effect");
        let state = DurableSession::open(
            RecoveryStore {
                inner: SqliteStore::open(&path)?,
                armed: armed.clone(),
                after_commit,
                format4: false,
            },
            "oai-native-effect",
        )
        .await?;
        let (agent, events) = Nanocodex::builder(backend()?)
            .tools(tools()?)
            .workspace(dir.path())
            .codex_home(dir.path())
            .durability(state)
            .await?
            .build()?;
        let error = agent.prompt(request()).await?.result().await.unwrap_err();
        assert!(error.execution_policy_disposition().is_some(), "{error}");
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        let _ = agent.shutdown().await;
        drop((agent, events));
        for attempt in 0..2 {
            let state = DurableSession::open(
                RecoveryStore {
                    inner: SqliteStore::open(&path)?,
                    armed: Arc::new(AtomicBool::new(false)),
                    after_commit: false,
                    format4: format4 && attempt == 0,
                },
                "oai-native-effect",
            )
            .await?;
            if format4 && attempt == 0 {
                let saved = state.state().await?;
                let pending_models: Vec<_> = saved
                    .pending_operations()
                    .into_iter()
                    .flat_map(|(_, operation)| operation.steps.values())
                    .filter(|step| {
                        step.kind == "model_call"
                            && matches!(
                                step.status,
                                nanocodex_durability::StepStatus::EffectPending
                            )
                    })
                    .collect();
                assert_eq!(pending_models.len(), 1);
                assert_eq!(
                    pending_models[0].replay_safety,
                    nanocodex_durability::ReplaySafety::Safe,
                    "migration must actually produce saved Safe before current Unsafe constrains it"
                );
            }
            let (agent, events) = Nanocodex::builder(backend()?)
                .tools(tools()?)
                .workspace(dir.path())
                .codex_home(dir.path())
                .durability(state)
                .await?
                .build()?;
            let result = agent.prompt(request()).await?.result().await;
            if after_commit {
                assert_eq!(result?.final_message(), "native effect receipt");
            } else {
                let error = result.unwrap_err();
                assert!(error.to_string().contains("outcome is unknown"), "{error}");
            }
            assert_eq!(
                requests.lock().unwrap().len(),
                1,
                "HTTP must not redispatch"
            );
            assert_eq!(
                effects.load(Ordering::SeqCst),
                1,
                "provider effect must execute exactly once"
            );
            let _ = agent.shutdown().await;
            drop((agent, events));
        }
        println!(
            "OAI HTTP recovery after_commit={after_commit} format4={format4}: requests=1 effects=1"
        );
        server.abort();
    }
    Ok(())
}
