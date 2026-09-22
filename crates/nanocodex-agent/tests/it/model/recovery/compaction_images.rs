use super::*;

#[tokio::test]
async fn automatic_compaction_image_failure_repairs_durable_checkpoint() -> Result<()> {
    rejected_compaction_image(false).await
}

#[tokio::test]
async fn manual_compaction_image_failure_repairs_durable_checkpoint() -> Result<()> {
    rejected_compaction_image(true).await
}

async fn rejected_compaction_image(manual: bool) -> Result<()> {
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
                "resp-image",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-image",
                    "name": "exec",
                    "input": "image(\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=\", \"original\");"
                })],
                if manual { 12 } else { 400_000 },
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        if !manual {
            assert!(continuation.to_string().contains("compaction_trigger"));
        }
        let output = continuation["input"][0]["output"]
            .as_array()
            .ok_or_else(|| eyre!("image tool output was not content"))?;
        let image = output
            .iter()
            .find(|item| item["type"] == "input_image")
            .ok_or_else(|| eyre!("prepared image was missing"))?;
        assert!(
            image["image_url"]
                .as_str()
                .is_some_and(|url| url.starts_with("data:image/png;base64,"))
        );
        assert!(image.get("detail").is_none());

        if manual {
            send_final(&mut socket, "resp-before-compact").await?;
            let compact = next_json(&mut socket).await?;
            assert!(compact.to_string().contains("compaction_trigger"));
        }
        send_json(
            &mut socket,
            json!({
                "type": "error",
                "status": 400,
                "error": {
                    "type": "invalid_request_error",
                    "code": "invalid_value",
                    "message": "Invalid 'input[175].output[1].image_url'. Expected a base64-encoded data URL with an image MIME type, but got an invalid base64-encoded value.",
                    "param": "input[175].output[1].image_url"
                }
            }),
        )
        .await?;

        // The original turn fails; its durable follow-up must replay repaired history.
        let next = timeout(std::time::Duration::from_secs(5), socket.next()).await?;
        assert!(!matches!(next, Some(Ok(Message::Text(_)))));
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let replay = next_json(&mut socket).await?;
        assert!(replay.get("previous_response_id").is_none());
        let encoded = replay.to_string();
        assert!(encoded.contains("inspect images"));
        assert!(encoded.contains("continue after rejected image"));
        let output = replay["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| {
                item["type"] == "custom_tool_call_output" && item["call_id"] == "call-image"
            })
            .expect("tool output must survive the failure checkpoint");
        let encoded_output = output.to_string();
        assert!(!encoded_output.contains("input_image"));
        assert!(!encoded_output.contains("data:image/"));
        assert!(encoded_output.contains("provider rejected its data"));
        send_final(&mut socket, "resp-final").await?;
        let later_compact = next_json(&mut socket).await?;
        assert!(later_compact.to_string().contains("compaction_trigger"));
        send_json(
            &mut socket,
            json!({
                "type": "response.output_item.done",
                "item": { "type": "compaction", "encrypted_content": "later summary" }
            }),
        )
        .await?;
        send_json(&mut socket, completed_response("resp-later-compact", &[])).await
    });

    let workspace = tempfile::tempdir()?;
    let rollout_home = tempfile::tempdir()?;
    let openai = || OpenAi::builder("test-key").websocket_url(&endpoint).build();
    let (agent, events) = Nanocodex::builder(openai()?)
        .model(Model::Astra)
        .thinking(Thinking::Low)
        .context_window_tokens(400_000)
        .workspace(workspace.path())
        .session_id(test_session_id())
        .rollout(RolloutConfig::new(rollout_home.path()))
        .build()?;
    drop(events);
    let turn = agent.prompt("inspect images").await?;
    let error = if manual {
        turn.await?;
        agent
            .compact()
            .await
            .expect_err("manual compaction must reject image")
    } else {
        turn.await
            .expect_err("automatic compaction must reject image")
    };
    assert!(error.requires_image_repair());
    assert!(matches!(
        error,
        NanocodexError::CompactionFailed {
            requires_session_stop: false,
            ..
        }
    ));
    agent.shutdown().await?;
    drop(agent);

    let durable = RolloutConfig::new(rollout_home.path()).load_session(TEST_SESSION_ID)?;
    let snapshot = serde_json::to_value(durable.snapshot())?;
    assert!(
        snapshot["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "custom_tool_call" && item["call_id"] == "call-image")
    );
    let output = snapshot["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "custom_tool_call_output" && item["call_id"] == "call-image")
        .expect("failed turn must persist the tool output");
    let encoded_output = output.to_string();
    assert!(!encoded_output.contains("input_image"));
    assert!(!encoded_output.contains("data:image/"));
    assert!(encoded_output.contains("provider rejected its data"));

    let (thread_id, snapshot, rollout) = durable.into_parts();
    let (agent, events) = Nanocodex::builder(openai()?)
        .thinking(Thinking::Low)
        .session_id(thread_id.parse()?)
        .resume(snapshot)
        .rollout(rollout)
        .build()?;
    drop(events);
    assert_eq!(
        agent
            .prompt("continue after rejected image")
            .await?
            .await?
            .final_message(),
        "done"
    );
    agent.compact().await?;
    agent.shutdown().await?;
    drop(agent);
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    Ok(())
}
