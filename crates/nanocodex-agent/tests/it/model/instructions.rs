use super::*;

#[tokio::test]
async fn model_prompt_selection_preserves_explicit_and_additional_instructions() -> Result<()> {
    let astra = include_str!("../../../../nanocodex-oai-api/prompts/astra.md");
    let legacy = include_str!("../../../../nanocodex-oai-api/prompts/system.md");
    for (initial, selected, replacement, additional, expected) in [
        (Model::Astra, Model::Astra, None, None, astra.to_owned()),
        (
            Model::Sol,
            Model::Astra,
            None,
            Some("host instructions"),
            format!("{astra}\n\nhost instructions"),
        ),
        (
            Model::Astra,
            Model::Sol,
            None,
            Some("host instructions"),
            format!("{legacy}\n\nhost instructions"),
        ),
        (Model::Terra, Model::Terra, None, None, legacy.to_owned()),
        (Model::Luna, Model::Luna, None, None, legacy.to_owned()),
        (
            Model::Sol,
            Model::Astra,
            Some("caller replacement"),
            Some("host instructions"),
            "caller replacement\n\nhost instructions".to_owned(),
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("ws://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut socket = accept_async(stream).await?;
            let generation = next_json(&mut socket).await?;
            assert_eq!(generation["model"], selected.as_str());
            assert_eq!(generation["input"][1]["role"], "developer");
            assert_eq!(generation["input"][1]["content"][0]["text"], expected);
            send_final(&mut socket, "resp-prompt").await
        });
        let openai = OpenAi::builder("test-key")
            .model(initial)
            .websocket_warmup(false)
            .websocket_url(endpoint)
            .build()?;
        let mut builder = Nanocodex::builder(openai).thinking(Thinking::Low);
        if let Some(replacement) = replacement {
            builder = builder.instructions(replacement);
        }
        if let Some(additional) = additional {
            builder = builder.additional_instructions(additional);
        }
        let (agent, events) = builder.build()?;
        agent.set_model(selected).await?;
        assert_eq!(
            agent
                .prompt("first turn")
                .await?
                .result()
                .await?
                .final_message(),
            "done"
        );
        agent.shutdown().await?;
        drop((agent, events));
        timeout(std::time::Duration::from_secs(5), server).await???;
    }
    Ok(())
}

#[tokio::test]
async fn astra_prompt_is_restored_from_the_retained_model() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let expected = format!(
        "{}\n\nhost instructions",
        include_str!("../../../../nanocodex-oai-api/prompts/astra.md")
    );
    let server = tokio::spawn(async move {
        let mut prefix = None;
        for response_id in ["resp-first", "resp-resumed"] {
            let (stream, _) = listener.accept().await?;
            let mut socket = accept_async(stream).await?;
            let generation = next_json(&mut socket).await?;
            assert_eq!(generation["model"], "gpt-6-astra");
            let instructions = &generation["input"][1];
            assert_eq!(instructions["content"][0]["text"], expected);
            if let Some(prefix) = &prefix {
                assert_eq!(instructions, prefix);
                assert!(generation["input"].to_string().contains("first turn"));
            }
            prefix = Some(instructions.clone());
            send_final(&mut socket, response_id).await?;
        }
        Result::<()>::Ok(())
    });
    let openai = OpenAi::builder("test-key")
        .websocket_warmup(false)
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai.clone())
        .model(Model::Astra)
        .thinking(Thinking::Low)
        .additional_instructions("host instructions")
        .build()?;
    let first = agent.prompt("first turn").await?.result().await?;
    let snapshot: SessionSnapshot =
        serde_json::from_value(serde_json::to_value(first.snapshot().unwrap())?)?;
    agent.shutdown().await?;
    drop((agent, events, first));
    let (resumed, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .additional_instructions("host instructions")
        .resume(snapshot)
        .build()?;
    assert_eq!(
        resumed
            .prompt("second turn")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    resumed.shutdown().await?;
    drop((resumed, events));
    timeout(std::time::Duration::from_secs(5), server).await???;
    Ok(())
}

#[tokio::test]
async fn rollout_home_supplies_global_instructions() -> Result<()> {
    let workspace = temporary_workspace("rollout-instructions-workspace")?;
    let rollout_home = temporary_workspace("rollout-instructions-home")?;
    std::fs::write(
        rollout_home.join("AGENTS.md"),
        "Use the rollout home instructions.",
    )?;

    run_global_instructions_case(
        &workspace,
        RolloutConfig::new(&rollout_home),
        None,
        "Use the rollout home instructions.",
        None,
    )
    .await?;

    std::fs::remove_dir_all(workspace)?;
    std::fs::remove_dir_all(rollout_home)?;
    Ok(())
}

#[tokio::test]
async fn explicit_codex_home_takes_precedence_over_rollout_home() -> Result<()> {
    let workspace = temporary_workspace("explicit-instructions-workspace")?;
    let rollout_home = temporary_workspace("explicit-instructions-rollout")?;
    let codex_home = temporary_workspace("explicit-instructions-home")?;
    std::fs::write(
        rollout_home.join("AGENTS.md"),
        "Do not use the rollout home instructions.",
    )?;
    std::fs::write(
        codex_home.join("AGENTS.md"),
        "Use the explicit Codex home instructions.",
    )?;

    run_global_instructions_case(
        &workspace,
        RolloutConfig::new(&rollout_home),
        Some(&codex_home),
        "Use the explicit Codex home instructions.",
        Some("Do not use the rollout home instructions."),
    )
    .await?;

    std::fs::remove_dir_all(workspace)?;
    std::fs::remove_dir_all(rollout_home)?;
    std::fs::remove_dir_all(codex_home)?;
    Ok(())
}

async fn run_global_instructions_case(
    workspace: &Path,
    rollout: RolloutConfig,
    codex_home: Option<&Path>,
    expected: &'static str,
    unexpected: Option<&'static str>,
) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        let input = generation["input"].to_string();
        assert!(input.contains(expected), "{input}");
        if let Some(unexpected) = unexpected {
            assert!(!input.contains(unexpected), "{input}");
        }
        send_final(&mut socket, "resp-final").await
    });

    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let mut builder = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(workspace)
        .session_id(test_session_id());
    if let Some(codex_home) = codex_home {
        builder = builder.codex_home(codex_home);
    }
    let (agent, events) = builder.rollout(rollout).build()?;
    assert_eq!(
        agent
            .prompt("follow the applicable instructions")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );

    agent.flush_rollout().await?;
    drop((agent, events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    Ok(())
}
