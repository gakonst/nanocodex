use super::*;

#[derive(Clone)]
struct FailFirstWarmup {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl tower::Service<nanocodex_oai_api::tower::ResponsesAttempt> for FailFirstWarmup {
    type Response = nanocodex_oai_api::tower::ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = std::future::Ready<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: nanocodex_oai_api::tower::ResponsesAttempt) -> Self::Future {
        use std::sync::atomic::Ordering;

        if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
            return std::future::ready(Err(ResponseError::service(std::io::Error::other(
                "fatal warmup boundary",
            ))));
        }
        assert!(matches!(
            request.kind(),
            nanocodex_oai_api::tower::ResponsesAttemptKind::Generation
        ));
        let replay = request
            .input_items()
            .map(|item| serde_json::to_string(item).expect("request item serializes"))
            .collect::<String>();
        assert!(replay.contains("accepted before warmup failure"));
        assert!(replay.contains("prompt after warmup failure"));
        std::future::ready(Ok(nanocodex_oai_api::tower::ResponsesServiceResponse::new(
            nanocodex_oai_api::tower::ResponsesOutput::Generation(
                nanocodex_oai_api::tower::GenerationOutput {
                    id: "resp-recovered".to_owned(),
                    status: "completed".to_owned(),
                    end_turn: Some(true),
                    final_message: Some("done".to_owned()),
                    output_items: vec![nanocodex_oai_api::responses::ResponseItem::message(
                        nanocodex_oai_api::responses::MessageRole::Assistant,
                        [nanocodex_oai_api::responses::ContentItem::output_text(
                            "done",
                        )],
                    )],
                    code_calls: Vec::new(),
                    usage: None,
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: None,
                    pipeline_stats: nanocodex_oai_api::tower::ResponsePipelineStats::default(),
                },
            ),
        )))
    }
}

#[tokio::test]
async fn missing_stored_checkpoint_replays_local_history_once() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        assert_warmup_with_store(&next_json(&mut root).await?, true);
        send_warmup(&mut root, "resp-warmup").await?;
        let first = next_json(&mut root).await?;
        send_final(&mut root, "resp-first").await?;

        let (stream, _) = listener.accept().await?;
        let mut branch = accept_async(stream).await?;
        let checkpoint = next_json(&mut branch).await?;
        assert_eq!(checkpoint["previous_response_id"], "resp-first");
        assert_eq!(checkpoint["input"].as_array().map(Vec::len), Some(1));
        send_json(
            &mut branch,
            json!({
                "type": "error",
                "status": 400,
                "error": {
                    "type": "invalid_request_error",
                    "message": "Invalid `previous_response_id`."
                }
            }),
        )
        .await?;

        let replay = next_json(&mut branch).await?;
        assert!(replay.get("previous_response_id").is_none());
        assert_eq!(replay["store"], true);
        assert_eq!(replay["input"][0]["type"], "additional_tools");
        assert_eq!(replay["input"][1]["role"], "developer");
        let replay_text = replay.to_string();
        assert!(replay_text.contains("root prompt"));
        assert!(replay_text.contains("branch after eviction"));
        assert!(
            replay["input"]
                .as_array()
                .is_some_and(|items| items.len() > 4)
        );
        send_final(&mut branch, "resp-replayed").await?;
        drop((root, first));
        Result::<()>::Ok(())
    });

    let workspace = temporary_workspace("checkpoint-miss")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .store(true)
        .build()?;
    let (agent, root_events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let first = agent
        .prompt(Prompt::new("root prompt"))
        .await?
        .result()
        .await?;
    let (fork, mut fork_events) = agent.fork_from(&first).await?;
    let branch = fork.prompt("branch after eviction").await?;
    assert_eq!(branch.result().await?.final_message(), "done");

    drop((agent, fork, root_events));
    let mut observed_checkpoint_retry = false;
    while let Some(event) = fork_events.recv().await {
        if event.kind == AgentEventKind::ModelAttemptRetrying {
            let payload = event.decode_payload::<Value>()?;
            observed_checkpoint_retry = payload["error_class"] == "checkpoint_missing"
                && payload["replay_mode"] == "full_history"
                && payload["opens_new_socket"] == false;
        }
    }
    assert!(observed_checkpoint_retry);
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn serialized_session_and_codex_rollout_share_committed_history() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut original = accept_async(stream).await?;
        let warmup = next_json(&mut original).await?;
        assert_eq!(warmup["prompt_cache_key"], "durable-cache");
        let original_tools_id = warmup["input"][0]["id"].clone();
        let original_instructions_id = warmup["input"][1]["id"].clone();
        send_warmup(&mut original, "resp-warmup").await?;
        let first = next_json(&mut original).await?;
        assert_eq!(first["previous_response_id"], "resp-warmup");
        send_final(&mut original, "resp-first").await?;

        let (stream, _) = listener.accept().await?;
        let mut resumed = accept_async(stream).await?;
        let replay = next_json(&mut resumed).await?;
        assert!(replay.get("previous_response_id").is_none());
        assert_eq!(replay["prompt_cache_key"], "durable-cache");
        assert_eq!(replay["input"][0]["type"], "additional_tools");
        assert_eq!(
            replay["input"][0]["id"], original_tools_id,
            "unchanged tools must retain their exact ID across rollout resume"
        );
        assert_eq!(replay["input"][1]["role"], "developer");
        assert_ne!(
            replay["input"][1]["id"], original_instructions_id,
            "changed instructions must receive a payload-sensitive ID"
        );
        assert_eq!(
            replay["input"][1]["content"][0]["text"],
            "instructions from the resumed rollout"
        );
        let replay_text = replay.to_string();
        assert!(replay_text.contains("first prompt"));
        assert!(replay_text.contains("resume prompt"));
        send_final(&mut resumed, "resp-resumed").await?;
        Result::<()>::Ok(())
    });

    let workspace = temporary_workspace("serialized-resume")?;
    let rollout_home = temporary_workspace("serialized-resume-rollout")?;
    let openai = || {
        OpenAi::builder("test-key")
            .websocket_url(endpoint.clone())
            .build()
    };
    let (agent, events) = Nanocodex::builder(openai()?)
        .instructions("durable instructions")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .prompt_cache_key("durable-cache")
        .rollout(RolloutConfig::new(&rollout_home))
        .build()?;
    let rollout_path = agent
        .rollout()
        .ok_or_else(|| eyre!("rollout was not configured"))?
        .path()
        .to_path_buf();
    let first = agent.prompt("first prompt").await?.result().await?;
    let encoded = serde_json::to_vec(
        &first
            .snapshot()
            .expect("local turns always retain a snapshot"),
    )?;
    agent.flush_rollout().await?;
    let durable_config = RolloutConfig::new(&rollout_home);
    let durable = durable_config.load_session("019c0d31-c308-7d91-bff4-5dca82d15ac6")?;
    assert_eq!(durable.thread_id(), agent.session_id().to_string());
    assert_eq!(
        Path::new(durable.workspace()).canonicalize()?,
        workspace.canonicalize()?
    );
    assert_eq!(durable.rollout_path(), rollout_path.canonicalize()?);
    let snapshot_json = serde_json::from_slice::<Value>(&encoded)?;
    let request_prefix = snapshot_json["request_prefix"]
        .as_array()
        .ok_or_else(|| eyre!("snapshot request prefix was not an array"))?;
    assert_eq!(snapshot_json["context_snapshot"]["kind"], "known");
    assert_eq!(
        snapshot_json["context_snapshot"]["snapshot"]["environment"]["cwd"],
        workspace.canonicalize()?.to_string_lossy().as_ref()
    );
    assert_eq!(request_prefix[0]["type"], "additional_tools");
    assert!(
        request_prefix[0]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("at_"))
    );
    assert!(
        request_prefix[1]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("msg_"))
    );
    assert!(
        snapshot_json["history"]
            .as_array()
            .is_some_and(|items| items.iter().all(|item| {
                item.get("id").is_some_and(Value::is_string) || item["type"] == "compaction_trigger"
            }))
    );
    let rollout_lines = std::fs::read_to_string(&rollout_path)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert_eq!(
        rollout_lines[0]["payload"]["prompt_cache_key"],
        "durable-cache"
    );
    let persisted_context = rollout_lines
        .iter()
        .find(|line| line["type"] == "world_state")
        .ok_or_else(|| eyre!("rollout omitted its typed context baseline"))?;
    assert_eq!(
        persisted_context["payload"]["state"]["nanocodex_context"],
        snapshot_json["context_snapshot"]
    );
    let rollout_history = rollout_lines
        .into_iter()
        .filter(|line| line["type"] == "response_item")
        .map(|line| line["payload"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        serde_json::to_value(durable.snapshot())?["history"].as_array(),
        Some(&rollout_history),
        "rollout resume must materialize the recorded committed history"
    );
    let snapshot: SessionSnapshot = serde_json::from_slice(&encoded)?;
    agent.shutdown().await?;
    drop((agent, events, first));

    let mut unsupported: Value = serde_json::from_slice(&encoded)?;
    unsupported["version"] = json!(2);
    let unsupported: SessionSnapshot = serde_json::from_value(unsupported)?;
    let unsupported = Nanocodex::builder(openai()?).resume(unsupported).build();
    assert!(matches!(
        unsupported,
        Err(NanocodexError::InvalidSessionSnapshot(message))
            if message.contains("unsupported format version")
    ));

    let mut astra_snapshot: Value = serde_json::from_slice(&encoded)?;
    astra_snapshot["model"] = json!("gpt-6-astra");
    let astra_snapshot: SessionSnapshot = serde_json::from_value(astra_snapshot)?;
    let incompatible = Nanocodex::builder(openai()?)
        .thinking(Thinking::None)
        .resume(astra_snapshot)
        .build();
    assert!(matches!(
        incompatible,
        Err(NanocodexError::InvalidRequest(message))
            if message.contains("GPT-6 Astra requires")
    ));

    let (compatible, compatible_events) = Nanocodex::builder(openai()?)
        .model(Model::Astra)
        .thinking(Thinking::None)
        .resume(snapshot.clone())
        .build()?;
    compatible.shutdown().await?;
    drop((compatible, compatible_events));

    let other_workspace = temporary_workspace("serialized-resume-other")?;
    let incompatible = Nanocodex::builder(openai()?)
        .instructions("durable instructions")
        .thinking(Thinking::Low)
        .workspace(&other_workspace)
        .resume(snapshot.clone())
        .build();
    assert!(matches!(
        incompatible,
        Err(NanocodexError::WorkspaceChanged { .. })
    ));
    std::fs::remove_dir_all(other_workspace)?;
    let incompatible = Nanocodex::builder(openai()?)
        .instructions("durable instructions")
        .thinking(Thinking::Low)
        .prompt_cache_key("changed-cache")
        .resume(snapshot.clone())
        .build();
    assert!(matches!(
        incompatible,
        Err(NanocodexError::InvalidSessionSnapshot(message))
            if message.contains("prompt cache key")
    ));

    let (thread_id, snapshot, rollout) = durable.into_parts();
    let (resumed, resumed_events) = Nanocodex::builder(openai()?)
        .instructions("instructions from the resumed rollout")
        .thinking(Thinking::Low)
        .session_id(thread_id.parse()?)
        .resume(snapshot)
        .rollout(rollout)
        .build()?;
    assert_eq!(
        resumed_events.request_id(),
        "019c0d31-c308-7d91-bff4-5dca82d15ac6"
    );
    assert_eq!(
        resumed
            .prompt("resume prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    resumed.flush_rollout().await?;
    assert_eq!(
        resumed
            .rollout()
            .map(|rollout| rollout.path().canonicalize())
            .transpose()?,
        Some(rollout_path.canonicalize()?)
    );
    let durable = durable_config.load_session("019c0d31-c308-7d91-bff4-5dca82d15ac6")?;
    let durable_json = serde_json::to_value(durable.snapshot())?;
    assert!(
        durable_json["history"]
            .to_string()
            .contains("resume prompt")
    );
    let session_meta_count = std::fs::read_to_string(&rollout_path)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<serde_json::Result<Vec<_>>>()?
        .into_iter()
        .filter(|line| line["type"] == "session_meta")
        .count();
    assert_eq!(session_meta_count, 1);

    resumed.shutdown().await?;
    drop((resumed, resumed_events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    std::fs::remove_dir_all(rollout_home)?;
    Ok(())
}

#[tokio::test]
async fn serialized_session_rebinds_deployed_instructions_and_tools() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let first = next_http_json(&listener).await?;
        assert_eq!(first.body["model"], "gpt-5.6-luna");
        assert_eq!(first.body["store"], false);
        assert!(first.body.get("previous_response_id").is_none());
        assert!(first.body.to_string().contains("first prompt"));
        send_http_final(first.stream, "resp-first").await?;

        let resumed = next_http_json(&listener).await?;
        assert_eq!(resumed.body["model"], "gpt-5.6-luna");
        assert_eq!(resumed.body["store"], false);
        assert!(resumed.body.get("previous_response_id").is_none());
        let replay = resumed.body.to_string();
        assert_eq!(
            resumed.body["input"][1]["content"][0]["text"],
            "instructions from the new deployment"
        );
        assert_eq!(
            resumed.body["input"][0]["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|tool| tool["name"].as_str())
                .collect::<Vec<_>>(),
            ["exec", "wait"]
        );
        assert!(!replay.contains("instructions from the old deployment"));
        assert!(replay.contains("first prompt"));
        assert!(replay.contains("done"));
        assert!(replay.contains("resume prompt"));
        send_http_final(resumed.stream, "resp-resumed").await
    });

    let workspace = temporary_workspace("serialized-resume-https")?;
    let openai = OpenAi::builder("test-key")
        .transport(ResponsesTransport::Https)
        .store(false)
        .api_base_url(endpoint.clone())
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .instructions("instructions from the old deployment")
        .model(Model::Luna)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .prompt_cache_key("durable-cache")
        .build()?;
    let first = agent.prompt("first prompt").await?.result().await?;
    let snapshot_json = serde_json::to_value(
        first
            .snapshot()
            .expect("local turns always retain a snapshot"),
    )?;
    assert_eq!(snapshot_json["model"], "gpt-5.6-luna");
    let snapshot = serde_json::from_value(snapshot_json)?;
    drop((agent, events, first));

    let openai = OpenAi::builder("test-key")
        .transport(ResponsesTransport::Https)
        .store(false)
        .api_base_url(endpoint)
        .build()?;
    let (resumed, resumed_events) = Nanocodex::builder(openai)
        .instructions("instructions from the new deployment")
        .tools(Tools::builder().without_defaults().build()?)
        .thinking(Thinking::Low)
        .resume(snapshot)
        .build()?;
    assert_eq!(
        resumed
            .prompt("resume prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );

    drop((resumed, resumed_events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock HTTPS Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn failed_accepted_prompt_is_durable_without_partial_assistant_output() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut original = accept_async(stream).await?;
        assert_warmup(&next_json(&mut original).await?);
        send_warmup(&mut original, "resp-warmup").await?;
        let failed = next_json(&mut original).await?;
        assert!(failed.to_string().contains("failed durable prompt"));
        send_json(
            &mut original,
            json!({
                "type": "response.output_text.delta",
                "delta": "partial assistant must not survive"
            }),
        )
        .await?;
        send_json(
            &mut original,
            json!({
                "type": "error",
                "error": {
                    "code": "invalid_request_error",
                    "message": "terminal provider failure"
                }
            }),
        )
        .await?;

        let (stream, _) = listener.accept().await?;
        let mut resumed = accept_async(stream).await?;
        let replay = next_json(&mut resumed).await?;
        assert!(replay.get("previous_response_id").is_none());
        let replay = replay.to_string();
        assert!(replay.contains("failed durable prompt"));
        assert!(replay.contains("prompt after cold resume"));
        assert!(!replay.contains("partial assistant must not survive"));
        send_final(&mut resumed, "resp-resumed").await
    });

    let workspace = temporary_workspace("failed-turn-durability")?;
    let rollout_home = temporary_workspace("failed-turn-durability-rollout")?;
    let openai = || {
        OpenAi::builder("test-key")
            .websocket_url(endpoint.clone())
            .build()
    };
    let (agent, events) = Nanocodex::builder(openai()?)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .rollout(RolloutConfig::new(&rollout_home))
        .build()?;
    let error = agent
        .prompt("failed durable prompt")
        .await?
        .result()
        .await
        .expect_err("the provider failure must fail the accepted turn");
    assert!(error.to_string().contains("terminal provider failure"));
    agent.shutdown().await?;
    drop((agent, events));

    let durable = RolloutConfig::new(&rollout_home).load_session(TEST_SESSION_ID)?;
    let (thread_id, snapshot, rollout) = durable.into_parts();
    let (resumed, resumed_events) = Nanocodex::builder(openai()?)
        .thinking(Thinking::Low)
        .session_id(thread_id.parse()?)
        .resume(snapshot)
        .rollout(rollout)
        .build()?;
    assert_eq!(
        resumed
            .prompt("prompt after cold resume")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    resumed.shutdown().await?;
    drop((resumed, resumed_events));

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    std::fs::remove_dir_all(rollout_home)?;
    Ok(())
}

#[tokio::test]
async fn accepted_prompt_survives_a_fatal_warmup_boundary() -> Result<()> {
    let workspace = temporary_workspace("fatal-warmup-durability")?;
    let rollout_home = temporary_workspace("fatal-warmup-durability-rollout")?;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = || {
        let calls = Arc::clone(&calls);
        OpenAi::builder("test-key")
            .service(move || FailFirstWarmup {
                calls: Arc::clone(&calls),
            })
            .build()
    };
    let (agent, events) = Nanocodex::builder(openai()?)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .rollout(RolloutConfig::new(&rollout_home))
        .build()?;
    let error = agent
        .prompt("accepted before warmup failure")
        .await?
        .result()
        .await
        .expect_err("the fatal warmup boundary must fail the turn");
    assert!(error.to_string().contains("fatal warmup boundary"));
    agent.shutdown().await?;
    drop((agent, events));

    let durable = RolloutConfig::new(&rollout_home).load_session(TEST_SESSION_ID)?;
    assert!(serde_json::to_string(durable.snapshot())?.contains("accepted before warmup failure"));
    let (thread_id, snapshot, rollout) = durable.into_parts();
    let (resumed, resumed_events) = Nanocodex::builder(openai()?)
        .thinking(Thinking::Low)
        .session_id(thread_id.parse()?)
        .resume(snapshot)
        .rollout(rollout)
        .build()?;
    assert_eq!(
        resumed
            .prompt("prompt after warmup failure")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    resumed.shutdown().await?;
    drop((resumed, resumed_events));

    std::fs::remove_dir_all(workspace)?;
    std::fs::remove_dir_all(rollout_home)?;
    Ok(())
}
