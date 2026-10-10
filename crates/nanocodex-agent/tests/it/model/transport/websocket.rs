use super::*;

#[tokio::test]
async fn websocket_ephemeral_fork_replays_history_on_its_fresh_socket() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        let warmup = next_json(&mut root).await?;
        assert_eq!(warmup["store"], false);
        assert_eq!(warmup["generate"], false);
        send_warmup(&mut root, "resp-warmup").await?;

        let first = next_json(&mut root).await?;
        assert_eq!(first["store"], false);
        assert_eq!(first["previous_response_id"], "resp-warmup");
        send_final(&mut root, "resp-first").await?;

        let second = next_json(&mut root).await?;
        assert_eq!(second["previous_response_id"], "resp-first");
        assert_eq!(second["input"].as_array().map(Vec::len), Some(1));
        send_final(&mut root, "resp-second").await?;

        let (stream, _) = listener.accept().await?;
        let mut branch = accept_async(stream).await?;
        let request = next_json(&mut branch).await?;
        assert_eq!(request["store"], false);
        assert!(request.get("previous_response_id").is_none());
        let request_text = request.to_string();
        assert!(request_text.contains("first prompt"));
        assert!(request_text.contains("branch prompt"));
        assert!(!request_text.contains("second prompt"));
        send_final(&mut branch, "resp-branch").await
    });

    let workspace = temporary_workspace("websocket-ephemeral-fork")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .store(false)
        .build()?;
    let (agent, root_events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let first = agent.prompt("first prompt").await?.result().await?;
    assert_eq!(
        agent
            .prompt("second prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    let (fork, fork_events) = agent.fork_from(&first).await?;
    assert_eq!(
        fork.prompt("branch prompt")
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
async fn websocket_full_replay_never_sends_a_previous_response_id() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let warmup = next_json(&mut socket).await?;
        send_warmup(&mut socket, "resp-warmup").await?;

        let first = next_json(&mut socket).await?;
        assert!(first.get("previous_response_id").is_none());
        assert!(first.to_string().contains("first prompt"));
        send_final(&mut socket, "resp-first").await?;

        let second = next_json(&mut socket).await?;
        assert!(second.get("previous_response_id").is_none());
        let replay = second.to_string();
        assert!(replay.contains("first prompt"));
        assert!(replay.contains("second prompt"));
        send_final(&mut socket, "resp-second").await?;
        drop(warmup);
        Result::<()>::Ok(())
    });

    let workspace = temporary_workspace("websocket-full-replay")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .store(false)
        .history(ResponsesHistory::FullReplay)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    agent.prompt("first prompt").await?.result().await?;
    agent.prompt("second prompt").await?.result().await?;
    drop((agent, events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn model_is_fixed_at_creation_while_runtime_reasoning_policy_can_change() -> Result<()> {
    // Compare the complete request envelope, allowing only the transport history
    // cursor, input, per-turn metadata, and exercised policy fields to change.
    fn stable_envelope(request: &Value) -> Value {
        let mut envelope = request.clone();
        let fields = envelope.as_object_mut().expect("request object");
        fields.remove("input");
        fields.remove("previous_response_id");
        fields.remove("service_tier");
        // Like codex-rs, compare inference settings independently of metadata
        // that identifies each turn and is expected to change between prompts.
        fields.remove("client_metadata");
        fields
            .get_mut("reasoning")
            .and_then(Value::as_object_mut)
            .expect("reasoning controls")
            .remove("effort");
        envelope
    }

    fn assistant_output(response_id: &str) -> Value {
        json!({
            "type": "message",
            "id": format!("msg_{response_id}"),
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "done" }]
        })
    }

    fn wire_summary(label: &str, request: &Value, retained_items: usize) {
        eprintln!(
            "policy-wire {label}: model={} effort={} tier={} previous={} input_items={} retained_items={} cache={} instructions=unchanged configuration_updates=0",
            request["model"].as_str().unwrap(),
            request["reasoning"]["effort"].as_str().unwrap(),
            request["service_tier"].as_str().unwrap_or("absent"),
            request["previous_response_id"].as_str().unwrap_or("absent"),
            request["input"].as_array().unwrap().len(),
            retained_items,
            request["prompt_cache_key"].as_str().unwrap(),
        );
    }

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    // Keep fallback local even if a server assertion closes the WebSocket.
    let api_base_url = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let warmup = next_json(&mut socket).await?;
        assert_warmup(&warmup);
        assert_eq!(warmup["model"], "gpt-6-luna");
        assert_eq!(warmup["reasoning"]["effort"], "low");
        assert_eq!(
            crate::model::instructions::caller_instructions(&warmup),
            "custom prompt"
        );
        send_warmup(&mut socket, "resp-warmup").await?;

        let first = next_json(&mut socket).await?;
        assert_eq!(first["model"], "gpt-6-luna");
        assert_eq!(first["previous_response_id"], "resp-warmup");
        assert_eq!(first["reasoning"]["effort"], "low");
        assert!(first.get("service_tier").is_none());
        assert_eq!(first["prompt_cache_key"], warmup["prompt_cache_key"]);
        let envelope = stable_envelope(&first);
        // Reconstruct the model-visible transcript from actual wire traffic,
        // including an executed tool and the provider's assistant response.
        let mut history = warmup["input"].as_array().unwrap().clone();
        history.extend(first["input"].as_array().unwrap().iter().cloned());
        let tool_call = json!({
            "type": "custom_tool_call",
            "id": "ctc_policy_history",
            "call_id": "call-policy-history",
            "name": "exec",
            "input": "text(\"retained tool evidence\")"
        });
        send_json(
            &mut socket,
            completed_response("resp-first-tool", std::slice::from_ref(&tool_call)),
        )
        .await?;
        history.push(tool_call);
        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-first-tool");
        assert_eq!(continuation["reasoning"]["effort"], "low");
        assert!(continuation.get("service_tier").is_none());
        assert_eq!(
            stable_envelope(&continuation),
            envelope,
            "tool continuation"
        );
        let tool_output = continuation["input"].as_array().unwrap();
        assert_eq!(tool_output.len(), 1);
        assert_eq!(tool_output[0]["type"], "custom_tool_call_output");
        assert_eq!(tool_output[0]["call_id"], "call-policy-history");
        assert!(
            tool_output[0]["output"]
                .to_string()
                .contains("retained tool evidence")
        );
        history.extend(tool_output.iter().cloned());
        // Provider IDs must survive every replay unchanged. Supplying them in
        // the fixture avoids mistaking assigned client IDs for history changes.
        let assistant = assistant_output("resp-first");
        send_json(
            &mut socket,
            completed_response("resp-first", std::slice::from_ref(&assistant)),
        )
        .await?;
        history.push(assistant);
        wire_summary("initial", &first, warmup["input"].as_array().unwrap().len());

        // Exercise the ordinary default path: effort is a request control, not
        // an injected configuration_update or a replacement instruction.
        for (label, prompt, effort, fast, response_id) in [
            ("effort-only", "second prompt", "high", false, "resp-second"),
            ("fast-only", "third prompt", "high", true, "resp-third"),
            ("fast-off", "fourth prompt", "high", false, "resp-fourth"),
            ("effort-back", "fifth prompt", "low", false, "resp-fifth"),
        ] {
            let request = next_json(&mut socket).await?;
            assert!(request.get("previous_response_id").is_none(), "{label}");
            assert_eq!(request["reasoning"]["effort"], effort, "{label}");
            if fast {
                assert_eq!(request["service_tier"], "priority", "{label}");
            } else {
                assert!(request.get("service_tier").is_none(), "{label}");
            }
            assert_eq!(
                stable_envelope(&request),
                envelope,
                "{label}: request envelope changed"
            );
            let input = request["input"].as_array().unwrap();
            assert_eq!(
                input.len(),
                history.len() + 1,
                "{label}: unexpected injected or missing items"
            );
            assert_eq!(
                input[..history.len()],
                history,
                "{label}: replay changed retained instructions/history"
            );
            assert!(
                input
                    .iter()
                    .all(|item| item["type"] != "configuration_update"),
                "{label}"
            );
            let mut user = input.last().unwrap().clone();
            remove_client_item_id(&mut user, "msg");
            assert_eq!(
                user,
                json!({
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": prompt }]
                }),
                "{label}: only the new user prompt may be appended"
            );
            wire_summary(label, &request, history.len());
            history = input.clone();
            let assistant = assistant_output(response_id);
            send_json(
                &mut socket,
                completed_response(response_id, std::slice::from_ref(&assistant)),
            )
            .await?;
            history.push(assistant);
        }

        let unchanged = next_json(&mut socket).await?;
        assert_eq!(unchanged["previous_response_id"], "resp-fifth");
        assert_eq!(unchanged["reasoning"]["effort"], "low");
        assert!(unchanged.get("service_tier").is_none());
        assert_eq!(stable_envelope(&unchanged), envelope, "unchanged policy");
        let input = unchanged["input"].as_array().unwrap();
        assert_eq!(
            input.len(),
            1,
            "unchanged policy should resume delta transport"
        );
        let mut user = input[0].clone();
        remove_client_item_id(&mut user, "msg");
        assert_eq!(
            user,
            json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "sixth prompt" }]
            })
        );
        wire_summary("unchanged", &unchanged, history.len());
        send_json(
            &mut socket,
            completed_response("resp-sixth", &[assistant_output("resp-sixth")]),
        )
        .await
    });

    let workspace = temporary_workspace("follow-on")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .api_base_url(api_base_url)
        .max_attempts(NonZeroU32::MIN)
        .model(Model::Sol)
        .thinking(Thinking::Medium)
        .fast_mode(true)
        .reasoning_mode(ReasoningMode::Pro)
        .build()?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .instructions("custom prompt")
        .model(Model::Luna)
        .thinking(Thinking::Low)
        .fast_mode(false)
        .reasoning_mode(ReasoningMode::Standard)
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
    assert_eq!(
        agent
            .prompt("second prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    agent.set_fast_mode(true).await?;
    assert_eq!(
        agent
            .prompt("third prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    agent.set_fast_mode(false).await?;
    assert_eq!(
        agent
            .prompt("fourth prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    agent.set_thinking(Thinking::Low).await?;
    assert_eq!(
        agent
            .prompt("fifth prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    assert_eq!(
        agent
            .prompt("sixth prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    drop(agent);

    let mut completed = Vec::new();
    while let Some(event) = events.recv().await {
        if event.kind == AgentEventKind::RunCompleted {
            completed.push(event.decode_payload::<Value>()?);
        }
    }
    assert_eq!(completed.len(), 6);
    for (index, effort) in ["low", "high", "high", "high", "low", "low"]
        .iter()
        .enumerate()
    {
        assert_eq!(
            completed[index]["connection_attempts"],
            usize::from(index == 0)
        );
        assert_eq!(
            completed[index]["response_attempts"],
            if index == 0 { 3 } else { 1 }
        );
        assert_eq!(completed[index]["effort"], *effort);
        assert_eq!(completed[index]["model"], "gpt-6-luna");
    }

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn queued_prompts_retain_tier_and_effort_captured_when_accepted() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (first_started, first_started_rx) = tokio::sync::oneshot::channel();
    let (release_first, release_first_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let warmup = next_json(&mut socket).await?;
        assert_eq!(warmup["model"], "gpt-6-luna");
        assert_eq!(warmup["reasoning"]["effort"], "low");
        send_warmup(&mut socket, "resp-warmup").await?;

        let first = next_json(&mut socket).await?;
        assert_eq!(first["model"], "gpt-6-luna");
        assert_eq!(first["reasoning"]["effort"], "low");
        first_started
            .send(())
            .map_err(|()| eyre!("first request signal receiver dropped"))?;
        release_first_rx
            .await
            .map_err(|_| eyre!("first request release sender dropped"))?;
        send_json(
            &mut socket,
            completed_response(
                "resp-first-tool",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "text(\"continued\")"
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["model"], "gpt-6-luna");
        assert_eq!(continuation["previous_response_id"], "resp-first-tool");
        assert_eq!(continuation["reasoning"]["effort"], "low");
        assert!(continuation.get("service_tier").is_none());
        send_final(&mut socket, "resp-first").await?;

        let queued = next_json(&mut socket).await?;
        assert_eq!(queued["model"], "gpt-6-luna");
        assert_eq!(queued["previous_response_id"], "resp-first");
        assert_eq!(queued["reasoning"]["effort"], "low");
        assert!(queued.get("service_tier").is_none());
        eprintln!(
            "policy-wire queued: continuation=low/absent queued=low/absent previous=resp-first; accepted policy retained after setters"
        );
        send_final(&mut socket, "resp-queued").await?;

        let updated = next_json(&mut socket).await?;
        assert_eq!(updated["model"], "gpt-6-luna");
        assert!(updated.get("previous_response_id").is_none());
        assert_eq!(updated["reasoning"]["effort"], "high");
        assert_eq!(updated["service_tier"], "priority");
        let replay = updated.to_string();
        assert!(replay.contains("first prompt"));
        assert!(replay.contains("queued prompt"));
        assert!(replay.contains("updated prompt"));
        eprintln!(
            "policy-wire updated: effort=high tier=priority previous=absent; queued and first prompts replayed"
        );
        send_final(&mut socket, "resp-updated").await?;

        let compact = next_json(&mut socket).await?;
        assert_eq!(compact["input"], json!([{"type": "compaction_trigger"}]));
        assert_eq!(compact["service_tier"], "priority");
        send_json(
            &mut socket,
            json!({
                "type": "response.output_item.done",
                "item": { "id": "cmp-server-id", "type": "compaction", "encrypted_content": "summary" }
            }),
        )
        .await?;
        send_json(
            &mut socket,
            completed_response_with_usage("resp-compact", &[], 120),
        )
        .await
    });

    let workspace = temporary_workspace("queued-turn-policy")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .model(Model::Luna)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;

    let first = agent.prompt("first prompt").await?;
    first_started_rx
        .await
        .map_err(|_| eyre!("first request was not observed"))?;
    let queued = agent.prompt("queued prompt").await?;
    agent.set_thinking(Thinking::High).await?;
    agent.set_service_tier(ServiceTier::Ultrafast).await?;
    release_first
        .send(())
        .map_err(|()| eyre!("first request release receiver dropped"))?;
    first.result().await?;
    let queued = queued.result().await?;
    assert_eq!(
        serde_json::to_value(
            queued
                .snapshot()
                .expect("local turns always retain a snapshot"),
        )?["model"],
        "gpt-6-luna"
    );
    assert_eq!(estimated_tier(&queued)?, ServiceTier::Standard);
    let updated = agent.prompt("updated prompt").await?.result().await?;
    assert_eq!(
        serde_json::to_value(
            updated
                .snapshot()
                .expect("local turns always retain a snapshot"),
        )?["model"],
        "gpt-6-luna"
    );
    assert_eq!(estimated_tier(&updated)?, ServiceTier::Fast);
    agent.compact().await?;

    let ChildSnapshot::Codex(runtime) = agent.runtime_snapshot().await? else {
        return Err(eyre!("native agent returned a different checkpoint family"));
    };
    assert_eq!(runtime.service_tier, ServiceTier::Ultrafast);
    let mut encoded = serde_json::to_value(&runtime)?;
    assert_eq!(encoded["service_tier"], "ultrafast");
    let fields = encoded
        .as_object_mut()
        .ok_or_else(|| eyre!("runtime snapshot was not an object"))?;
    fields.remove("service_tier");
    fields.insert("fast_mode".into(), json!(true));
    let legacy: ChildRuntimeSnapshot = serde_json::from_value(encoded.clone())?;
    assert_eq!(legacy.service_tier, ServiceTier::Fast);
    encoded["service_tier"] = json!("ultrafast");
    assert!(
        serde_json::from_value::<ChildRuntimeSnapshot>(encoded).is_err(),
        "conflicting tier fields must be rejected"
    );

    drop((agent, events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn supported_reasoning_updates_preserve_socket_prefix_and_replay_after_fast_or_reconnect()
-> Result<()> {
    for model in [Model::Astra, Model::Sol] {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut socket = accept_async(stream).await?;
            let warmup = next_json(&mut socket).await?;
            assert_warmup(&warmup);
            assert_eq!(warmup["reasoning"]["effort"], "medium");
            assert_eq!(
                crate::model::instructions::caller_instructions(&warmup),
                "Keep this developer prompt byte-for-byte stable."
            );
            send_warmup(&mut socket, "resp-policy-warmup").await?;
            let mut history = warmup["input"].as_array().unwrap().clone();
            let mut envelope = None;
            for (index, (effort, fast)) in [
                ("medium", false),
                ("high", false),
                ("high", false),
                ("low", false),
                ("low", true),
                ("low", false),
                ("low", false),
            ]
            .into_iter()
            .enumerate()
            {
                if index == 6 {
                    socket.send(Message::Close(None)).await?;
                    drop(socket);
                    let (stream, _) = listener.accept().await?;
                    socket = accept_async(stream).await?;
                }
                let request = next_json(&mut socket).await?;
                assert_eq!(request["model"], model.as_str());
                assert_eq!(request["reasoning"]["effort"], "medium");
                assert_eq!(request["prompt_cache_key"], warmup["prompt_cache_key"]);
                if fast {
                    assert_eq!(request["service_tier"], "priority");
                } else {
                    assert!(request.get("service_tier").is_none());
                }
                let mut stable = request.clone();
                for field in [
                    "input",
                    "previous_response_id",
                    "service_tier",
                    "client_metadata",
                ] {
                    stable.as_object_mut().unwrap().remove(field);
                }
                if let Some(expected) = &envelope {
                    assert_eq!(&stable, expected, "turn {index}: stable request envelope");
                } else {
                    envelope = Some(stable);
                }
                let input = request["input"].as_array().unwrap();
                let full_replay = index >= 4;
                if full_replay {
                    assert!(request.get("previous_response_id").is_none());
                    assert_eq!(
                        &input[..history.len()],
                        &history,
                        "turn {index}: retained history including IDs"
                    );
                } else {
                    let previous = if index == 0 {
                        "resp-policy-warmup".to_owned()
                    } else {
                        format!("resp-policy-{}", index - 1)
                    };
                    assert_eq!(request["previous_response_id"], previous);
                }
                let start = if full_replay { history.len() } else { 0 };
                let tail = &input[start..];
                let changed = matches!(index, 1 | 3);
                // The first prompt also sends the cached local context items.
                let user_index = if index == 0 { tail.len() - 1 } else { 0 };
                assert_eq!(tail.len(), user_index + 1 + usize::from(changed));
                let mut user = tail[user_index].clone();
                remove_client_item_id(&mut user, "msg");
                assert_eq!(
                    user,
                    json!({
                        "type": "message", "role": "user",
                        "content": [{"type": "input_text", "text": format!("policy prompt {index}")}]
                    })
                );
                if changed {
                    assert_eq!(
                        tail.last().unwrap(),
                        &json!({
                            "type": "configuration_update", "reasoning": {"effort": effort}
                        })
                    );
                }
                assert_eq!(
                    tail.iter()
                        .filter(|item| item["type"] == "configuration_update")
                        .count(),
                    usize::from(changed)
                );
                if full_replay {
                    history = input.clone();
                } else {
                    history.extend(input.iter().cloned());
                }
                if index == 0 {
                    let call = json!({
                        "id": "ctc_supported_policy", "type": "custom_tool_call",
                        "call_id": "call-supported-policy", "name": "exec",
                        "input": "text(\"retained supported tool evidence\")"
                    });
                    send_json(
                        &mut socket,
                        completed_response("resp-policy-tool", std::slice::from_ref(&call)),
                    )
                    .await?;
                    history.push(call);
                    let continuation = next_json(&mut socket).await?;
                    assert_eq!(continuation["previous_response_id"], "resp-policy-tool");
                    assert_eq!(continuation["reasoning"]["effort"], "medium");
                    assert_eq!(continuation["prompt_cache_key"], warmup["prompt_cache_key"]);
                    let output = continuation["input"].as_array().unwrap();
                    assert_eq!(
                        output.len(),
                        1,
                        "tool continuation must not repeat the effort update"
                    );
                    assert_eq!(output[0]["type"], "custom_tool_call_output");
                    assert_eq!(output[0]["call_id"], "call-supported-policy");
                    assert!(
                        output[0]["output"]
                            .to_string()
                            .contains("retained supported tool evidence")
                    );
                    history.extend(output.iter().cloned());
                }
                let assistant = json!({
                    "id": format!("msg_policy_{index}"), "type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "done"}]
                });
                send_json(
                    &mut socket,
                    completed_response(
                        &format!("resp-policy-{index}"),
                        std::slice::from_ref(&assistant),
                    ),
                )
                .await?;
                history.push(assistant);
                eprintln!(
                    "supported-policy-wire model={} turn={index} selected={effort} pinned=medium fast={fast} replay={full_replay} update={changed}",
                    model.as_str()
                );
            }
            Result::<_>::Ok(history)
        });
        let workspace = temporary_workspace("supported-policy-wire")?;
        let openai = OpenAi::builder("test-key")
            .model(model)
            .websocket_url(format!("ws://{address}"))
            .api_base_url(format!("http://{address}"))
            .max_attempts(NonZeroU32::new(2).unwrap())
            .build()?;
        let (agent, events) = Nanocodex::builder(openai)
            .thinking(Thinking::Medium)
            .fast_mode(false)
            .instructions("Keep this developer prompt byte-for-byte stable.")
            .workspace(&workspace)
            .session_id(test_session_id())
            .build()?;
        for (index, (effort, fast)) in [
            (Thinking::Medium, false),
            (Thinking::High, false),
            (Thinking::High, false),
            (Thinking::Low, false),
            (Thinking::Low, true),
            (Thinking::Low, false),
            (Thinking::Low, false),
        ]
        .into_iter()
        .enumerate()
        {
            agent.set_thinking(effort).await?;
            agent.set_fast_mode(fast).await?;
            assert_eq!(
                agent
                    .prompt(format!("policy prompt {index}"))
                    .await?
                    .result()
                    .await?
                    .final_message(),
                "done"
            );
        }
        let snapshot = serde_json::to_value(agent.snapshot().await?)?;
        agent.shutdown().await?;
        drop((agent, events));
        let history = timeout(std::time::Duration::from_secs(5), server)
            .await
            .map_err(|_| eyre!("supported policy server did not finish"))???;
        assert_eq!(&history[2..], snapshot["history"].as_array().unwrap());
        std::fs::remove_dir_all(workspace)?;
    }
    Ok(())
}

fn estimated_tier(result: &TurnResult) -> Result<ServiceTier> {
    Ok(result
        .usage()
        .and_then(|usage| usage.estimated_cost())
        .ok_or_else(|| eyre!("reported usage must produce an estimate"))?
        .service_tier())
}
