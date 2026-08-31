use super::*;

async fn reject_provider_call(
    _attempt: nanocodex_oai_api::tower::ResponsesAttempt,
) -> std::result::Result<nanocodex_oai_api::tower::ResponsesServiceResponse, ResponseError> {
    panic!("token-budget local compaction must not call the provider")
}

#[tokio::test]
async fn new_context_rebuilds_a_full_window_and_persists_rollover_lineage() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let warmup = next_json(&mut socket).await?;
        let warmup_text = warmup.to_string();
        assert!(warmup_text.contains("new_context"), "{warmup}");
        assert!(warmup_text.contains("get_context_remaining"), "{warmup}");
        send_json(
            &mut socket,
            json!({
                "type": "response.completed",
                "response": {
                    "id": "resp-warmup",
                    "usage": null,
                    "usage_metadata": { "amount": "0.000001" }
                }
            }),
        )
        .await?;

        let first = next_json(&mut socket).await?;
        assert!(first.to_string().contains("<context_window>"), "{first}");
        send_json(
            &mut socket,
            completed_response(
                "resp-rollover",
                &[json!({
                    "type": "function_call",
                    "call_id": "call-new-context",
                    "name": "new_context",
                    "arguments": "{}"
                })],
            ),
        )
        .await?;

        let fresh = next_json(&mut socket).await?;
        assert!(fresh.get("previous_response_id").is_none(), "{fresh}");
        let fresh_text = fresh.to_string();
        assert!(!fresh_text.contains("new context task"), "{fresh}");
        assert!(fresh_text.contains("<environment_context>"), "{fresh}");
        assert!(
            fresh_text.contains("Previous context window id:"),
            "{fresh}"
        );
        assert!(!fresh_text.contains("call-new-context"), "{fresh}");
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("new-context-window")?;
    let openai = OpenAi::builder("test-key")
        .token_budget(nanocodex_oai_api::session::TokenBudgetConfig::default())
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;

    let result = agent.prompt("new context task").await?.result().await?;
    assert_eq!(result.final_message(), "done");
    let completions = result.response_completions();
    assert_eq!(completions.len(), 3);
    assert_eq!(completions[0].response_id(), "resp-warmup");
    assert_eq!(completions[0].operation(), ResponseOperation::Warmup);
    assert_eq!(
        completions[0]
            .usage_metadata()
            .and_then(|metadata| metadata.amount.as_deref()),
        Some("0.000001")
    );
    assert_eq!(completions[1].operation(), ResponseOperation::Generation);
    assert_eq!(completions[2].operation(), ResponseOperation::Generation);
    let snapshot = serde_json::to_value(result.snapshot().expect("completed turn snapshot"))?;
    assert_eq!(snapshot["context_window"]["number"], 1);
    assert_eq!(snapshot["context_window"]["first_id"], TEST_SESSION_ID);
    assert!(snapshot["context_window"]["previous_id"].is_string());
    assert_ne!(snapshot["context_window"]["current_id"], TEST_SESSION_ID);
    assert!(
        completions
            .iter()
            .all(|completion| completion.source() == ResponseCompletionSource::Live)
    );
    drop((agent, events));

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn token_budget_limit_rolls_over_locally_without_remote_compaction() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let first = next_json(&mut socket).await?;
        assert!(first.to_string().contains("exhaust this window"), "{first}");
        let mut exhausted = completed_response(
            "resp-exhausted",
            &[json!({
                "type": "function_call",
                "call_id": "call-context-remaining",
                "name": "get_context_remaining",
                "arguments": "{}"
            })],
        );
        exhausted["response"]["usage"] = json!({
            "input_tokens": 90,
            "output_tokens": 10,
            "total_tokens": 100
        });
        send_json(&mut socket, exhausted).await?;

        let fresh = next_json(&mut socket).await?;
        assert!(fresh.get("previous_response_id").is_none(), "{fresh}");
        let fresh_text = fresh.to_string();
        assert!(!fresh_text.contains("exhaust this window"), "{fresh}");
        assert!(!fresh_text.contains("call-context-remaining"), "{fresh}");
        assert!(
            fresh_text.contains("Previous context window id:"),
            "{fresh}"
        );
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("token-budget-local-rollover")?;
    let openai = OpenAi::builder("test-key")
        .token_budget(nanocodex_oai_api::session::TokenBudgetConfig::default())
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .context_window_tokens(100)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;

    let result = agent.prompt("exhaust this window").await?.result().await?;
    assert_eq!(result.final_message(), "done");
    assert!(
        result
            .response_completions()
            .iter()
            .all(|completion| completion.operation() != ResponseOperation::Compaction)
    );
    drop((agent, events));

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn token_budget_manual_compaction_is_a_local_fresh_window() -> Result<()> {
    let workspace = temporary_workspace("token-budget-manual-local")?;
    let rollout_home = temporary_workspace("token-budget-manual-rollout")?;
    let openai = OpenAi::builder("test-key")
        .token_budget(nanocodex_oai_api::session::TokenBudgetConfig::default())
        .service(|| tower::service_fn(reject_provider_call))
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .session_id(test_session_id())
        .rollout(RolloutConfig::new(&rollout_home))
        .build()?;

    agent.compact().await?;
    agent.flush_rollout().await?;
    let rollout_path = agent
        .rollout()
        .ok_or_else(|| eyre!("manual local compaction rollout was not configured"))?
        .path();
    let lines = std::fs::read_to_string(rollout_path)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    let compacted = lines
        .iter()
        .find(|line| line["type"] == "compacted")
        .ok_or_else(|| eyre!("local fresh window did not persist a compacted boundary"))?;
    assert_eq!(compacted["payload"]["window_number"], 1);
    assert_eq!(compacted["payload"]["message"], "");

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    std::fs::remove_dir_all(rollout_home)?;
    Ok(())
}

#[tokio::test]
async fn execution_environment_suppresses_host_context_discovery() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let request = next_json(&mut socket).await?.to_string();
        assert!(request.contains("remote task prompt"));
        assert!(request.contains("<current_date>2026-07-29</current_date>"));
        assert!(request.contains("<timezone>Etc/UTC</timezone>"));
        assert!(!request.contains("host-only instructions"));
        assert!(!request.contains("# AGENTS.md instructions"));
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("remote-empty-agents-context")?;
    std::fs::write(workspace.join("AGENTS.md"), "host-only instructions\n")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .execution_environment(ExecutionEnvironment::new("2026-07-29", "Etc/UTC"))
        .session_id(test_session_id())
        .build()?;
    assert_eq!(
        agent
            .prompt("remote task prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );

    drop((agent, events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn ordinary_turns_keep_creation_time_agents_md() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let first = next_json(&mut socket).await?;
        assert!(first.to_string().contains("creation-time agents"));
        send_final(&mut socket, "resp-first").await?;

        let second = next_json(&mut socket).await?;
        assert_eq!(second["previous_response_id"], "resp-first");
        let second = second.to_string();
        assert!(second.contains("second prompt"));
        assert!(!second.contains("disk mutation after creation"));
        assert!(!second.contains("replace all previously provided"));
        send_final(&mut socket, "resp-second").await
    });

    let workspace = temporary_workspace("ordinary-agents-context")?;
    std::fs::write(workspace.join("AGENTS.md"), "creation-time agents\n")?;
    let openai = OpenAi::builder("test-key")
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

    std::fs::write(
        workspace.join("AGENTS.md"),
        "disk mutation after creation\n",
    )?;
    assert_eq!(
        agent
            .prompt("second prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );

    drop((agent, events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn fork_replaces_or_removes_changed_agents_md_once() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        assert_warmup(&next_json(&mut root).await?);
        send_warmup(&mut root, "resp-root-warmup").await?;
        let initial = next_json(&mut root).await?;
        assert!(initial.to_string().contains("creation-time agents"));
        send_final(&mut root, "resp-root").await?;

        let (stream, _) = listener.accept().await?;
        let mut replacement = accept_async(stream).await?;
        let replaced = next_json(&mut replacement).await?;
        assert!(replaced.get("previous_response_id").is_none());
        let replaced = replaced.to_string();
        assert_eq!(replaced.matches("creation-time agents").count(), 1);
        assert!(replaced.contains(
            "These AGENTS.md instructions replace all previously provided AGENTS.md instructions."
        ));
        assert!(replaced.contains("replacement agents"));
        assert_eq!(
            replaced.matches("replace all previously provided").count(),
            1
        );
        send_final(&mut replacement, "resp-replacement").await?;

        let (stream, _) = listener.accept().await?;
        let mut removal = accept_async(stream).await?;
        let removed = next_json(&mut removal).await?;
        assert!(removed.get("previous_response_id").is_none());
        let removed = removed.to_string();
        assert_eq!(removed.matches("creation-time agents").count(), 1);
        assert!(
            removed.contains("The previously provided AGENTS.md instructions no longer apply.")
        );
        assert_eq!(removed.matches("instructions no longer apply").count(), 1);
        send_final(&mut removal, "resp-removal").await
    });

    let workspace = temporary_workspace("fork-agents-context")?;
    std::fs::write(workspace.join("AGENTS.md"), "creation-time agents\n")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, root_events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let first = agent.prompt("root prompt").await?.result().await?;

    std::fs::write(workspace.join("AGENTS.md"), "replacement agents\n")?;
    let (replacement, replacement_events) = agent.fork_from(&first).await?;
    assert_eq!(
        replacement
            .prompt("replacement branch")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );

    std::fs::remove_file(workspace.join("AGENTS.md"))?;
    let (removal, removal_events) = agent.fork_from(&first).await?;
    assert_eq!(
        removal
            .prompt("removal branch")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );

    drop((
        agent,
        replacement,
        removal,
        root_events,
        replacement_events,
        removal_events,
    ));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn fork_reloads_a_changed_global_agents_source_once() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        assert_warmup(&next_json(&mut root).await?);
        send_warmup(&mut root, "resp-root-warmup").await?;
        let initial = next_json(&mut root).await?;
        assert!(initial.to_string().contains("original global agents"));
        send_final(&mut root, "resp-root").await?;

        let (stream, _) = listener.accept().await?;
        let mut fork = accept_async(stream).await?;
        let replaced = next_json(&mut fork).await?;
        assert!(replaced.get("previous_response_id").is_none());
        let replaced = replaced.to_string();
        assert_eq!(replaced.matches("original global agents").count(), 1);
        assert!(replaced.contains("replacement global agents"));
        assert_eq!(
            replaced.matches("replace all previously provided").count(),
            1
        );
        send_final(&mut fork, "resp-fork").await
    });

    let workspace = temporary_workspace("fork-global-context")?;
    let codex_home = temporary_workspace("fork-global-codex-home")?;
    std::fs::write(codex_home.join("AGENTS.md"), "original global agents\n")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, root_events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .codex_home(&codex_home)
        .session_id(test_session_id())
        .build()?;
    let first = agent.prompt("root prompt").await?.result().await?;

    std::fs::write(codex_home.join("AGENTS.md"), "replacement global agents\n")?;
    let (fork, fork_events) = agent.fork_from(&first).await?;
    assert_eq!(
        fork.prompt("fork prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );

    drop((agent, fork, root_events, fork_events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock global-context server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    std::fs::remove_dir_all(codex_home)?;
    Ok(())
}

#[tokio::test]
async fn legacy_snapshot_reconstructs_agents_md_before_diffing() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut original = accept_async(stream).await?;
        assert_warmup(&next_json(&mut original).await?);
        send_warmup(&mut original, "resp-warmup").await?;
        let initial = next_json(&mut original).await?;
        assert!(initial.to_string().contains("legacy original agents"));
        send_final(&mut original, "resp-original").await?;

        let (stream, _) = listener.accept().await?;
        let mut resumed = accept_async(stream).await?;
        let replay = next_json(&mut resumed).await?;
        let replay = replay.to_string();
        assert!(replay.contains("legacy original agents"));
        assert!(replay.contains(
            "These AGENTS.md instructions replace all previously provided AGENTS.md instructions."
        ));
        assert!(replay.contains("legacy replacement agents"));
        send_final(&mut resumed, "resp-resumed").await
    });

    let workspace = temporary_workspace("legacy-agents-context")?;
    std::fs::write(workspace.join("AGENTS.md"), "legacy original agents\n")?;
    let openai = || {
        OpenAi::builder("test-key")
            .websocket_url(endpoint.clone())
            .build()
    };
    let (agent, events) = Nanocodex::builder(openai()?)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let first = agent.prompt("first prompt").await?.result().await?;
    let mut legacy = serde_json::to_value(
        first
            .snapshot()
            .expect("local turns always retain a snapshot"),
    )?;
    legacy
        .as_object_mut()
        .ok_or_else(|| eyre!("snapshot is not an object"))?
        .remove("context_snapshot");
    let legacy: SessionSnapshot = serde_json::from_value(legacy)?;
    agent.shutdown().await?;
    drop((agent, events, first));

    std::fs::write(workspace.join("AGENTS.md"), "legacy replacement agents\n")?;
    let (resumed, resumed_events) = Nanocodex::builder(openai()?)
        .thinking(Thinking::Low)
        .resume(legacy)
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
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}
