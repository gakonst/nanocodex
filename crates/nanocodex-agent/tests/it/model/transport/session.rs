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
        let mut requests = Vec::new();
        for response_id in ["resp-first", "resp-second", "resp-third", "resp-fourth"] {
            let request = next_http_json(&listener).await?;
            requests.push(request.body);
            send_http_final(request.stream, response_id).await?;
        }
        Result::<_>::Ok(requests)
    });

    let workspace = temporary_workspace("https-ephemeral-follow-on")?;
    let openai = OpenAi::builder("test-key")
        .model(Model::Luna)
        .transport(ResponsesTransport::Https)
        .store(false)
        .api_base_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .fast_mode(false)
        .instructions("Keep these developer instructions unchanged.")
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    for (prompt, thinking, fast) in [
        ("first prompt", Thinking::Low, false),
        ("second prompt", Thinking::High, false),
        ("third prompt", Thinking::High, true),
        ("fourth prompt", Thinking::High, false),
    ] {
        agent.set_thinking(thinking).await?;
        agent.set_fast_mode(fast).await?;
        assert_eq!(
            agent.prompt(prompt).await?.result().await?.final_message(),
            "done"
        );
    }
    drop((agent, events));
    let requests = timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock HTTPS Responses server did not finish"))???;
    for (index, request) in requests.iter().enumerate() {
        assert_eq!(request["store"], false);
        assert!(request.get("type").is_none());
        assert!(request.get("previous_response_id").is_none());
        assert_eq!(
            request["reasoning"]["effort"],
            if index == 0 { "low" } else { "high" }
        );
        if index == 2 {
            assert_eq!(request["service_tier"], "priority");
        } else {
            assert!(request.get("service_tier").is_none());
        }
        assert_eq!(request["prompt_cache_key"], requests[0]["prompt_cache_key"]);
        assert_eq!(request.get("instructions"), requests[0].get("instructions"));
        let input = request["input"].as_array().expect("HTTP input array");
        assert!(
            input
                .iter()
                .all(|item| item["type"] != "configuration_update")
        );
        assert_eq!(&input[..2], &requests[0]["input"].as_array().unwrap()[..2]);
        if index > 0 {
            let previous = requests[index - 1]["input"].as_array().unwrap();
            assert_eq!(&input[..previous.len()], previous);
            assert_eq!(input.len(), previous.len() + 2);
            assert_eq!(input[previous.len()]["role"], "assistant");
            assert_eq!(input[previous.len()]["content"][0]["text"], "done");
        }
        let prompt = [
            "first prompt",
            "second prompt",
            "third prompt",
            "fourth prompt",
        ][index];
        assert_eq!(input.last().unwrap()["role"], "user");
        assert!(input.last().unwrap().to_string().contains(prompt));
        println!(
            "HTTP replay turn={} effort={} service_tier={} input_items={} prior_input_and_prefix=unchanged configuration_updates=0",
            index + 1,
            request["reasoning"]["effort"],
            request
                .get("service_tier")
                .map_or("omitted", |tier| tier.as_str().unwrap()),
            input.len()
        );
    }
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn https_restored_configuration_update_is_retained_but_not_sent() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for response_id in ["resp-original", "resp-restored"] {
            let request = next_http_json(&listener).await?;
            requests.push(request.body);
            send_http_final(request.stream, response_id).await?;
        }
        let compact = next_http_json(&listener).await?;
        requests.push(compact.body);
        let item = json!({
            "type": "response.output_item.done",
            "item": {
                "id": "cmp-http-restored",
                "type": "compaction",
                "encrypted_content": "opaque-http-summary"
            }
        });
        let completed = completed_response_with_usage("resp-compacted", &[], 120);
        let body = format!("data: {item}\n\ndata: {completed}\n\ndata: [DONE]\n\n");
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut stream = compact.stream;
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await?;
        Result::<_>::Ok(requests)
    });
    let workspace = temporary_workspace("https-restored-configuration")?;
    let openai = OpenAi::builder("test-key")
        .model(Model::Luna)
        .transport(ResponsesTransport::Https)
        .store(false)
        .api_base_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai.clone())
        .thinking(Thinking::Low)
        .instructions("Keep these developer instructions unchanged.")
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;
    let first = agent.prompt("original user prompt").await?.result().await?;
    assert_eq!(first.final_message(), "done");
    let snapshot = first
        .checkpoint()
        .as_ref()
        .map(conversation)
        .expect("completed local snapshot");
    let (head, mut history, prefix) = snapshot.into_context_parts();
    // Simulate a saved session from a client that emitted effort overrides.
    // Developer history must survive the same outgoing-only filter.
    history.push(serde_json::from_value(json!({
        "type": "message",
        "id": "msg_saved_developer",
        "role": "developer",
        "content": [{ "type": "input_text", "text": "Retain saved developer guidance." }]
    }))?);
    let configuration =
        nanocodex_oai_api::responses::ResponseItem::configuration_update(Thinking::Low);
    let configuration_json = serde_json::to_value(&configuration)?;
    history.push(configuration);
    let saved: SessionSnapshot =
        serde_json::from_value(serde_json::to_value(head.with_context(history, prefix))?)?;
    let saved_json = serde_json::to_value(&saved)?;
    agent.shutdown().await?;
    drop((agent, events, first));

    let (resumed, events) = Nanocodex::builder(openai)
        .thinking(Thinking::High)
        .fast_mode(false)
        .instructions("Keep these developer instructions unchanged.")
        .resume_native_snapshot(saved)
        .build()?;
    let result = resumed.prompt("resume user prompt").await?.result().await?;
    assert_eq!(result.final_message(), "done");
    let retained = serde_json::to_value(
        result
            .checkpoint()
            .as_ref()
            .map(conversation)
            .expect("resumed local snapshot"),
    )?;
    let saved_history = saved_json["history"].as_array().unwrap();
    let retained_history = retained["history"].as_array().unwrap();
    assert_eq!(&retained_history[..saved_history.len()], saved_history);
    assert_eq!(
        retained_history
            .iter()
            .filter(|item| **item == configuration_json)
            .count(),
        1
    );
    resumed.compact().await?;
    let compacted = serde_json::to_value(
        resumed
            .checkpoint()
            .await
            .map(|checkpoint| conversation(&checkpoint))?,
    )?;
    assert!(compacted["history"].as_array().unwrap().iter().any(|item| {
        item["type"] == "compaction" && item["encrypted_content"] == "opaque-http-summary"
    }));
    resumed.shutdown().await?;
    drop((resumed, events, result));
    let requests = timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock HTTPS Responses server did not finish"))???;
    let request = &requests[1];
    assert_eq!(request["reasoning"]["effort"], "high");
    assert!(request.get("service_tier").is_none());
    assert!(request.get("previous_response_id").is_none());
    assert_eq!(request["store"], false);
    assert_eq!(request.get("instructions"), requests[0].get("instructions"));
    assert_eq!(request["prompt_cache_key"], requests[0]["prompt_cache_key"]);
    let input = request["input"]
        .as_array()
        .expect("restored HTTP input array");
    assert_eq!(&input[..2], &requests[0]["input"].as_array().unwrap()[..2]);
    assert!(
        input
            .iter()
            .all(|item| item["type"] != "configuration_update")
    );
    let expected_history = saved_history
        .iter()
        .filter(|item| item["type"] != "configuration_update")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(&input[2..2 + expected_history.len()], &expected_history);
    assert!(expected_history.iter().any(|item| item["role"] == "user"));
    assert!(
        expected_history
            .iter()
            .any(|item| item["role"] == "assistant")
    );
    assert!(
        expected_history
            .iter()
            .any(|item| item["role"] == "developer")
    );
    assert!(
        input
            .last()
            .unwrap()
            .to_string()
            .contains("resume user prompt")
    );
    let compact = &requests[2];
    assert_eq!(compact["reasoning"]["effort"], "high");
    assert!(compact.get("service_tier").is_none());
    assert_eq!(compact["prompt_cache_key"], request["prompt_cache_key"]);
    let compact_input = compact["input"]
        .as_array()
        .expect("compaction HTTP input array");
    assert!(
        compact_input
            .iter()
            .all(|item| item["type"] != "configuration_update")
    );
    assert_eq!(&compact_input[..input.len()], input);
    assert_eq!(compact_input.last().unwrap()["type"], "compaction_trigger");
    println!(
        "HTTP compaction effort=high service_tier=omitted configuration_updates=0 prompt_cache_key=unchanged prior_input=unchanged compacted_snapshot=installed input_items={}",
        compact_input.len()
    );
    println!(
        "HTTP restored effort={} service_tier=omitted wire_configuration_updates=0 retained_configuration_updates=1 saved_effort=low prior_user_assistant_developer=unchanged prefix=unchanged input_items={}",
        request["reasoning"]["effort"],
        input.len()
    );
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
    let (fork, fork_events) = agent.fork(ForkRequest::at_turn(&root)).await?;
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

#[tokio::test]
async fn supported_reasoning_snapshot_resume_preserves_pin_and_appends_only_changes() -> Result<()>
{
    supported_reasoning_resume_preserves_pin(false).await
}

#[tokio::test]
async fn supported_reasoning_rollout_resume_preserves_pin_and_appends_only_changes() -> Result<()> {
    supported_reasoning_resume_preserves_pin(true).await
}

async fn supported_reasoning_resume_preserves_pin(durable_resume: bool) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for index in 0..4 {
            let request = next_http_json(&listener).await?;
            requests.push(request.body);
            let response = completed_response(
                &format!("resp-resume-{index}"),
                &[json!({
                    "id": format!("msg_resume_{index}"), "type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "done"}]
                })],
            );
            let body = format!("data: {response}\n\ndata: [DONE]\n\n");
            let mut stream = request.stream;
            stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
            stream.shutdown().await?;
        }
        Result::<_>::Ok(requests)
    });
    let workspace = temporary_workspace("supported-policy-resume")?;
    let rollout_home = temporary_workspace("supported-policy-rollout")?;
    let openai = OpenAi::builder("test-key")
        .model(Model::Sol)
        .transport(ResponsesTransport::Https)
        .api_base_url(endpoint)
        .max_attempts(NonZeroU32::MIN)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai.clone())
        .thinking(Thinking::Medium)
        .fast_mode(false)
        .instructions("Keep the saved developer instructions unchanged.")
        .workspace(&workspace)
        .session_id(test_session_id())
        .rollout(RolloutConfig::new(&rollout_home))
        .build()?;
    agent.prompt("initial medium").await?.result().await?;
    agent.set_thinking(Thinking::High).await?;
    agent.prompt("changed high").await?.result().await?;
    // Serialize and deserialize the public snapshot to exercise persistence,
    // rather than reusing live transport state from the original agent.
    let saved_json = serde_json::to_value(
        agent
            .checkpoint()
            .await
            .map(|checkpoint| conversation(&checkpoint))?,
    )?;
    let saved: SessionSnapshot = serde_json::from_value(saved_json.clone())?;
    agent.shutdown().await?;
    drop((agent, events));
    // Reassembling a snapshot cannot claim a trusted update that was removed
    // from its retained history. Reject the inconsistent public resume input.
    let (head, mut edited_history, prefix) = saved.clone().into_context_parts();
    edited_history.retain(|item| {
        !matches!(item,
        nanocodex_oai_api::responses::ResponseItem::ConfigurationUpdate { reasoning }
            if reasoning.effort == Thinking::High)
    });
    let invalid = head.with_context(edited_history, prefix);
    assert!(matches!(
        Nanocodex::builder(openai.clone())
            .resume_native_snapshot(invalid)
            .build(),
        Err(NanocodexError::InvalidSessionSnapshot(_))
    ));
    let saved = if durable_resume {
        let durable = RolloutConfig::new(&rollout_home).load_session(TEST_SESSION_ID)?;
        assert_eq!(
            serde_json::to_value(durable.snapshot())?["reasoning"],
            saved_json["reasoning"]
        );
        durable.into_parts().1
    } else {
        saved
    };
    let (resumed, events) = Nanocodex::builder(openai)
        .thinking(Thinking::High)
        .fast_mode(false)
        .instructions("Keep the saved developer instructions unchanged.")
        .resume_native_snapshot(saved)
        .build()?;
    assert_eq!(
        resumed
            .prompt("resumed high")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    resumed.set_thinking(Thinking::Low).await?;
    assert_eq!(
        resumed
            .prompt("lowered low")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    let restored_json = serde_json::to_value(
        resumed
            .checkpoint()
            .await
            .map(|checkpoint| conversation(&checkpoint))?,
    )?;
    resumed.shutdown().await?;
    drop((resumed, events));
    let requests = timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("supported snapshot server did not finish"))???;
    for (index, request) in requests.iter().enumerate() {
        assert_eq!(request["model"], "gpt-6.1-sol");
        assert_eq!(request["reasoning"]["effort"], "medium");
        assert_eq!(request["prompt_cache_key"], requests[0]["prompt_cache_key"]);
        assert!(request.get("previous_response_id").is_none());
        assert!(request.get("service_tier").is_none());
        let input = request["input"].as_array().unwrap();
        assert_eq!(&input[..2], &requests[0]["input"].as_array().unwrap()[..2]);
        if index > 0 {
            let previous = requests[index - 1]["input"].as_array().unwrap();
            assert_eq!(
                &input[..previous.len()],
                previous,
                "resume must preserve every retained item and ID"
            );
            assert_eq!(input.len(), previous.len() + 2 + usize::from(index != 2));
            assert_eq!(
                input[previous.len()],
                json!({
                    "id": format!("msg_resume_{}", index - 1), "type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "done"}]
                })
            );
        }
        let mut user = input[input.len() - 1 - usize::from(index != 2)].clone();
        remove_client_item_id(&mut user, "msg");
        assert_eq!(
            user,
            json!({"type": "message", "role": "user", "content": [{
                "type": "input_text", "text": (["initial medium", "changed high", "resumed high", "lowered low"][index])
            }]})
        );
        if index != 2 {
            assert_eq!(
                input.last().unwrap(),
                &json!({"type": "configuration_update", "reasoning": {
                    "effort": (["medium", "high", "high", "low"][index])
                }})
            );
        }
        assert_eq!(
            input
                .iter()
                .filter(|item| item["type"] == "configuration_update")
                .count(),
            [1, 2, 2, 3][index]
        );
        eprintln!(
            "supported-resume-wire durable={durable_resume} turn={index} pinned={} retained_items={} no_redundant_resume_update=true",
            request["reasoning"]["effort"],
            input.len()
        );
    }
    let saved_history = saved_json["history"].as_array().unwrap();
    assert_eq!(
        &requests[2]["input"].as_array().unwrap()[2..2 + saved_history.len()],
        saved_history
    );
    assert_eq!(
        &restored_json["history"].as_array().unwrap()[..saved_history.len()],
        saved_history
    );
    std::fs::remove_dir_all(workspace)?;
    std::fs::remove_dir_all(rollout_home)?;
    Ok(())
}
