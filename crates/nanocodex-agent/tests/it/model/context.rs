use super::*;

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
