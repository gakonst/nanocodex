use std::{
    future::{Future, Ready, ready},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    task::{Context, Poll},
};

use nanocodex_oai_api::{
    responses::{ContentItem, MessageRole, ResponseItem, ResponseItemId, Usage, WarmupResponse},
    tower::{
        CompactionOutput, GenerationOutput, ResponsePipelineStats, ResponsesAttempt,
        ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse,
    },
};
use tower::Service;

use super::*;

async fn send_compaction<S>(socket: &mut WebSocketStream<S>, response_id: &str) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_json(
        socket,
        json!({
            "type": "response.output_item.done",
            "item": {
                "id": "cmp-server-id",
                "type": "compaction",
                "encrypted_content": "opaque-summary"
            }
        }),
    )
    .await?;
    send_json(socket, completed_response_with_usage(response_id, &[], 120)).await
}

#[tokio::test]
async fn manual_compaction_before_first_prompt_reinjects_cached_context_and_persists_boundary()
-> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;

        let compact = next_json(&mut socket).await?;
        assert!(compact.get("previous_response_id").is_none());
        let compact_input = compact["input"]
            .as_array()
            .ok_or_else(|| eyre!("manual compaction input was not an array"))?;
        assert_eq!(
            compact_input.last(),
            Some(&json!({"type": "compaction_trigger"}))
        );
        assert!(!compact.to_string().contains("<environment_context>"));
        assert!(!compact.to_string().contains("first real prompt"));
        send_compaction(&mut socket, "resp-compact").await?;

        let generation = next_json(&mut socket).await?;
        assert!(generation.get("previous_response_id").is_none());
        let input = generation["input"]
            .as_array()
            .ok_or_else(|| eyre!("post-compaction input was not an array"))?;
        assert_eq!(input[0]["type"], "additional_tools");
        let compact_index = input
            .iter()
            .position(|item| item["type"] == "compaction")
            .ok_or_else(|| eyre!("installed compaction was not replayed"))?;
        assert_eq!(input[compact_index + 1]["role"], "developer");
        assert!(
            input[compact_index + 2]
                .to_string()
                .contains("creation-time agents")
        );
        assert!(
            !input[compact_index + 2]
                .to_string()
                .contains("mutated after compact")
        );
        assert!(
            input[compact_index + 2]
                .to_string()
                .contains("<environment_context>")
        );
        assert_eq!(
            input
                .last()
                .and_then(|item| item["content"][0]["text"].as_str()),
            Some("first real prompt")
        );
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("manual-empty-compaction")?;
    std::fs::write(workspace.join("AGENTS.md"), "creation-time agents\n")?;
    let rollout_home = temporary_workspace("manual-empty-compaction-rollout")?;
    let openai = OpenAi::builder("test-key")
        .experimental_context(false)
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .rollout(RolloutConfig::new(&rollout_home))
        .build()?;

    agent.compact().await?;
    let (fork, fork_events) = agent.fork().await?;
    fork.shutdown().await?;
    drop((fork, fork_events));
    agent.flush_rollout().await?;
    let rollout_path = agent
        .rollout()
        .ok_or_else(|| eyre!("manual compaction rollout was not configured"))?
        .path();
    let lines = std::fs::read_to_string(rollout_path)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert_eq!(
        lines.iter().map(|line| &line["type"]).collect::<Vec<_>>(),
        [
            "session_meta",
            "event_msg",
            "compacted",
            "turn_context",
            "world_state",
            "event_msg"
        ]
    );
    assert_eq!(lines[1]["payload"]["type"], "task_started");
    assert_eq!(lines[5]["payload"]["type"], "task_complete");
    let compact_turn_id = lines[1]["payload"]["turn_id"]
        .as_str()
        .ok_or_else(|| eyre!("compact task_started omitted its turn ID"))?;
    assert_eq!(
        lines[5]["payload"]["turn_id"].as_str(),
        Some(compact_turn_id)
    );
    assert_ne!(compact_turn_id, TEST_SESSION_ID);
    assert_eq!(uuid::Uuid::parse_str(compact_turn_id)?.get_version_num(), 7);
    assert!(
        !lines.iter().any(|line| {
            line["type"] == "event_msg" && line["payload"]["type"] == "user_message"
        })
    );
    assert!(
        !lines.iter().any(|line| {
            line["type"] == "event_msg" && line["payload"]["type"] == "agent_message"
        })
    );

    std::fs::write(workspace.join("AGENTS.md"), "mutated after compact\n")?;
    assert_eq!(
        agent
            .prompt("first real prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    agent.shutdown().await?;
    drop((agent, events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    std::fs::remove_dir_all(rollout_home)?;
    Ok(())
}

#[tokio::test]
async fn manual_compaction_after_a_turn_uses_the_live_session_and_reinjects_next_context()
-> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let first = next_json(&mut socket).await?;
        assert_eq!(first["previous_response_id"], "resp-warmup");
        send_final(&mut socket, "resp-first").await?;

        let compact = next_json(&mut socket).await?;
        assert_eq!(compact["previous_response_id"], "resp-first");
        assert_eq!(compact["input"], json!([{"type": "compaction_trigger"}]));
        send_compaction(&mut socket, "resp-compact").await?;

        let second = next_json(&mut socket).await?;
        assert!(second.get("previous_response_id").is_none());
        let input = second["input"]
            .as_array()
            .ok_or_else(|| eyre!("post-compaction input was not an array"))?;
        let compact_index = input
            .iter()
            .position(|item| item["type"] == "compaction")
            .ok_or_else(|| eyre!("installed compaction was not replayed"))?;
        assert!(
            input[..compact_index]
                .iter()
                .any(|item| item.to_string().contains("first prompt"))
        );
        assert_eq!(input[compact_index + 1]["role"], "developer");
        assert!(
            input[compact_index + 2]
                .to_string()
                .contains("<environment_context>")
        );
        assert_eq!(
            input
                .last()
                .and_then(|item| item["content"][0]["text"].as_str()),
            Some("second prompt")
        );
        send_final(&mut socket, "resp-second").await
    });

    let workspace = temporary_workspace("manual-post-turn-compaction")?;
    let openai = OpenAi::builder("test-key")
        .experimental_context(false)
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    assert_eq!(
        agent
            .prompt("first prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    agent.compact().await?;
    assert_eq!(
        agent
            .prompt("second prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    agent.shutdown().await?;
    drop((agent, events));

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn manual_compaction_uses_current_defaults_and_a_fresh_logical_turn() -> Result<()> {
    const TURN_STATE: &str = "x-codex-turn-state";

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let warmup = next_json(&mut socket).await?;
        assert_warmup(&warmup);
        send_json(
            &mut socket,
            json!({
                "type": "response.metadata",
                "headers": { TURN_STATE: "first-turn-state" }
            }),
        )
        .await?;
        send_warmup(&mut socket, "resp-warmup").await?;

        let first = next_json(&mut socket).await?;
        assert_eq!(
            first["client_metadata"][TURN_STATE], "first-turn-state",
            "warmup and generation belong to one logical turn"
        );
        assert_eq!(first["reasoning"]["effort"], "low");
        assert_eq!(first["service_tier"], "default");
        send_final(&mut socket, "resp-first").await?;

        let compact = next_json(&mut socket).await?;
        assert!(
            compact["client_metadata"].get(TURN_STATE).is_none(),
            "standalone compaction must clear the preceding turn's sticky state"
        );
        assert_eq!(compact["reasoning"]["effort"], "high");
        assert_eq!(compact["service_tier"], "priority");
        send_json(
            &mut socket,
            json!({
                "type": "response.metadata",
                "headers": { TURN_STATE: "compact-turn-state" }
            }),
        )
        .await?;
        send_compaction(&mut socket, "resp-compact").await?;

        let second = next_json(&mut socket).await?;
        assert!(
            second["client_metadata"].get(TURN_STATE).is_none(),
            "the next prompt must not inherit standalone compaction state"
        );
        assert_eq!(second["reasoning"]["effort"], "high");
        assert_eq!(second["service_tier"], "priority");
        send_final(&mut socket, "resp-second").await
    });

    let workspace = temporary_workspace("manual-current-defaults")?;
    let openai = OpenAi::builder("test-key")
        .experimental_context(false)
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    assert_eq!(
        agent
            .prompt("first prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    agent.set_thinking(Thinking::High).await?;
    agent.set_fast_mode(true).await?;
    agent.compact().await?;
    assert_eq!(
        agent
            .prompt("second prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    agent.shutdown().await?;
    drop((agent, events));

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock logical-turn server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[derive(Clone)]
struct ReplaceableCompactionService {
    calls: Arc<AtomicU32>,
    first_started: Arc<tokio::sync::Notify>,
}

impl Service<ResponsesAttempt> for ReplaceableCompactionService {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future =
        Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _context: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        assert!(matches!(request.kind(), ResponsesAttemptKind::Compaction));
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        let first_started = Arc::clone(&self.first_started);
        Box::pin(async move {
            if call == 0 {
                first_started.notify_one();
                std::future::pending::<()>().await;
                unreachable!("the superseded compaction future must be dropped");
            }
            if call == 2 {
                return Err(ResponseError::service(std::io::Error::other(
                    "injected standalone compaction failure",
                )));
            }
            assert_eq!(call, 1, "unexpected standalone compaction attempt");
            Ok(ResponsesServiceResponse::new(ResponsesOutput::Compaction(
                CompactionOutput {
                    id: "resp-compact".to_owned(),
                    status: "completed".to_owned(),
                    item: ResponseItem::Compaction {
                        id: Some(ResponseItemId::from("cmp-replacement")),
                        encrypted_content: "opaque-summary".into(),
                        created_by: None,
                        internal_chat_message_metadata_passthrough: None,
                    },
                    usage: None,
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: None,
                    pipeline_stats: ResponsePipelineStats::default(),
                },
            )))
        })
    }
}

#[tokio::test]
async fn a_later_manual_compaction_replaces_a_stuck_manual_compaction() -> Result<()> {
    let workspace = temporary_workspace("replace-manual-compaction")?;
    let rollout_home = temporary_workspace("replace-manual-compaction-rollout")?;
    let calls = Arc::new(AtomicU32::new(0));
    let first_started = Arc::new(tokio::sync::Notify::new());
    let factory_calls = Arc::clone(&calls);
    let factory_started = Arc::clone(&first_started);
    let openai = OpenAi::builder("test-key")
        .experimental_context(false)
        .service(move || ReplaceableCompactionService {
            calls: Arc::clone(&factory_calls),
            first_started: Arc::clone(&factory_started),
        })
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .session_id(test_session_id())
        .rollout(RolloutConfig::new(&rollout_home))
        .build()?;

    let first_agent = agent.clone();
    let first = tokio::spawn(async move { first_agent.compact().await });
    timeout(std::time::Duration::from_secs(5), first_started.notified())
        .await
        .map_err(|_| eyre!("first manual compaction did not start"))?;
    let second_agent = agent.clone();
    let second = tokio::spawn(async move { second_agent.compact().await });

    assert!(matches!(first.await?, Err(NanocodexError::TurnCancelled)));
    second.await??;
    assert!(matches!(
        agent.compact().await,
        Err(NanocodexError::Response(error))
            if error.to_string() == "injected standalone compaction failure"
    ));
    assert_eq!(calls.load(Ordering::Relaxed), 3);

    agent.flush_rollout().await?;
    let rollout_path = agent
        .rollout()
        .ok_or_else(|| eyre!("replacement rollout was not configured"))?
        .path();
    let lines = std::fs::read_to_string(rollout_path)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    let lifecycle = lines
        .iter()
        .filter(|line| line["type"] == "event_msg")
        .map(|line| &line["payload"])
        .collect::<Vec<_>>();
    assert_eq!(
        lifecycle
            .iter()
            .map(|event| event["type"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        [
            "task_started",
            "turn_aborted",
            "task_started",
            "task_complete",
            "task_started",
            "task_complete"
        ]
    );
    assert_eq!(lifecycle[1]["reason"], "replaced");
    assert_eq!(lifecycle[0]["turn_id"], lifecycle[1]["turn_id"]);
    assert_eq!(lifecycle[2]["turn_id"], lifecycle[3]["turn_id"]);
    assert_eq!(lifecycle[4]["turn_id"], lifecycle[5]["turn_id"]);
    assert_ne!(lifecycle[0]["turn_id"], lifecycle[2]["turn_id"]);
    assert_ne!(lifecycle[2]["turn_id"], lifecycle[4]["turn_id"]);
    assert!(lifecycle[5]["last_agent_message"].is_null());

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    std::fs::remove_dir_all(rollout_home)?;
    Ok(())
}

#[derive(Clone)]
struct ActiveReplacementService {
    calls: Arc<AtomicU32>,
    active_started: Arc<tokio::sync::Notify>,
    order: Arc<Mutex<Vec<&'static str>>>,
}

impl Service<ResponsesAttempt> for ActiveReplacementService {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future =
        Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _context: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        let active_started = Arc::clone(&self.active_started);
        let order = Arc::clone(&self.order);
        Box::pin(async move {
            let output = match (call, request.kind()) {
                (0, ResponsesAttemptKind::Warmup) => {
                    order.lock().unwrap().push("warmup");
                    ResponsesOutput::Warmup(WarmupResponse {
                        id: "resp-warmup".to_owned(),
                        usage: None,
                    })
                }
                (1, ResponsesAttemptKind::Generation) => {
                    order.lock().unwrap().push("active_generation");
                    active_started.notify_one();
                    std::future::pending::<()>().await;
                    unreachable!("cancelled active generation must be dropped")
                }
                (2, ResponsesAttemptKind::Compaction) => {
                    order.lock().unwrap().push("compaction");
                    ResponsesOutput::Compaction(CompactionOutput {
                        id: "resp-compact".to_owned(),
                        status: "completed".to_owned(),
                        item: ResponseItem::Compaction {
                            id: Some(ResponseItemId::from("cmp-active-replacement")),
                            encrypted_content: "opaque-summary".into(),
                            created_by: None,
                            internal_chat_message_metadata_passthrough: None,
                        },
                        usage: None,
                        time_to_first_event_ns: 0,
                        time_to_first_output_ns: None,
                        pipeline_stats: ResponsePipelineStats::default(),
                    })
                }
                (3, ResponsesAttemptKind::Generation) => {
                    order.lock().unwrap().push("queued_generation");
                    ResponsesOutput::Generation(GenerationOutput {
                        id: "resp-queued".to_owned(),
                        status: "completed".to_owned(),
                        end_turn: Some(true),
                        final_message: Some("done".to_owned()),
                        output_items: vec![ResponseItem::message(
                            MessageRole::Assistant,
                            [ContentItem::output_text("done")],
                        )],
                        code_calls: Vec::new(),
                        usage: None,
                        time_to_first_event_ns: 0,
                        time_to_first_output_ns: None,
                        pipeline_stats: ResponsePipelineStats::default(),
                    })
                }
                _ => panic!("unexpected attempt {call}: {:?}", request.kind()),
            };
            Ok(ResponsesServiceResponse::new(output))
        })
    }
}

#[tokio::test]
async fn manual_compaction_replaces_an_active_turn_before_queued_prompts() -> Result<()> {
    let workspace = temporary_workspace("manual-active-compaction")?;
    let calls = Arc::new(AtomicU32::new(0));
    let active_started = Arc::new(tokio::sync::Notify::new());
    let order = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_started = Arc::clone(&active_started);
    let factory_order = Arc::clone(&order);
    let openai = OpenAi::builder("test-key")
        .experimental_context(false)
        .service(move || ActiveReplacementService {
            calls: Arc::clone(&factory_calls),
            active_started: Arc::clone(&factory_started),
            order: Arc::clone(&factory_order),
        })
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;

    let active = agent.prompt("active prompt").await?;
    timeout(std::time::Duration::from_secs(5), active_started.notified())
        .await
        .map_err(|_| eyre!("active model request did not start"))?;
    let queued = agent.prompt("queued prompt").await?;
    let compact_agent = agent.clone();
    let compact = tokio::spawn(async move { compact_agent.compact().await });

    assert!(matches!(
        active.result().await,
        Err(NanocodexError::TurnCancelled)
    ));
    compact.await??;
    assert_eq!(queued.result().await?.final_message(), "done");
    assert_eq!(
        order.lock().unwrap().as_slice(),
        [
            "warmup",
            "active_generation",
            "compaction",
            "queued_generation"
        ]
    );

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[derive(Default)]
struct CompactionObservations {
    warmup_prefix: Vec<Vec<u8>>,
    generations: Vec<ObservedAttempt>,
    compactions: Vec<ObservedAttempt>,
}

struct ObservedAttempt {
    previous_response_id: Option<String>,
    full_replay: bool,
    input: Vec<Value>,
    input_bytes: Vec<Vec<u8>>,
}

impl ObservedAttempt {
    fn capture(request: &ResponsesAttempt) -> Self {
        Self {
            previous_response_id: request.previous_response_id().map(str::to_owned),
            full_replay: request.is_full_replay(),
            input: request
                .input_items()
                .map(|item| serde_json::to_value(item).expect("response items serialize"))
                .collect(),
            input_bytes: request
                .input_items()
                .map(|item| serde_json::to_vec(item).expect("response items serialize"))
                .collect(),
        }
    }
}

#[derive(Clone)]
struct PreTurnCompactionService {
    observations: Arc<Mutex<CompactionObservations>>,
    generation_calls: Arc<AtomicU32>,
    workspace: PathBuf,
}

impl Service<ResponsesAttempt> for PreTurnCompactionService {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Ready<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        let output = match request.kind() {
            ResponsesAttemptKind::Warmup => {
                self.observations.lock().unwrap().warmup_prefix = request
                    .input_items()
                    .map(|item| serde_json::to_vec(item).expect("response items serialize"))
                    .collect();
                ResponsesOutput::Warmup(WarmupResponse {
                    id: "resp-warmup".to_owned(),
                    usage: None,
                })
            }
            ResponsesAttemptKind::Generation => {
                let call = self.generation_calls.fetch_add(1, Ordering::Relaxed) + 1;
                self.observations
                    .lock()
                    .unwrap()
                    .generations
                    .push(ObservedAttempt::capture(&request));
                let answer = format!("answer-{call}");
                ResponsesOutput::Generation(GenerationOutput {
                    id: format!("resp-{call}"),
                    status: "completed".to_owned(),
                    end_turn: Some(true),
                    final_message: Some(answer.clone()),
                    output_items: vec![ResponseItem::message(
                        MessageRole::Assistant,
                        [ContentItem::output_text(answer)],
                    )],
                    code_calls: Vec::new(),
                    usage: Some(Usage {
                        total_tokens: if call == 1 { 244_800 } else { 120 },
                        ..Usage::default()
                    }),
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: None,
                    pipeline_stats: ResponsePipelineStats::default(),
                })
            }
            ResponsesAttemptKind::Compaction => {
                self.observations
                    .lock()
                    .unwrap()
                    .compactions
                    .push(ObservedAttempt::capture(&request));
                std::fs::write(
                    self.workspace.join("AGENTS.md"),
                    "fresh instructions loaded after compaction\n",
                )
                .expect("replace project instructions while compaction is in flight");
                ResponsesOutput::Compaction(CompactionOutput {
                    id: "resp-compact".to_owned(),
                    status: "completed".to_owned(),
                    item: ResponseItem::Compaction {
                        id: Some(ResponseItemId::from("cmp-server")),
                        encrypted_content: "opaque-summary".into(),
                        created_by: None,
                        internal_chat_message_metadata_passthrough: None,
                    },
                    usage: Some(Usage {
                        total_tokens: 120,
                        ..Usage::default()
                    }),
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: None,
                    pipeline_stats: ResponsePipelineStats::default(),
                })
            }
            _ => panic!("the pre-turn compaction regression uses only supported attempt kinds"),
        };
        ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

#[tokio::test]
async fn pre_turn_compaction_keeps_creation_time_agents_md() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    std::fs::write(
        workspace.path().join("AGENTS.md"),
        "instructions captured before compaction\n",
    )?;
    let observations = Arc::new(Mutex::new(CompactionObservations::default()));
    let generation_calls = Arc::new(AtomicU32::new(0));
    let factory_observations = Arc::clone(&observations);
    let factory_generation_calls = Arc::clone(&generation_calls);
    let service_workspace = workspace.path().to_path_buf();
    let openai = OpenAi::builder("test-key")
        .experimental_context(false)
        .service(move || PreTurnCompactionService {
            observations: Arc::clone(&factory_observations),
            generation_calls: Arc::clone(&factory_generation_calls),
            workspace: service_workspace.clone(),
        })
        .build()?;
    let tools = Tools::builder().without_defaults().build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .instructions("Keep the request prefix stable.")
        .workspace(workspace.path())
        .tools(tools)
        .build()?;
    drop(events);

    assert_eq!(
        agent.prompt("first prompt").await?.await?.final_message(),
        "answer-1"
    );
    let second = agent.prompt("second prompt").await?.await?;
    assert_eq!(second.final_message(), "answer-2");

    let observations = observations.lock().unwrap();
    assert_eq!(observations.compactions.len(), 1);
    let compact = &observations.compactions[0];
    assert!(!compact.full_replay);
    assert_eq!(compact.previous_response_id.as_deref(), Some("resp-1"));
    assert_eq!(compact.input, [json!({ "type": "compaction_trigger" })]);

    assert_eq!(observations.generations.len(), 2);
    let first = &observations.generations[0];
    let follow_on = &observations.generations[1];
    assert!(follow_on.full_replay);
    assert_eq!(follow_on.previous_response_id, None);
    assert_eq!(
        follow_on.input_bytes[..observations.warmup_prefix.len()],
        observations.warmup_prefix,
        "pre-turn compaction must preserve the byte-stable request prefix"
    );
    assert!(!first.full_replay);
    assert_eq!(first.previous_response_id.as_deref(), Some("resp-warmup"));
    assert_eq!(follow_on.input.len(), 7);
    assert_eq!(follow_on.input[0]["type"], "additional_tools");
    assert_eq!(follow_on.input[1]["role"], "developer");
    assert_eq!(follow_on.input[2]["content"][0]["text"], "first prompt");
    assert_eq!(follow_on.input[3]["type"], "compaction");
    assert_eq!(follow_on.input[3]["encrypted_content"], "opaque-summary");
    assert_eq!(follow_on.input[4]["role"], "developer");
    assert!(
        follow_on.input[5]
            .to_string()
            .contains("instructions captured before compaction")
    );
    assert!(
        !follow_on.input[5]
            .to_string()
            .contains("fresh instructions loaded after compaction")
    );
    assert!(
        follow_on.input[5]
            .to_string()
            .contains("<environment_context>")
    );
    assert_eq!(follow_on.input[6]["content"][0]["text"], "second prompt");

    let snapshot = serde_json::to_value(
        second
            .snapshot()
            .expect("local turns always retain a snapshot"),
    )?;
    let history = snapshot["history"]
        .as_array()
        .expect("snapshot history is an array");
    let compact_index = history
        .iter()
        .position(|item| item["type"] == "compaction")
        .expect("snapshot retains the installed compaction");
    assert_eq!(history[compact_index + 1]["role"], "developer");
    assert!(
        history[compact_index + 2]
            .to_string()
            .contains("instructions captured before compaction")
    );
    assert_eq!(
        history[compact_index + 3]["content"][0]["text"],
        "second prompt"
    );
    drop((observations, agent));
    Ok(())
}
