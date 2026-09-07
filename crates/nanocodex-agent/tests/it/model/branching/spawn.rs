use super::*;

#[tokio::test]
async fn per_agent_tool_factory_binds_recursive_forks_to_the_invoking_driver() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        let warmup = next_json(&mut root).await?;
        assert_eq!(warmup["store"], true);
        let lineage = warmup["prompt_cache_key"].clone();
        let root_session = warmup["client_metadata"]["session_id"].clone();
        let root_thread = warmup["client_metadata"]["thread_id"].clone();
        send_warmup(&mut root, "resp-warmup").await?;
        let root_turn = next_json(&mut root).await?;
        assert_eq!(root_turn["previous_response_id"], "resp-warmup");
        send_final(&mut root, "resp-root").await?;

        let (stream, _) = listener.accept().await?;
        let mut child = accept_async(stream).await?;
        let child_turn = next_json(&mut child).await?;
        let child_session = child_turn["client_metadata"]["session_id"].clone();
        let child_thread = child_turn["client_metadata"]["thread_id"].clone();
        assert_eq!(child_turn["previous_response_id"], "resp-root");
        assert_eq!(child_turn["prompt_cache_key"], lineage);
        assert_eq!(child_session, root_session);
        assert_ne!(child_thread, root_thread);
        send_final(&mut child, "resp-child").await?;

        let (stream, _) = listener.accept().await?;
        let mut grandchild = accept_async(stream).await?;
        let grandchild_turn = next_json(&mut grandchild).await?;
        assert_eq!(grandchild_turn["previous_response_id"], "resp-child");
        assert_eq!(grandchild_turn["prompt_cache_key"], lineage);
        assert_eq!(
            grandchild_turn["client_metadata"]["session_id"],
            child_session
        );
        assert_ne!(
            grandchild_turn["client_metadata"]["thread_id"],
            child_thread
        );
        send_final(&mut grandchild, "resp-grandchild").await
    });

    let (handles, mut received_handles) = tokio::sync::mpsc::unbounded_channel::<AgentHandle>();
    let workspace = temporary_workspace("recursive-fork-tools")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .store(true)
        .build()?;
    let (root, root_events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools_factory(move |handle| {
            drop(handles.send(handle));
            Tools::builder().without_defaults().build()
        })
        .build()?;
    let root_handle = received_handles
        .recv()
        .await
        .ok_or_else(|| eyre!("root tool factory did not receive a fork handle"))?;

    root.prompt(Prompt::new("root turn"))
        .await?
        .result()
        .await?;
    let (child, child_events) = root_handle.fork().await?;
    let child_handle = received_handles
        .recv()
        .await
        .ok_or_else(|| eyre!("child tool factory did not receive a fork handle"))?;
    child.prompt("child turn").await?.result().await?;
    let (grandchild, grandchild_events) = child_handle.fork().await?;
    received_handles
        .recv()
        .await
        .ok_or_else(|| eyre!("grandchild tool factory did not receive a fork handle"))?;
    grandchild.prompt("grandchild turn").await?.result().await?;

    drop((
        root,
        child,
        grandchild,
        root_events,
        child_events,
        grandchild_events,
    ));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn clean_spawn_reuses_the_root_cache_key_without_history() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        let root_warmup = next_json(&mut root).await?;
        assert_eq!(root_warmup["prompt_cache_key"], TEST_SESSION_ID);
        assert!(
            root_warmup
                .to_string()
                .contains("shared private configuration"),
            "root request omitted the configured system prompt"
        );
        send_warmup(&mut root, "resp-root-warmup").await?;
        let root_turn = next_json(&mut root).await?;
        assert_eq!(root_turn["previous_response_id"], "resp-root-warmup");
        send_final(&mut root, "resp-root").await?;

        let (stream, _) = listener.accept().await?;
        let mut child = accept_async(stream).await?;
        let child_warmup = next_json(&mut child).await?;
        assert_eq!(child_warmup["reasoning"]["effort"], "high");
        let child_session = child_warmup["client_metadata"]["session_id"]
            .as_str()
            .ok_or_else(|| eyre!("clean child warmup omitted its session id"))?;
        assert_eq!(child_session, TEST_SESSION_ID);
        assert_ne!(
            child_warmup["client_metadata"]["thread_id"],
            TEST_SESSION_ID
        );
        assert_eq!(child_warmup["prompt_cache_key"], TEST_SESSION_ID);
        assert!(child_warmup.get("previous_response_id").is_none());
        assert!(
            child_warmup
                .to_string()
                .contains("shared private configuration"),
            "clean child did not reuse the configured system prompt"
        );
        send_warmup(&mut child, "resp-child-warmup").await?;
        let child_turn = next_json(&mut child).await?;
        assert_eq!(child_turn["previous_response_id"], "resp-child-warmup");
        assert_ne!(child_turn["previous_response_id"], "resp-root");
        send_final(&mut child, "resp-child").await
    });

    let (handles, mut received_handles) = tokio::sync::mpsc::unbounded_channel::<AgentHandle>();
    let workspace = temporary_workspace("clean-spawn-tools")?;
    let openai = OpenAi::builder("private-test-key")
        .websocket_url(endpoint)
        .build()?;
    let (root, root_events) = Nanocodex::builder(openai)
        .instructions("shared private configuration")
        .thinking(Thinking::Low)
        .session_id(test_session_id())
        .workspace(&workspace)
        .tools_factory(move |handle| {
            drop(handles.send(handle));
            Tools::builder().without_defaults().build()
        })
        .build()?;
    let root_handle = received_handles
        .recv()
        .await
        .ok_or_else(|| eyre!("root tool factory did not receive an agent handle"))?;
    root.prompt("root turn").await?.result().await?;
    root.set_thinking(Thinking::High).await?;

    let (child, child_events) = root_handle.spawn().await?;
    received_handles
        .recv()
        .await
        .ok_or_else(|| eyre!("clean child tool factory did not receive an agent handle"))?;
    child.prompt("clean child turn").await?.result().await?;

    drop((root, child, root_events, child_events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn clean_batch_spawn_preserves_requested_order() -> Result<()> {
    let (handles, mut received_handles) = tokio::sync::mpsc::unbounded_channel::<AgentHandle>();
    let openai = OpenAi::builder("test-key")
        .websocket_url("ws://127.0.0.1:1")
        .build()?;
    let (root, root_events) = Nanocodex::builder(openai)
        .tools_factory(move |handle| {
            drop(handles.send(handle));
            Tools::builder().without_defaults().build()
        })
        .build()?;
    let root_handle = received_handles
        .recv()
        .await
        .ok_or_else(|| eyre!("root tool factory did not receive an agent handle"))?;

    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed_sessions = Arc::clone(&observed);
    let children = root_handle
        .spawn_many_observed(3, move |session_id| {
            observed_sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(session_id.to_owned());
        })
        .await?;
    let child_session_ids = children
        .iter()
        .map(|(child, _)| child.session_id().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(child_session_ids.len(), 3);
    assert_ne!(child_session_ids[0], child_session_ids[1]);
    assert_ne!(child_session_ids[1], child_session_ids[2]);
    assert_eq!(
        *observed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        child_session_ids
    );
    for _ in 0..3 {
        received_handles
            .recv()
            .await
            .ok_or_else(|| eyre!("child tool factory did not receive an agent handle"))?;
    }

    drop((root, root_events, children));
    Ok(())
}

#[tokio::test]
async fn clean_spawn_can_override_model_and_thinking_without_mutating_parent() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut child = accept_async(stream).await?;
        let child_warmup = next_json(&mut child).await?;
        assert_eq!(child_warmup["model"], Model::Luna.as_str());
        assert_eq!(child_warmup["reasoning"]["effort"], "medium");
        send_warmup(&mut child, "resp-child-warmup").await?;
        let child_turn = next_json(&mut child).await?;
        send_final(&mut child, "resp-child").await?;

        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        let root_warmup = next_json(&mut root).await?;
        assert_eq!(root_warmup["model"], Model::Sol.as_str());
        assert_eq!(root_warmup["reasoning"]["effort"], "low");
        send_warmup(&mut root, "resp-root-warmup").await?;
        let root_turn = next_json(&mut root).await?;
        send_final(&mut root, "resp-root").await?;

        Ok::<_, eyre::Report>((child_turn, root_turn))
    });

    let (handles, mut received_handles) = tokio::sync::mpsc::unbounded_channel::<AgentHandle>();
    let workspace = temporary_workspace("configured-clean-spawn")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (root, root_events) = Nanocodex::builder(openai)
        .model(Model::Sol)
        .thinking(Thinking::Low)
        .session_id(test_session_id())
        .workspace(&workspace)
        .tools_factory(move |handle| {
            drop(handles.send(handle));
            Tools::builder().without_defaults().build()
        })
        .build()?;
    let root_handle = received_handles
        .recv()
        .await
        .ok_or_else(|| eyre!("root tool factory did not receive an agent handle"))?;

    let (child, child_events) = root_handle
        .spawn_with(
            SpawnOptions::new()
                .model(Model::Luna)
                .thinking(Thinking::Medium),
        )
        .await?;
    received_handles
        .recv()
        .await
        .ok_or_else(|| eyre!("clean child tool factory did not receive an agent handle"))?;
    child.prompt("child turn").await?.result().await?;
    root.prompt("root turn").await?.result().await?;

    drop((root, child, root_events, child_events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn cloned_builders_singleflight_one_shared_prefix_warmup() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut first = accept_async(stream).await?;
        let warmup = next_json(&mut first).await?;
        assert_eq!(warmup["prompt_cache_key"], "shared-prefix");
        let first_session = warmup["client_metadata"]["session_id"]
            .as_str()
            .ok_or_else(|| eyre!("first warmup omitted its session id"))?
            .to_owned();
        send_warmup(&mut first, "resp-shared-warmup").await?;
        let first_turn = next_json(&mut first).await?;
        assert_eq!(first_turn["previous_response_id"], "resp-shared-warmup");
        send_final(&mut first, "resp-first").await?;

        let (stream, _) = listener.accept().await?;
        let mut second = accept_async(stream).await?;
        let second_turn = next_json(&mut second).await?;
        assert_eq!(second_turn["prompt_cache_key"], "shared-prefix");
        assert!(second_turn.get("previous_response_id").is_none());
        assert_ne!(second_turn["client_metadata"]["session_id"], first_session);
        assert_eq!(second_turn["input"].as_array().map(Vec::len), Some(5));
        assert!(second_turn.get("generate").is_none());
        send_final(&mut second, "resp-second").await
    });

    let workspace = temporary_workspace("shared-warmup")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let builder = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .prompt_cache_key("shared-prefix")
        .shared_prompt_cache();

    let (first, mut first_events) = builder.clone().build()?;
    let first_session = first.session_id().to_owned();
    first.prompt("first turn").await?.result().await?;
    drop(first);
    let mut first_warmup_source = None;
    while let Some(event) = first_events.recv().await {
        if event.kind == AgentEventKind::ModelWarmupCompleted {
            first_warmup_source = Some(event.decode_payload::<Value>()?["source"].clone());
        }
    }

    let (second, mut second_events) = builder.build()?;
    assert_ne!(second.session_id(), first_session);
    second.prompt("second turn").await?.result().await?;
    drop(second);
    let mut second_warmup_source = None;
    while let Some(event) = second_events.recv().await {
        if event.kind == AgentEventKind::ModelWarmupCompleted {
            let payload = event.decode_payload::<Value>()?;
            assert!(payload.get("response_id").is_none());
            second_warmup_source = Some(payload["source"].clone());
        }
    }

    assert_eq!(first_warmup_source, Some(json!("response")));
    assert_eq!(second_warmup_source, Some(json!("shared_prefix")));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}
