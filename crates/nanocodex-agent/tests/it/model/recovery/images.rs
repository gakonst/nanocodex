use super::*;

#[tokio::test]
async fn invalid_image_value_is_removed_before_durable_followup() -> Result<()> {
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
            completed_response(
                "resp-image",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-image",
                    "name": "exec",
                    "input": "image(\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=\", \"original\");"
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
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
        send_final(&mut socket, "resp-final").await
    });

    let workspace = tempfile::tempdir()?;
    let rollout_home = tempfile::tempdir()?;
    let openai = || OpenAi::builder("test-key").websocket_url(&endpoint).build();
    let (agent, events) = Nanocodex::builder(openai()?)
        .thinking(Thinking::Low)
        .workspace(workspace.path())
        .session_id(test_session_id())
        .rollout(RolloutConfig::new(rollout_home.path()))
        .build()?;
    drop(events);
    let error = agent
        .prompt("inspect images")
        .await?
        .await
        .expect_err("invalid image must fail the original turn");
    assert!(matches!(
        error.responses_error(),
        Some(ResponsesError::InvalidImageRequest { .. })
    ));
    agent.shutdown().await?;
    drop(agent);

    let durable = RolloutConfig::new(rollout_home.path()).load_session(TEST_SESSION_ID)?;
    let snapshot = serde_json::to_value(durable.snapshot())?;
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
    agent.shutdown().await?;
    drop(agent);
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    Ok(())
}

#[tokio::test]
async fn malformed_stored_image_is_removed_before_provider_replay() -> Result<()> {
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
            completed_response(
                "resp-image",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-image",
                    "name": "exec",
                    "input": "image(\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=\", \"original\");"
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
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

        // Seed a completed image history; the test corrupts its persisted payload.
        send_final(&mut socket, "resp-seeded").await?;
        let next = timeout(std::time::Duration::from_secs(5), socket.next()).await?;
        assert!(!matches!(next, Some(Ok(Message::Text(_)))));
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let replay = next_json(&mut socket).await?;
        assert!(replay.get("previous_response_id").is_none());
        let encoded = replay.to_string();
        assert!(encoded.contains("inspect images"));
        assert!(encoded.contains("continue after malformed stored image"));
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
        assert!(encoded_output.contains("malformed base64 image data"));
        send_final(&mut socket, "resp-final").await
    });

    let workspace = tempfile::tempdir()?;
    let rollout_home = tempfile::tempdir()?;
    let openai = || OpenAi::builder("test-key").websocket_url(&endpoint).build();
    let (agent, events) = Nanocodex::builder(openai()?)
        .thinking(Thinking::Low)
        .workspace(workspace.path())
        .session_id(test_session_id())
        .rollout(RolloutConfig::new(rollout_home.path()))
        .build()?;
    drop(events);
    assert_eq!(
        agent.prompt("inspect images").await?.await?.final_message(),
        "done"
    );
    agent.shutdown().await?;
    drop(agent);

    let durable = RolloutConfig::new(rollout_home.path()).load_session(TEST_SESSION_ID)?;
    let (thread_id, snapshot, rollout) = durable.into_parts();
    let mut encoded = serde_json::to_value(snapshot)?;
    let output = encoded["history"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|item| item["type"] == "custom_tool_call_output" && item["call_id"] == "call-image")
        .unwrap();
    let image = output["output"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|part| part["type"] == "input_image")
        .unwrap();
    image["image_url"] = json!("data:image/png;base64,AAAA\n[output truncated]");
    let snapshot: SessionSnapshot = serde_json::from_value(encoded)?;
    let (agent, events) = Nanocodex::builder(openai()?)
        .thinking(Thinking::Low)
        .session_id(thread_id.parse()?)
        .resume(snapshot)
        .rollout(rollout)
        .build()?;
    drop(events);
    assert_eq!(
        agent
            .prompt("continue after malformed stored image")
            .await?
            .await?
            .final_message(),
        "done"
    );
    agent.shutdown().await?;
    drop(agent);
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    Ok(())
}
