use super::*;

#[tokio::test]
async fn assistant_events_preserve_commentary_and_final_answer_phases() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let initial = next_json(&mut socket).await?;
        assert_eq!(initial["previous_response_id"], "resp-warmup");
        let commentary = send_assistant_output(
            &mut socket,
            0,
            "msg-commentary",
            "commentary",
            "I’ll verify.",
        )
        .await?;
        send_json(
            &mut socket,
            completed_response(
                "resp-commentary",
                &[
                    commentary,
                    json!({
                        "id": "call-item",
                        "type": "custom_tool_call",
                        "call_id": "call-exec",
                        "name": "exec",
                        "input": "text(\"observed\");"
                    }),
                ],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-commentary");
        let final_answer =
            send_assistant_output(&mut socket, 0, "msg-final", "final_answer", "Done.").await?;
        send_json(
            &mut socket,
            completed_response("resp-final", &[final_answer]),
        )
        .await
    });

    let workspace = temporary_workspace("assistant-phases")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let turn = agent.prompt("check the live state").await?;
    assert_eq!(turn.result().await?.final_message(), "Done.");
    drop(agent);

    let mut deltas = Vec::new();
    let mut messages = Vec::new();
    let mut timeline = Vec::new();
    while let Some(event) = events.recv().await {
        match event.kind {
            AgentEventKind::AssistantDelta => {
                deltas.push(event.decode_payload::<Value>()?);
            }
            AgentEventKind::AssistantMessage => {
                let message = event.decode_payload::<Value>()?;
                timeline.push(message["phase"].clone());
                messages.push(message);
            }
            AgentEventKind::ToolCall => {
                timeline.push(json!("tool.call"));
            }
            AgentEventKind::ToolResult => {
                timeline.push(json!("tool.result"));
            }
            _ => {}
        }
    }
    assert_assistant_phase_events(&deltas, &messages, &timeline);

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

fn assert_assistant_phase_events(deltas: &[Value], messages: &[Value], timeline: &[Value]) {
    let expected_messages = [
        json!({
            "model_call_index": 1,
            "item_id": "msg-commentary",
            "phase": "commentary",
            "text": "I’ll verify."
        }),
        json!({
            "model_call_index": 2,
            "item_id": "msg-final",
            "phase": "final_answer",
            "text": "Done."
        }),
    ];
    assert_eq!(deltas, expected_messages);
    assert_eq!(messages, expected_messages);
    assert_eq!(
        timeline,
        [
            json!("commentary"),
            json!("tool.call"),
            json!("tool.result"),
            json!("final_answer")
        ]
    );
}

#[tokio::test]
async fn steering_is_bounded_fifo_and_joins_at_the_next_model_boundary() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (first_seen, first_seen_rx) = tokio::sync::oneshot::channel();
    let (release_first, release_first_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let first = next_json(&mut socket).await?;
        assert_eq!(first["previous_response_id"], "resp-warmup");
        assert_eq!(first["input"][2]["content"][0]["text"], "initial task");
        first_seen
            .send(())
            .map_err(|()| eyre!("first-request signal receiver dropped"))?;
        release_first_rx
            .await
            .map_err(|_| eyre!("first-request release sender dropped"))?;
        send_final(&mut socket, "resp-first").await?;

        let steered = next_json(&mut socket).await?;
        assert_eq!(steered["previous_response_id"], "resp-first");
        assert_eq!(steered["input"].as_array().map(Vec::len), Some(8));
        for index in 0..8 {
            assert_eq!(steered["input"][index]["role"], "user");
            assert_eq!(
                steered["input"][index]["content"][0]["text"],
                format!("constraint {index}")
            );
        }
        send_final(&mut socket, "resp-steered").await
    });

    let workspace = temporary_workspace("steer")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let turn = agent.prompt(Prompt::new("initial task")).await?;
    first_seen_rx
        .await
        .map_err(|_| eyre!("first request was not observed"))?;
    for index in 0..8 {
        turn.steer(format!("constraint {index}")).await?;
    }
    let overflow = turn.steer("constraint 8").await.unwrap_err();
    assert!(matches!(overflow, NanocodexError::SteerQueueFull));
    release_first
        .send(())
        .map_err(|()| eyre!("server release receiver dropped"))?;
    assert_eq!(turn.result().await?.final_message(), "done");
    drop(agent);

    let mut steered_events = 0;
    let mut terminal = None;
    while let Some(event) = events.recv().await {
        match event.kind {
            AgentEventKind::RunSteered => {
                steered_events += 1;
                let payload = event.decode_payload::<Value>()?;
                assert_eq!(payload["steer_index"], steered_events);
                assert_eq!(payload["instruction_bytes"], "constraint 0".len());
            }
            AgentEventKind::RunCompleted => {
                terminal = Some(event.decode_payload::<Value>()?);
            }
            _ => {}
        }
    }
    assert_eq!(steered_events, 8);
    assert_eq!(
        terminal.as_ref().map(|payload| &payload["steers"]),
        Some(&json!(8))
    );

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn steering_during_a_tool_call_joins_after_the_tool_result() -> Result<()> {
    let workspace = temporary_workspace("steer-tool")?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let initial = next_json(&mut socket).await?;
        assert_eq!(initial["previous_response_id"], "resp-warmup");
        send_json(
            &mut socket,
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

        let steered = next_json(&mut socket).await?;
        assert_eq!(steered["previous_response_id"], "resp-tool");
        let input = steered["input"]
            .as_array()
            .ok_or_else(|| eyre!("steered request input was not an array"))?;
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "custom_tool_call_output");
        assert_eq!(input[0]["call_id"], "call-exec");
        assert!(input[0].to_string().contains("shit"));
        assert_eq!(input[1]["role"], "user");
        assert_eq!(input[1]["content"][0]["text"], "print shat instead");
        send_final(&mut socket, "resp-steered").await
    });

    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let turn = match agent.route_prompt("print shit a lot of times").await? {
        PromptRoute::Started(turn) => turn,
        PromptRoute::Steered => return Err(eyre!("idle live input unexpectedly steered a turn")),
    };
    timeout(std::time::Duration::from_secs(5), async {
        while !workspace.join("tool-started").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| eyre!("tool process did not start"))?;

    assert!(matches!(
        agent.route_prompt("print shat instead").await?,
        PromptRoute::Steered
    ));
    assert!(!workspace.join("release-tool").exists());
    std::fs::write(workspace.join("release-tool"), [])?;
    assert_eq!(turn.result().await?.final_message(), "done");
    drop(agent);

    let mut saw_steer = false;
    while let Some(event) = events.recv().await {
        saw_steer |= event.kind == AgentEventKind::RunSteered;
    }
    assert!(saw_steer);
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn compaction_resumes_tool_continuation_before_queued_steering() -> Result<()> {
    let workspace = temporary_workspace("steer-after-compaction")?;
    let server_workspace = workspace.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        drop(next_json(&mut socket).await?);
        send_json(
            &mut socket,
            completed_response_with_usage(
                "resp-tool",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "const result = await tools.exec_command({cmd: \"printf started > tool-started; while [ ! -f release-tool ]; do sleep 0.01; done; printf tool-output\"}); text(result.output);"
                })],
                372_001,
            ),
        )
        .await?;

        let compact = next_json(&mut socket).await?;
        assert_eq!(compact["previous_response_id"], "resp-tool");
        assert!(compact.to_string().contains("tool-output"));
        assert!(!compact.to_string().contains("queued steer"));
        assert_eq!(
            compact["input"].as_array().and_then(|input| input.last()),
            Some(&json!({ "type": "compaction_trigger" }))
        );
        send_json(
            &mut socket,
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
        send_json(
            &mut socket,
            completed_response_with_usage("resp-compact", &[], 120),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert!(continuation.get("previous_response_id").is_none());
        assert!(continuation.to_string().contains("opaque-summary"));
        assert!(!continuation.to_string().contains("queued steer"));
        send_final(&mut socket, "resp-continuation").await?;

        let steered = next_json(&mut socket).await?;
        assert_eq!(steered["previous_response_id"], json!("resp-continuation"));
        assert!(steered.to_string().contains("queued steer"));
        send_final(&mut socket, "resp-steered").await?;

        if !server_workspace.join("release-tool").exists() {
            return Err(eyre!("tool release marker disappeared"));
        }
        Ok(())
    });

    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let turn = agent.prompt("run a tool and continue").await?;
    timeout(std::time::Duration::from_secs(5), async {
        while !workspace.join("tool-started").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| eyre!("tool process did not start"))?;
    turn.steer("queued steer").await?;
    std::fs::write(workspace.join("release-tool"), [])?;

    assert_eq!(turn.result().await?.final_message(), "done");
    drop(agent);
    drop(events);
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}
