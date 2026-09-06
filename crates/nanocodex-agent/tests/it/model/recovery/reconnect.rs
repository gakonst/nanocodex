use super::*;

#[tokio::test]
async fn reconnect_drops_previous_response_id_and_replays_full_history() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut first = accept_async(stream).await?;
        let warmup = next_json(&mut first).await?;
        assert_warmup(&warmup);
        send_warmup(&mut first, "resp-warmup").await?;
        let generation = next_json(&mut first).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut first,
            completed_response(
                "resp-tool",
                &[json!({
                    "id": "server-item-id",
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "text(\"continued\")"
                })],
            ),
        )
        .await?;
        first.send(Message::Close(None)).await?;
        drop(first);

        let (stream, _) = listener.accept().await?;
        let mut second = accept_async(stream).await?;
        let replay = next_json(&mut second).await?;
        assert!(replay.get("previous_response_id").is_none());
        assert_eq!(replay["store"], false);
        assert_eq!(replay["input"].as_array().map(Vec::len), Some(7));
        assert_eq!(replay["input"][0]["type"], "additional_tools");
        assert_eq!(replay["input"][1]["role"], "developer");
        assert_eq!(replay["input"][2]["role"], "developer");
        assert_eq!(replay["input"][3]["role"], "user");
        assert_eq!(replay["input"][5]["type"], "custom_tool_call");
        assert!(replay["input"][5].get("id").is_none());
        assert_eq!(replay["input"][6]["type"], "custom_tool_call_output");
        assert_client_item_id(&replay["input"][6], "ctco");
        send_final(&mut second, "resp-final").await
    });

    let workspace = temporary_workspace("reconnect")?;
    run_model(&endpoint, &workspace, "exercise reconnect").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn stored_reconnect_drops_checkpoint_and_replays_full_history() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut first = accept_async(stream).await?;
        let warmup = next_json(&mut first).await?;
        assert_warmup_with_store(&warmup, true);
        send_warmup(&mut first, "resp-warmup").await?;
        let generation = next_json(&mut first).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        assert_eq!(generation["store"], true);
        send_json(
            &mut first,
            completed_response(
                "resp-tool",
                &[json!({
                    "id": "server-item-id",
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "text(\"continued\")"
                })],
            ),
        )
        .await?;
        first.send(Message::Close(None)).await?;
        drop(first);

        let (stream, _) = listener.accept().await?;
        let mut replacement = accept_async(stream).await?;
        let replay = next_json(&mut replacement).await?;
        assert!(replay.get("previous_response_id").is_none());
        assert_eq!(replay["store"], true);
        assert_eq!(replay["input"].as_array().map(Vec::len), Some(7));
        assert_eq!(replay["input"][0]["type"], "additional_tools");
        assert_eq!(replay["input"][1]["role"], "developer");
        assert_eq!(replay["input"][2]["role"], "developer");
        assert_eq!(replay["input"][3]["role"], "user");
        assert_eq!(replay["input"][5]["type"], "custom_tool_call");
        assert!(replay["input"][5].get("id").is_none());
        assert_eq!(replay["input"][6]["type"], "custom_tool_call_output");
        assert_client_item_id(&replay["input"][6], "ctco");
        send_final(&mut replacement, "resp-final").await
    });

    let workspace = temporary_workspace("stored-reconnect")?;
    let openai = OpenAi::builder("test-key")
        .experimental_context(false)
        .websocket_url(&endpoint)
        .store(true)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let turn = agent.prompt("exercise stored reconnect").await?;
    drop(agent);
    let mut output = Vec::new();
    let (event_result, turn_result) = tokio::join!(events.write_jsonl(&mut output), turn.result());
    event_result?;
    turn_result?;

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn receive_reset_reconnects_without_replaying_completed_tools() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut first = accept_async(stream).await?;
        assert_warmup(&next_json(&mut first).await?);
        send_warmup(&mut first, "resp-warmup").await?;

        let generation = next_json(&mut first).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut first,
            completed_response(
                "resp-tool",
                &[json!({
                    "id": "server-item-id",
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "const result = await tools.exec_command({cmd: \"printf x >> marker.txt\"}); text(result.output);"
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut first).await?;
        assert_eq!(continuation["previous_response_id"], "resp-tool");
        assert_eq!(continuation["input"].as_array().map(Vec::len), Some(1));
        let tool_output = continuation["input"][0].clone();
        send_json(
            &mut first,
            json!({
                "type": "response.created",
                "response": { "id": "resp-interrupted" }
            }),
        )
        .await?;
        send_json(
            &mut first,
            json!({
                "type": "response.in_progress",
                "response": { "id": "resp-interrupted" }
            }),
        )
        .await?;
        send_json(
            &mut first,
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "reasoning" }
            }),
        )
        .await?;
        drop(first);

        let (stream, _) = listener.accept().await?;
        let mut second = accept_async(stream).await?;
        let replay = next_json(&mut second).await?;
        assert!(replay.get("previous_response_id").is_none());
        assert_eq!(replay["input"].as_array().map(Vec::len), Some(7));
        assert_eq!(replay["input"][5]["type"], "custom_tool_call");
        assert_eq!(replay["input"][5]["call_id"], "call-exec");
        assert_eq!(replay["input"][6], tool_output);
        send_final(&mut second, "resp-final").await
    });

    let workspace = temporary_workspace("receive-reconnect")?;
    let output = run_model(&endpoint, &workspace, "recover after a receive reset").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert_eq!(std::fs::read_to_string(workspace.join("marker.txt"))?, "x");
    assert!(output.contains("\"model.attempt.retrying\""));
    assert!(output.contains("failed to receive a Responses WebSocket frame"));
    assert!(output.contains("\"purpose\":\"reconnect\""));
    assert!(output.contains("\"connection_attempts\":2"));
    assert!(output.contains("\"websocket_reconnects\":1"));
    assert!(output.contains("\"model_calls\":2"));
    assert!(!output.contains("\"model.call.failed\""));
    assert!(output.contains("\"run.completed\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn sol_compacts_with_the_session_agents_md_and_installs_the_returned_context() -> Result<()> {
    let workspace = temporary_workspace("compaction")?;
    std::fs::write(
        workspace.join("AGENTS.md"),
        "original session instructions\n",
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut socket,
            completed_response_with_usage(
                "resp-tool",
                &[json!({
                    "id": "item-exec",
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "await tools.apply_patch(\"*** Begin Patch\\n*** Update File: AGENTS.md\\n@@\\n-original session instructions\\n+fresh compacted instructions\\n*** End Patch\"); text(\"tool completed\")"
                })],
                372_001,
            ),
        )
        .await?;

        let compact = next_json(&mut socket).await?;
        assert_eq!(compact["previous_response_id"], "resp-tool");
        assert_eq!(compact["input"].as_array().map(Vec::len), Some(2));
        assert_eq!(compact["input"][0]["type"], "custom_tool_call_output");
        assert!(
            compact["input"][0]["output"]
                .to_string()
                .contains("tool completed")
        );
        assert_eq!(compact["input"][1], json!({ "type": "compaction_trigger" }));
        send_json(
            &mut socket,
            json!({
                "type": "response.output_item.done",
                "item": {
                    "id": "cmp_01a0710d-9f5e-7f80-91ad-730ae4a6ba93",
                    "type": "compaction",
                    "encrypted_content": "opaque-summary"
                }
            }),
        )
        .await?;
        send_json(
            &mut socket,
            completed_response_with_usage("resp-compact", &[], 120),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert!(continuation.get("previous_response_id").is_none());
        assert_eq!(continuation["input"].as_array().map(Vec::len), Some(6));
        assert_eq!(continuation["input"][0]["type"], "additional_tools");
        assert_eq!(continuation["input"][1]["role"], "developer");
        assert_eq!(continuation["input"][2]["role"], "developer");
        assert_eq!(continuation["input"][3]["role"], "user");
        assert_eq!(continuation["input"][4]["role"], "user");
        assert_eq!(continuation["input"][5]["type"], "compaction");
        assert_eq!(
            continuation["input"][5]["encrypted_content"],
            "opaque-summary"
        );
        assert_eq!(
            continuation["input"][5]["id"],
            "cmp_01a0710d-9f5e-7f80-91ad-730ae4a6ba93"
        );
        assert!(continuation.to_string().contains("exercise compaction"));
        assert!(
            continuation
                .to_string()
                .contains("original session instructions")
        );
        assert!(
            !continuation
                .to_string()
                .contains("fresh compacted instructions")
        );
        assert!(!continuation.to_string().contains("tool completed"));
        send_final(&mut socket, "resp-final").await
    });

    let output = run_model(&endpoint, &workspace, "exercise compaction").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"model.compaction.started\""));
    assert!(output.contains("\"model.compaction.completed\""));
    assert!(output.contains("\"compactions\":1"));
    let terminal = output
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<serde_json::Result<Vec<_>>>()?
        .into_iter()
        .find(|event| event["type"] == "run.completed")
        .ok_or_else(|| eyre!("compaction run did not emit a terminal event"))?;
    let compaction_duration_ns = terminal["payload"]["compaction_duration_ns"]
        .as_u64()
        .ok_or_else(|| eyre!("terminal event omitted compaction duration"))?;
    assert!(compaction_duration_ns > 0);
    assert!(
        compaction_duration_ns
            <= terminal["payload"]["model_duration_ns"]
                .as_u64()
                .ok_or_else(|| eyre!("terminal event omitted aggregate model duration"))?
    );
    let terminal = serde_json::from_value::<AgentEvent>(terminal)?;
    let AgentEventData::Run(RunEvent::Completed(terminal)) = terminal.data()? else {
        return Err(eyre!("terminal event did not decode as a completed run"));
    };
    assert_eq!(
        terminal.metrics.compaction_duration_ns,
        compaction_duration_ns
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn sol_compacts_before_sampling_a_follow_on_turn() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let first = next_json(&mut socket).await?;
        assert!(first.to_string().contains("first prompt"));
        send_json(
            &mut socket,
            completed_response_with_usage(
                "resp-first",
                &[json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "done" }]
                })],
                244_800,
            ),
        )
        .await?;

        let compact = next_json(&mut socket).await?;
        assert_eq!(compact["previous_response_id"], "resp-first");
        assert_eq!(compact["input"], json!([{ "type": "compaction_trigger" }]));
        assert!(!compact.to_string().contains("second prompt"));
        send_json(
            &mut socket,
            json!({
                "type": "response.output_item.done",
                "item": {
                    "id": "cmp_01a0710d-9f5e-7f80-91ad-730ae4a6ba93",
                    "type": "compaction",
                    "encrypted_content": "opaque-summary"
                }
            }),
        )
        .await?;
        send_json(
            &mut socket,
            completed_response_with_usage("resp-compact", &[], 120),
        )
        .await?;

        let second = next_json(&mut socket).await?;
        assert!(second.get("previous_response_id").is_none());
        assert!(second.to_string().contains("second prompt"));
        let second_input = second["input"]
            .as_array()
            .ok_or_else(|| eyre!("follow-on request input was not an array"))?;
        let compact_index = second_input.len().saturating_sub(4);
        assert!(
            second_input[compact_index.saturating_sub(1)]
                .to_string()
                .contains("first prompt")
        );
        assert_eq!(second_input[compact_index]["type"], "compaction");
        assert_eq!(
            second_input[compact_index]["id"],
            "cmp_01a0710d-9f5e-7f80-91ad-730ae4a6ba93"
        );
        assert_eq!(
            second_input[compact_index]["encrypted_content"],
            "opaque-summary"
        );
        assert_eq!(second_input[compact_index + 1]["role"], "developer");
        assert!(
            second_input[compact_index + 2]
                .to_string()
                .contains("<environment_context>")
        );
        assert!(
            second_input
                .last()
                .is_some_and(|item| item.to_string().contains("second prompt"))
        );
        send_final(&mut socket, "resp-second").await
    });

    let workspace = temporary_workspace("pre-turn-compaction")?;
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
