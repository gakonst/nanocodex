use super::*;

#[tokio::test]
async fn model_prompt_selection_preserves_explicit_and_additional_instructions() -> Result<()> {
    for (initial, selected, replacement, additional) in [
        (Model::Astra, Model::Astra, None, None),
        (Model::Sol, Model::Astra, None, Some("host instructions")),
        (Model::Astra, Model::Sol, None, Some("host instructions")),
        (Model::Luna, Model::Luna, None, None),
        (
            Model::Sol,
            Model::Astra,
            Some("caller replacement"),
            Some("host instructions"),
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
            let instructions = assert_runtime_model_identity(&generation);
            for sentinel in [replacement, additional].into_iter().flatten() {
                assert!(instructions.contains(sentinel), "missing {sentinel}");
            }
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
        agent
            .set_harness_model(HarnessModel::Codex(selected))
            .await?;
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
    let server = tokio::spawn(async move {
        let mut prefix = None;
        for response_id in ["resp-first", "resp-resumed"] {
            let (stream, _) = listener.accept().await?;
            let mut socket = accept_async(stream).await?;
            let generation = next_json(&mut socket).await?;
            assert_eq!(generation["model"], "gpt-6-astra");
            let instructions = &generation["input"][1];
            assert!(assert_runtime_model_identity(&generation).contains("host instructions"));
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
    let snapshot: SessionSnapshot = serde_json::from_value(serde_json::to_value(
        first.checkpoint().as_ref().map(conversation).unwrap(),
    )?)?;
    agent.shutdown().await?;
    drop((agent, events, first));
    let (resumed, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .additional_instructions("host instructions")
        .resume_native_snapshot(snapshot)
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

// Return the exact caller-owned developer instructions from the second input
// item after validating the runtime identity block appended by #808.
pub(crate) fn caller_instructions(request: &Value) -> &str {
    let instructions = assert_runtime_model_identity(request);
    assert_eq!(request["input"][1]["role"], "developer");
    assert_eq!(request["input"][1]["content"][0]["text"], instructions);
    instructions
        .split_once("\n\n<runtime_model_identity>")
        .expect("runtime identity follows caller instructions")
        .0
}

// Observe the actual serialized provider request, independently of prompt prose.
fn assert_runtime_model_identity(request: &Value) -> &str {
    let instructions = request["input"]
        .as_array()
        .expect("request input")
        .iter()
        .filter(|item| item["role"] == "developer")
        .filter_map(|item| item["content"].as_array())
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .find(|text| text.contains("<runtime_model_identity>"))
        .expect("provider request must include runtime model identity");
    assert_eq!(instructions.matches("<runtime_model_identity>").count(), 1);
    let identity = instructions
        .split_once("<runtime_model_identity>")
        .unwrap()
        .1
        .split_once("</runtime_model_identity>")
        .expect("identity block must close")
        .0;
    let model_ids = identity
        .lines()
        .filter_map(|line| line.strip_prefix("model_id: "))
        .collect::<Vec<_>>();
    assert_eq!(
        model_ids,
        vec![request["model"].as_str().expect("wire model")]
    );
    instructions
}

#[tokio::test]
async fn overridden_identity_follows_gateway_model_child_switch_and_resume() -> Result<()> {
    const CALLER: &str = "Use the synthetic caller's preferred concise style.";
    const HOST: &str = "Use the synthetic host's authorized workspace.";
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        for (index, model) in [Model::Mimo, Model::Kimi, Model::Mimo, Model::Mimo]
            .into_iter()
            .enumerate()
        {
            let request = next_http_json(&listener).await?;
            assert_eq!(request.body["model"], model.as_str());
            let instructions = assert_runtime_model_identity(&request.body);
            assert!(instructions.contains(CALLER));
            assert!(instructions.contains(HOST));
            let transcript = request.body["input"].to_string();
            if index == 1 {
                assert!(
                    !transcript.contains("original root turn"),
                    "clean child inherited history"
                );
            } else if index > 1 {
                assert!(
                    transcript.contains("original root turn"),
                    "root lost its history"
                );
            }
            if index == 3 {
                assert!(
                    transcript.contains("parent after child"),
                    "resume lost completed turn"
                );
            }
            send_http_final(request.stream, &format!("resp-identity-{index}")).await?;
        }
        Result::<()>::Ok(())
    });
    let openai = OpenAi::builder("synthetic-identity-key")
        .model(Model::Glm53)
        .transport(ResponsesTransport::Https)
        .store(false)
        .api_base_url(endpoint)
        .build()?;
    let (root, root_events) = Nanocodex::builder(openai.clone())
        .thinking(Thinking::Low)
        .instructions(CALLER)
        .additional_instructions(HOST)
        .build()?;
    // Model selection can change only before the first accepted history.
    root.set_harness_model(HarnessModel::Codex(Model::Mimo))
        .await?;
    assert_eq!(
        root.prompt("original root turn")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    let (child, child_events) = root
        .spawn_with(SpawnOptions::new().model(Model::Kimi))
        .await?;
    assert_eq!(
        child
            .prompt("clean child turn")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    child.shutdown().await?;
    drop((child, child_events));
    let completed = root.prompt("parent after child").await?.result().await?;
    assert_eq!(completed.final_message(), "done");
    let snapshot: SessionSnapshot = serde_json::from_value(serde_json::to_value(
        completed.checkpoint().as_ref().map(conversation).unwrap(),
    )?)?;
    root.shutdown().await?;
    drop((root, root_events, completed));
    // The retained MiMo model must win over this builder's original GLM model.
    let (resumed, resumed_events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .instructions(CALLER)
        .additional_instructions(HOST)
        .resume_native_snapshot(snapshot)
        .build()?;
    assert_eq!(
        resumed
            .prompt("resumed root turn")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    resumed.shutdown().await?;
    drop((resumed, resumed_events));
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

    agent.flush().await?;
    drop((agent, events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    Ok(())
}
