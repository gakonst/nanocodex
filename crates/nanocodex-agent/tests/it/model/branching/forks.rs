use super::*;

#[tokio::test]
async fn completed_fork_replays_history_without_the_parent_transport_checkpoint() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        assert_warmup(&next_json(&mut root).await?);
        send_warmup(&mut root, "resp-warmup").await?;

        let root_turn = next_json(&mut root).await?;
        assert_eq!(root_turn["previous_response_id"], "resp-warmup");
        send_final(&mut root, "resp-root").await?;

        let (stream, _) = listener.accept().await?;
        let mut branch = accept_async(stream).await?;
        let fork = next_json(&mut branch).await?;
        assert!(fork.get("previous_response_id").is_none());
        let fork_text = fork.to_string();
        assert!(fork_text.contains("completed root prompt"));
        assert!(fork_text.contains("BTW question"));
        send_final(&mut branch, "resp-branch").await
    });

    let workspace = temporary_workspace("completed-fork-full-replay")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, root_events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    agent
        .prompt("completed root prompt")
        .await?
        .result()
        .await?;
    let (fork, fork_events) = agent.fork().await?;
    assert_eq!(
        fork.prompt("BTW question")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );

    drop((agent, fork, root_events, fork_events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn latest_fork_during_streaming_inherits_the_active_prompt_delta() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (root_started, root_started_rx) = tokio::sync::oneshot::channel();
    let (release_root, release_root_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        assert_warmup(&next_json(&mut root).await?);
        send_warmup(&mut root, "resp-warmup").await?;

        let active = next_json(&mut root).await?;
        assert_eq!(active["previous_response_id"], "resp-warmup");
        assert_eq!(active["reasoning"]["effort"], "low");
        assert!(active.to_string().contains("active root prompt"));
        root_started
            .send(())
            .map_err(|()| eyre!("root request signal receiver dropped"))?;

        let (stream, _) = listener.accept().await?;
        let mut branch = accept_async(stream).await?;
        let fork = next_json(&mut branch).await?;
        assert!(fork.get("previous_response_id").is_none());
        assert_eq!(fork["reasoning"]["effort"], "high");
        let fork_text = fork.to_string();
        assert!(fork_text.contains("active root prompt"));
        assert!(fork_text.contains("BTW question"));
        send_final(&mut branch, "resp-branch").await?;

        release_root_rx
            .await
            .map_err(|_| eyre!("root release sender dropped"))?;
        send_final(&mut root, "resp-root").await
    });

    let workspace = temporary_workspace("active-prompt-fork")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, root_events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let root = agent.prompt("active root prompt").await?;
    root_started_rx
        .await
        .map_err(|_| eyre!("root request was not observed"))?;
    agent.set_thinking(Thinking::High).await?;
    let (fork, fork_events) = agent.fork().await?;
    let branch = fork.prompt("BTW question").await?;
    assert_eq!(branch.result().await?.final_message(), "done");
    release_root
        .send(())
        .map_err(|()| eyre!("root release receiver dropped"))?;
    assert_eq!(root.result().await?.final_message(), "done");

    drop((agent, fork, root_events, fork_events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn active_boundary_fork_sends_tool_and_steer_delta_then_replays_on_checkpoint_miss()
-> Result<()> {
    let workspace = temporary_workspace("active-tool-steer-fork")?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (boundary_seen, boundary_seen_rx) = tokio::sync::oneshot::channel();
    let (release_root, release_root_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        assert_warmup_with_store(&next_json(&mut root).await?, true);
        send_warmup(&mut root, "resp-warmup").await?;

        let initial = next_json(&mut root).await?;
        assert_eq!(initial["previous_response_id"], "resp-warmup");
        send_json(
            &mut root,
            completed_response(
                "resp-tool",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "const result = await tools.exec_command({cmd: \"printf started > tool-started; while [ ! -f release-tool ]; do sleep 0.01; done; printf shit\"}); text(result.output);"
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut root).await?;
        assert_eq!(continuation["previous_response_id"], "resp-tool");
        assert_eq!(continuation["input"].as_array().map(Vec::len), Some(2));
        assert_eq!(continuation["input"][0]["type"], "custom_tool_call_output");
        assert!(continuation["input"][0].to_string().contains("shit"));
        let tool_output_id = continuation["input"][0]["id"]
            .as_str()
            .ok_or_else(|| eyre!("root continuation omitted the tool output ID"))?
            .to_owned();
        assert_eq!(
            continuation["input"][1]["content"][0]["text"],
            "print shat instead"
        );
        boundary_seen
            .send(())
            .map_err(|()| eyre!("boundary signal receiver dropped"))?;

        let (stream, _) = listener.accept().await?;
        let mut branch = accept_async(stream).await?;
        let incremental = next_json(&mut branch).await?;
        assert_eq!(incremental["previous_response_id"], "resp-tool");
        assert_eq!(incremental["input"].as_array().map(Vec::len), Some(3));
        assert_eq!(incremental["input"][0]["type"], "custom_tool_call_output");
        assert!(incremental["input"][0].to_string().contains("shit"));
        assert_eq!(incremental["input"][0]["id"], tool_output_id);
        assert_eq!(
            incremental["input"][1]["content"][0]["text"],
            "print shat instead"
        );
        assert_eq!(
            incremental["input"][2]["content"][0]["text"],
            "BTW question"
        );
        send_json(
            &mut branch,
            json!({
                "type": "error",
                "error": {
                    "code": "previous_response_not_found",
                    "message": "checkpoint expired"
                }
            }),
        )
        .await?;

        let replay = next_json(&mut branch).await?;
        assert!(replay.get("previous_response_id").is_none());
        let replay_tool_output = replay["input"]
            .as_array()
            .and_then(|items| {
                items.iter().find(|item| {
                    item["type"] == "custom_tool_call_output" && item["call_id"] == "call-exec"
                })
            })
            .ok_or_else(|| eyre!("full replay omitted the inherited tool output"))?;
        assert_eq!(replay_tool_output["id"], tool_output_id);
        let replay_text = replay.to_string();
        assert!(replay_text.contains("active root prompt"));
        assert!(replay_text.contains("call-exec"));
        assert!(replay_text.contains("shit"));
        assert!(replay_text.contains("print shat instead"));
        assert!(replay_text.contains("BTW question"));
        send_final(&mut branch, "resp-branch").await?;

        release_root_rx
            .await
            .map_err(|_| eyre!("root release sender dropped"))?;
        send_final(&mut root, "resp-root").await
    });

    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .store(true)
        .build()?;
    let (agent, root_events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let root = agent.prompt("active root prompt").await?;
    timeout(std::time::Duration::from_secs(5), async {
        while !workspace.join("tool-started").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| eyre!("tool process did not start"))?;
    root.steer("print shat instead").await?;
    std::fs::write(workspace.join("release-tool"), [])?;
    boundary_seen_rx
        .await
        .map_err(|_| eyre!("root boundary request was not observed"))?;

    let (fork, fork_events) = agent.fork().await?;
    assert_eq!(
        fork.prompt("BTW question")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    release_root
        .send(())
        .map_err(|()| eyre!("root release receiver dropped"))?;
    assert_eq!(root.result().await?.final_message(), "done");

    drop((agent, fork, root_events, fork_events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn latest_and_historical_forks_keep_distinct_boundaries_during_an_active_turn() -> Result<()>
{
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (active_seen, active_seen_rx) = tokio::sync::oneshot::channel();
    let (release_active, release_active_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        assert_warmup_with_store(&next_json(&mut root).await?, true);
        send_warmup(&mut root, "resp-warmup").await?;
        let first = next_json(&mut root).await?;
        assert!(first.to_string().contains("completed root prompt"));
        send_final(&mut root, "resp-first").await?;

        let active = next_json(&mut root).await?;
        assert_eq!(active["previous_response_id"], "resp-first");
        assert!(active.to_string().contains("active root prompt"));
        active_seen
            .send(())
            .map_err(|()| eyre!("active signal receiver dropped"))?;

        let (stream, _) = listener.accept().await?;
        let mut latest = accept_async(stream).await?;
        let latest_request = next_json(&mut latest).await?;
        assert_eq!(latest_request["previous_response_id"], "resp-first");
        assert_eq!(latest_request["input"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            latest_request["input"][0]["content"][0]["text"],
            "active root prompt"
        );
        assert_eq!(
            latest_request["input"][1]["content"][0]["text"],
            "latest branch prompt"
        );
        send_final(&mut latest, "resp-latest").await?;

        let (stream, _) = listener.accept().await?;
        let mut historical = accept_async(stream).await?;
        let historical_request = next_json(&mut historical).await?;
        assert_eq!(historical_request["previous_response_id"], "resp-first");
        assert_eq!(
            historical_request["input"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            historical_request["input"][0]["content"][0]["text"],
            "historical branch prompt"
        );
        assert!(
            !historical_request
                .to_string()
                .contains("active root prompt")
        );
        send_final(&mut historical, "resp-historical").await?;

        release_active_rx
            .await
            .map_err(|_| eyre!("active release sender dropped"))?;
        send_final(&mut root, "resp-active").await
    });

    let workspace = temporary_workspace("latest-vs-historical-fork")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .store(true)
        .build()?;
    let (agent, root_events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let completed = agent
        .prompt("completed root prompt")
        .await?
        .result()
        .await?;
    let active = agent.prompt("active root prompt").await?;
    active_seen_rx
        .await
        .map_err(|_| eyre!("active root request was not observed"))?;

    let (latest, latest_events) = agent.fork().await?;
    assert_eq!(
        latest
            .prompt("latest branch prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    let (historical, historical_events) = agent.fork_from(&completed).await?;
    assert_eq!(
        historical
            .prompt("historical branch prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    release_active
        .send(())
        .map_err(|()| eyre!("active release receiver dropped"))?;
    assert_eq!(active.result().await?.final_message(), "done");

    drop((
        agent,
        latest,
        historical,
        root_events,
        latest_events,
        historical_events,
    ));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn historical_fork_runs_while_the_mainline_turn_is_in_flight() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (root_started, root_started_rx) = tokio::sync::oneshot::channel();
    let (branch_started, branch_started_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        let warmup = next_json(&mut root).await?;
        assert_warmup_with_store(&warmup, true);
        let lineage = warmup["prompt_cache_key"].clone();
        let root_session = warmup["client_metadata"]["session_id"].clone();
        send_warmup(&mut root, "resp-warmup").await?;

        let first = next_json(&mut root).await?;
        assert_eq!(first["previous_response_id"], "resp-warmup");
        send_final(&mut root, "resp-first").await?;
        let second = next_json(&mut root).await?;
        assert_eq!(second["previous_response_id"], "resp-first");
        send_final(&mut root, "resp-second").await?;

        let mainline = next_json(&mut root).await?;
        assert_eq!(mainline["previous_response_id"], "resp-second");
        root_started
            .send(())
            .map_err(|()| eyre!("root signal dropped"))?;
        let root_task = tokio::spawn(async move {
            branch_started_rx
                .await
                .map_err(|_| eyre!("branch signal dropped"))?;
            send_final(&mut root, "resp-mainline").await
        });

        let (stream, _) = listener.accept().await?;
        let mut branch = accept_async(stream).await?;
        let fork = next_json(&mut branch).await?;
        assert_eq!(fork["previous_response_id"], "resp-first");
        assert_eq!(fork["prompt_cache_key"], lineage);
        assert_ne!(fork["client_metadata"]["session_id"], root_session);
        assert_eq!(fork["input"].as_array().map(Vec::len), Some(1));
        assert_eq!(fork["input"][0]["content"][0]["text"], "fork prompt");
        branch_started
            .send(())
            .map_err(|()| eyre!("branch signal receiver dropped"))?;
        send_final(&mut branch, "resp-fork").await?;
        root_task.await??;
        Result::<()>::Ok(())
    });

    let workspace = temporary_workspace("historical-fork")?;
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
        .prompt(Prompt::new("first prompt"))
        .await?
        .result()
        .await?;
    agent.prompt("second prompt").await?.result().await?;

    let mainline = agent.prompt("continue mainline").await?;
    root_started_rx
        .await
        .map_err(|_| eyre!("root request was not observed"))?;
    let (fork, fork_events) = agent.fork_from(&first).await?;
    let branch = fork.prompt("fork prompt").await?;
    let (mainline, branch) = tokio::join!(mainline.result(), branch.result());
    assert_eq!(mainline?.final_message(), "done");
    assert_eq!(branch?.final_message(), "done");

    drop((agent, fork, root_events, fork_events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}
