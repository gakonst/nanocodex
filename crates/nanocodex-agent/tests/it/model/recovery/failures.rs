use super::*;

#[tokio::test]
async fn empty_completed_response_id_fails_before_the_terminal_event() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let _generation = next_json(&mut socket).await?;
        send_final(&mut socket, "").await
    });

    let workspace = temporary_workspace("empty-response-id")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;

    let error = agent
        .prompt("reject an empty continuation checkpoint")
        .await?
        .result()
        .await
        .expect_err("an empty completed response ID must fail the turn");
    assert!(matches!(
        error,
        NanocodexError::MalformedResponse {
            detail: "completed turn did not have a response ID"
        }
    ));

    let terminal = timeout(std::time::Duration::from_secs(1), async {
        loop {
            let event = events
                .recv()
                .await
                .ok_or_else(|| eyre!("event stream closed before a terminal event"))?;
            if event.kind.is_terminal() {
                return Result::<AgentEventKind>::Ok(event.kind);
            }
        }
    })
    .await
    .map_err(|_| eyre!("turn did not emit a terminal event"))??;
    assert_eq!(terminal, AgentEventKind::RunFailed);
    assert!(
        std::iter::from_fn(|| events.try_recv_timed()).all(|event| !event.event.kind.is_terminal()),
        "the rejected turn emitted more than one terminal event"
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
async fn warmup_failure_falls_back_to_a_full_first_request() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut first = accept_async(stream).await?;
        assert_warmup(&next_json(&mut first).await?);
        send_json(
            &mut first,
            json!({
                "type": "error",
                "error": { "message": "prewarm unavailable" }
            }),
        )
        .await?;
        drop(first);

        let (stream, _) = listener.accept().await?;
        let mut second = accept_async(stream).await?;
        let generation = next_json(&mut second).await?;
        assert!(generation.get("previous_response_id").is_none());
        assert!(generation.get("generate").is_none());
        assert_eq!(generation["input"].as_array().map(Vec::len), Some(5));
        assert_eq!(generation["input"][0]["type"], "additional_tools");
        assert_eq!(generation["input"][1]["role"], "developer");
        assert_eq!(generation["input"][2]["role"], "developer");
        assert_eq!(generation["input"][3]["role"], "user");
        assert_eq!(generation["input"][4]["role"], "user");
        send_final(&mut second, "resp-final").await
    });

    let workspace = temporary_workspace("warmup-fallback")?;
    let output = run_model(&endpoint, &workspace, "exercise warmup fallback").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"model.warmup.failed\""));
    assert!(output.contains("\"purpose\":\"warmup_fallback\""));
    assert!(output.contains("\"connection_attempts\":2"));
    assert!(output.contains("\"websocket_reconnects\":1"));
    assert!(output.contains("\"run.completed\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn failed_turn_forces_the_next_turn_to_replay_its_latest_safe_history() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let failed = next_json(&mut socket).await?;
        assert_eq!(failed["previous_response_id"], "resp-warmup");
        assert!(failed.to_string().contains("failed prompt"));
        send_json(
            &mut socket,
            json!({
                "type": "error",
                "error": {
                    "code": "invalid_request_error",
                    "message": "reject this logical turn"
                }
            }),
        )
        .await?;

        let replay = next_json(&mut socket).await?;
        assert!(replay.get("previous_response_id").is_none());
        let replay = replay.to_string();
        assert!(replay.contains("failed prompt"));
        assert!(replay.contains("follow-on prompt"));
        send_final(&mut socket, "resp-follow-on").await
    });

    let workspace = temporary_workspace("failed-turn-replay")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    drop(events);

    let failed = agent.prompt("failed prompt").await?.await;
    assert!(failed.is_err());
    let completed = agent.prompt("follow-on prompt").await?.await?;
    assert_eq!(completed.final_message(), "done");

    drop(agent);
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn steer_accepted_before_an_initial_failure_survives_in_the_next_replay() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (request_seen, request_seen_rx) = tokio::sync::oneshot::channel();
    let (release_failure, release_failure_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let failed = next_json(&mut socket).await?;
        assert!(failed.to_string().contains("initial failing task"));
        request_seen
            .send(())
            .map_err(|()| eyre!("request observer dropped"))?;
        release_failure_rx
            .await
            .map_err(|_| eyre!("failure release sender dropped"))?;
        send_json(
            &mut socket,
            json!({
                "type": "error",
                "error": {
                    "code": "invalid_request_error",
                    "message": "fail after accepting steer"
                }
            }),
        )
        .await?;

        let replay = next_json(&mut socket).await?;
        assert!(replay.get("previous_response_id").is_none());
        let replay = replay.to_string();
        assert_eq!(
            replay.matches("initial failing task").count(),
            1,
            "{replay}"
        );
        assert_eq!(replay.matches("retained steer").count(), 1, "{replay}");
        assert_eq!(replay.matches("explicit retry task").count(), 1, "{replay}");
        send_final(&mut socket, "resp-retry").await
    });

    let workspace = temporary_workspace("failed-initial-steer")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    drop(events);

    let failed = agent.prompt("initial failing task").await?;
    request_seen_rx
        .await
        .map_err(|_| eyre!("initial request was not observed"))?;
    failed.steer("retained steer").await?;
    release_failure
        .send(())
        .map_err(|()| eyre!("failure release receiver dropped"))?;
    assert!(failed.result().await.is_err());
    assert_eq!(
        agent
            .prompt("explicit retry task")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );

    drop(agent);
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn context_window_error_forces_compaction_before_the_next_prompt() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let failed = next_json(&mut socket).await?;
        assert_eq!(failed["previous_response_id"], "resp-warmup");
        assert!(failed.to_string().contains("first prompt"));
        send_json(
            &mut socket,
            json!({
                "type": "error",
                "error": {
                    "code": "context_length_exceeded",
                    "message": "request exceeds the model context window"
                }
            }),
        )
        .await?;

        let compact = next_json(&mut socket).await?;
        assert!(compact.get("previous_response_id").is_none());
        let compact_text = compact.to_string();
        assert!(compact_text.contains("first prompt"));
        assert!(!compact_text.contains("second prompt"));
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

        let follow_on = next_json(&mut socket).await?;
        assert!(follow_on.get("previous_response_id").is_none());
        let follow_on_text = follow_on.to_string();
        assert!(follow_on_text.contains("opaque-summary"));
        assert!(follow_on_text.contains("second prompt"));
        send_final(&mut socket, "resp-follow-on").await
    });

    let workspace = temporary_workspace("context-error-compaction")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    drop(events);

    let first = agent.prompt("first prompt").await?.result().await;
    assert!(
        first
            .expect_err("the provider context error should fail the first turn")
            .responses_error()
            .is_some_and(ResponsesError::is_context_window_exceeded)
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

    drop(agent);
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn warmup_connection_failure_falls_back_to_a_full_first_request() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (failed_prewarm, _) = listener.accept().await?;
        drop(failed_prewarm);

        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let generation = next_json(&mut socket).await?;
        assert!(generation.get("previous_response_id").is_none());
        assert!(generation.get("generate").is_none());
        assert_eq!(generation["input"].as_array().map(Vec::len), Some(5));
        assert_eq!(generation["input"][0]["type"], "additional_tools");
        assert_eq!(generation["input"][1]["role"], "developer");
        assert_eq!(generation["input"][2]["role"], "developer");
        assert_eq!(generation["input"][3]["role"], "user");
        assert_eq!(generation["input"][4]["role"], "user");
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("warmup-connection-fallback")?;
    let output = run_model(&endpoint, &workspace, "exercise warmup connection fallback").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"model.connection.failed\""));
    assert!(output.contains("\"purpose\":\"warmup_fallback\""));
    assert!(output.contains("\"connection_attempts\":2"));
    assert!(output.contains("\"websocket_reconnects\":1"));
    assert!(output.contains("\"run.completed\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn continues_past_previous_model_call_limit() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        for call_index in 1..=33 {
            let generation = next_json(&mut socket).await?;
            let previous_response_id = if call_index == 1 {
                "resp-warmup".to_owned()
            } else {
                format!("resp-tool-{}", call_index - 1)
            };
            assert_eq!(generation["previous_response_id"], previous_response_id);
            let response_id = format!("resp-tool-{call_index}");
            let call_id = format!("call-exec-{call_index}");
            send_json(
                &mut socket,
                completed_response(
                    &response_id,
                    &[json!({
                        "type": "custom_tool_call",
                        "call_id": call_id,
                        "name": "exec",
                        "input": "text(\"continue\")"
                    })],
                ),
            )
            .await?;
        }

        let final_generation = next_json(&mut socket).await?;
        assert_eq!(final_generation["previous_response_id"], "resp-tool-33");
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("unbounded-turn")?;
    let output = run_model(&endpoint, &workspace, "continue until done").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"model_calls\":34"));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn explicit_end_turn_false_continues_without_a_tool_call() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        let mut response = completed_response(
            "resp-continue",
            &[json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "intermediate" }]
            })],
        );
        response["response"]["end_turn"] = json!(false);
        send_json(&mut socket, response).await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-continue");
        assert_eq!(continuation["input"].as_array().map(Vec::len), Some(0));
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("end-turn-false")?;
    let output = run_model(&endpoint, &workspace, "continue when requested").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"model_calls\":2"));
    assert!(output.contains("\"text\":\"done\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn completed_response_accepts_null_usage() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        let mut response = completed_response(
            "resp-final",
            &[json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "done" }]
            })],
        );
        response["response"]["usage"] = Value::Null;
        send_json(&mut socket, response).await
    });

    let workspace = temporary_workspace("null-usage")?;
    let task = Prompt::new("accept missing usage");
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let turn = agent.prompt(task).await?;
    drop(agent);
    let mut encoded_events = Vec::new();
    let (event_result, turn_result) =
        tokio::join!(events.write_jsonl(&mut encoded_events), turn.result());
    event_result?;
    let result = turn_result?;
    let output = String::from_utf8(encoded_events)?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"model.call.completed\""));
    assert!(output.contains("\"usage\":null"));
    assert!(output.contains("\"run.completed\""));
    let usage = result.usage().expect("local turns always report usage");
    assert!(usage.estimated_cost().is_none());
    assert_eq!(usage.cost_status(), CostStatus::UsageNotReported);
    let terminal: Value = serde_json::from_str(
        output
            .lines()
            .find(|line| line.contains("\"type\":\"run.completed\""))
            .ok_or_else(|| eyre!("missing terminal event"))?,
    )?;
    assert_eq!(
        terminal["payload"]["cost_status"],
        json!("usage_not_reported")
    );
    assert!(terminal["payload"]["estimated_cost"].is_null());
    assert!(!result.response_completions().is_empty());
    assert_eq!(
        terminal["payload"]["response_completions"],
        serde_json::to_value(result.response_completions())?,
        "terminal JSONL must preserve the same provider completions as TurnResult"
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn completed_response_accepts_null_usage_details() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        let mut response = completed_response(
            "resp-final",
            &[json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "done" }]
            })],
        );
        response["response"]["usage"]["input_tokens_details"] = Value::Null;
        response["response"]["usage"]["output_tokens_details"] = Value::Null;
        send_json(&mut socket, response).await
    });

    let workspace = temporary_workspace("null-usage-details")?;
    let output = run_model(&endpoint, &workspace, "accept missing usage details").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"input_tokens_details\":null"));
    assert!(output.contains("\"output_tokens_details\":null"));
    assert!(output.contains("\"cached_input_tokens\":0"));
    assert!(output.contains("\"reasoning_output_tokens\":0"));
    assert!(output.contains("\"run.completed\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}
