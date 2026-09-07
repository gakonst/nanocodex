use super::*;

#[tokio::test]
async fn a_turn_stream_mirrors_one_turn_and_await_retains_its_result() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let warmup = next_json(&mut socket).await?;
        assert_warmup(&warmup);
        send_warmup(&mut socket, "resp-warmup").await?;
        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("turn-stream")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, mut session_events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;

    let mut turn = agent.prompt("return one answer").await?;
    let mut streamed = Vec::new();
    while let Some(event) = turn.next().await {
        streamed.push(event);
    }
    let result = turn.await?;
    assert_eq!(result.final_message(), "done");
    let usage = result.usage().expect("local turns always report usage");
    assert_eq!(usage.input_tokens(), 10);
    assert_eq!(usage.cached_input_tokens(), 5);
    assert_eq!(usage.cache_write_input_tokens(), 0);
    assert_eq!(usage.output_tokens(), 2);
    assert_eq!(usage.reasoning_output_tokens(), 1);
    assert_eq!(usage.total_tokens(), 12);
    let estimated_cost = usage
        .estimated_cost()
        .expect("provider usage should produce an estimate");
    assert_eq!(estimated_cost.amount().decimal(), "0.000155");
    assert_eq!(usage.cost_status(), CostStatus::EstimatedFromUsage);

    drop(agent);
    let mut session = Vec::new();
    while let Some(event) = session_events.recv().await {
        session.push(event);
    }
    assert_eq!(
        streamed
            .iter()
            .map(|event| (event.seq, event.kind, event.payload.get()))
            .collect::<Vec<_>>(),
        session
            .iter()
            .map(|event| (event.seq, event.kind, event.payload.get()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        streamed.last().map(|event| event.kind),
        Some(AgentEventKind::RunCompleted)
    );
    let terminal = streamed
        .last()
        .expect("the turn should have a terminal event");
    let AgentEventData::Run(RunEvent::Completed(typed_terminal)) = terminal.data()? else {
        return Err(eyre!(
            "terminal event should have a typed completed projection"
        ));
    };
    assert_eq!(
        typed_terminal
            .estimated_cost
            .as_ref()
            .expect("terminal should retain the automatic estimate")
            .amount()
            .decimal(),
        "0.000155"
    );
    let terminal_payload = terminal.decode_payload::<Value>()?;
    assert_eq!(terminal_payload["estimated_cost"]["usd"], json!("0.000155"));
    assert_eq!(
        terminal_payload["estimated_cost"]["service_tier"],
        json!("standard")
    );
    assert_eq!(
        terminal_payload["cost_status"],
        json!("estimated_from_usage")
    );

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn https_ephemeral_replays_complete_follow_on_history() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let first = next_http_json(&listener).await?;
        assert_eq!(first.body["store"], false);
        assert!(first.body.get("type").is_none());
        assert!(first.body.get("previous_response_id").is_none());
        assert!(first.body.to_string().contains("first prompt"));
        send_http_final(first.stream, "resp-first").await?;

        let second = next_http_json(&listener).await?;
        assert_eq!(second.body["store"], false);
        assert!(second.body.get("type").is_none());
        assert!(second.body.get("previous_response_id").is_none());
        let replay = second.body.to_string();
        assert!(replay.contains("first prompt"));
        assert!(replay.contains("done"));
        assert!(replay.contains("second prompt"));
        send_http_final(second.stream, "resp-second").await
    });

    let workspace = temporary_workspace("https-ephemeral-follow-on")?;
    let openai = OpenAi::builder("test-key")
        .transport(ResponsesTransport::Https)
        .store(false)
        .api_base_url(endpoint)
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
        .map_err(|_| eyre!("mock HTTPS Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn https_stored_fork_uses_the_historical_response_checkpoint() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let root = next_http_json(&listener).await?;
        assert_eq!(root.body["store"], true);
        assert!(root.body.get("previous_response_id").is_none());
        send_http_final(root.stream, "resp-root").await?;

        let branch = next_http_json(&listener).await?;
        assert_eq!(branch.body["store"], true);
        assert_eq!(branch.body["previous_response_id"], "resp-root");
        assert!(branch.body.to_string().contains("branch prompt"));
        send_http_final(branch.stream, "resp-branch").await
    });

    let workspace = temporary_workspace("https-stored-fork")?;
    let openai = OpenAi::builder("test-key")
        .experimental_context(false)
        .transport(ResponsesTransport::Https)
        .store(true)
        .api_base_url(endpoint)
        .build()?;
    let (agent, root_events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let root = agent.prompt("root prompt").await?.result().await?;
    let (fork, fork_events) = agent.fork_from(&root).await?;
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
        .map_err(|_| eyre!("mock HTTPS Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn chatgpt_https_uses_subscription_headers_and_ephemeral_replay() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let request = next_http_json(&listener).await?;
        assert!(
            request
                .headers
                .contains("authorization: bearer subscription-token")
        );
        assert!(request.headers.contains("chatgpt-account-id: account-123"));
        assert_eq!(request.body["store"], false);
        assert!(request.body.get("previous_response_id").is_none());
        send_http_final(request.stream, "resp-chatgpt").await
    });

    let workspace = temporary_workspace("https-chatgpt")?;
    let openai = OpenAi::builder(chatgpt_auth())
        .transport(ResponsesTransport::Https)
        .api_base_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    assert_eq!(
        agent
            .prompt("subscription prompt")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    drop((agent, events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock HTTPS Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn https_uses_the_configured_http_client() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let request = next_http_json(&listener).await?;
        assert!(request.headers.contains("x-nanocodex-client: configured"));
        send_http_final(request.stream, "resp-configured-client").await
    });
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-nanocodex-client",
        reqwest::header::HeaderValue::from_static("configured"),
    );
    nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()?;
    let workspace = temporary_workspace("https-configured-client")?;
    let openai = OpenAi::builder("test-key")
        .transport(ResponsesTransport::Https)
        .api_base_url(endpoint)
        .http_client(client)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;

    assert_eq!(
        agent
            .prompt("configured client")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    drop((agent, events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock HTTPS Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn configured_attempt_limit_prevents_a_paid_request_replay() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let request = next_http_json(&listener).await?;
        send_http_unexpected_end(request.stream).await?;
        if timeout(std::time::Duration::from_millis(100), listener.accept())
            .await
            .is_ok()
        {
            return Err(eyre!("Responses client replayed the failed paid request"));
        }
        Ok(())
    });

    let workspace = temporary_workspace("https-single-attempt")?;
    let openai = OpenAi::builder("test-key")
        .transport(ResponsesTransport::Https)
        .api_base_url(endpoint)
        .max_attempts(NonZeroU32::MIN)
        .build()?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;

    let result = agent.prompt("paid request").await?.result().await;
    assert!(result.is_err());
    drop(agent);

    let mut generation_attempts = 0;
    let mut reported_max_attempts = None;
    let mut observed_retry = false;
    while let Some(event) = events.recv().await {
        match event.kind {
            AgentEventKind::ModelAttemptStarted => {
                let payload = event.decode_payload::<Value>()?;
                if payload["phase"] == "generation" {
                    generation_attempts += 1;
                    reported_max_attempts = payload["max_attempts"].as_u64();
                }
            }
            AgentEventKind::ModelAttemptRetrying => observed_retry = true,
            _ => {}
        }
    }
    assert_eq!(generation_attempts, 1);
    assert_eq!(reported_max_attempts, Some(1));
    assert!(!observed_retry);

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock HTTPS Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[test]
fn rejects_invalid_auth_storage_and_https_history_policies() {
    let error = OpenAi::builder(chatgpt_auth())
        .store(true)
        .build()
        .err()
        .expect("ChatGPT store:true must fail");
    assert!(
        error
            .to_string()
            .contains("ChatGPT subscription authentication does not support store: true")
    );

    let error = OpenAi::builder("test-key")
        .transport(ResponsesTransport::Https)
        .store(false)
        .history(ResponsesHistory::Incremental)
        .build()
        .err()
        .expect("ephemeral HTTPS incremental history must fail");
    assert!(
        error
            .to_string()
            .contains("HTTPS with store: false requires full client-history replay")
    );
}
