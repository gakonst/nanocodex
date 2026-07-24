use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use eyre::{Result, eyre};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

use crate::{
    AgentHandle, Nanocodex, NanocodexError, OpenAiAuth, Prompt, Responses, ResponsesError,
    ResponsesHistory, ResponsesTransport, RolloutConfig, SessionSnapshot, Thinking, Tools,
};

#[derive(Clone)]
struct StaticChatGptAuth;

impl nanocodex_core::OpenAiAuthSource for StaticChatGptAuth {
    fn validate(&self) -> std::result::Result<(), nanocodex_core::OpenAiAuthError> {
        Ok(())
    }

    fn snapshot(
        &self,
    ) -> nanocodex_core::OpenAiAuthFuture<
        '_,
        std::result::Result<nanocodex_core::OpenAiAuthSnapshot, nanocodex_core::OpenAiAuthError>,
    > {
        Box::pin(async {
            Ok(nanocodex_core::OpenAiAuthSnapshot::new(
                nanocodex_core::OpenAiAuthMode::ChatGpt,
                "subscription-token",
                Some("account-123"),
                false,
                0,
            ))
        })
    }

    fn recover_unauthorized(
        &self,
        _rejected: &nanocodex_core::OpenAiAuthSnapshot,
    ) -> nanocodex_core::OpenAiAuthFuture<
        '_,
        std::result::Result<(), nanocodex_core::OpenAiAuthError>,
    > {
        Box::pin(async { Ok(()) })
    }
}

fn chatgpt_auth() -> OpenAiAuth {
    OpenAiAuth::managed_chatgpt(Arc::new(StaticChatGptAuth))
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
    let responses = Responses::builder()
        .transport(ResponsesTransport::Https)
        .store(false)
        .api_base_url(endpoint)
        .build();
    let (agent, events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("https-ephemeral")
        .build()?;
    assert_eq!(
        agent
            .prompt("first prompt")
            .await?
            .result()
            .await?
            .final_message,
        "done"
    );
    assert_eq!(
        agent
            .prompt("second prompt")
            .await?
            .result()
            .await?
            .final_message,
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
    let responses = Responses::builder()
        .transport(ResponsesTransport::Https)
        .store(true)
        .api_base_url(endpoint)
        .build();
    let (agent, root_events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("https-stored")
        .build()?;
    let root = agent.prompt("root prompt").await?.result().await?;
    let (fork, fork_events) = agent.fork_from(&root).await?;
    assert_eq!(
        fork.prompt("branch prompt")
            .await?
            .result()
            .await?
            .final_message,
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
    let responses = Responses::builder()
        .transport(ResponsesTransport::Https)
        .api_base_url(endpoint)
        .build();
    let (agent, events) = Nanocodex::builder(chatgpt_auth())
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("https-chatgpt")
        .build()?;
    assert_eq!(
        agent
            .prompt("subscription prompt")
            .await?
            .result()
            .await?
            .final_message,
        "done"
    );
    drop((agent, events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock HTTPS Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[test]
fn rejects_invalid_auth_storage_and_https_history_policies() {
    let stored_chatgpt = Responses::builder().store(true).build();
    let error = Nanocodex::builder(chatgpt_auth())
        .responses(stored_chatgpt)
        .build()
        .err()
        .expect("ChatGPT store:true must fail");
    assert!(
        error
            .to_string()
            .contains("ChatGPT subscription authentication does not support store: true")
    );

    let incremental_ephemeral_https = Responses::builder()
        .transport(ResponsesTransport::Https)
        .store(false)
        .history(ResponsesHistory::Incremental)
        .build();
    let error = Nanocodex::builder("test-key")
        .responses(incremental_ephemeral_https)
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
async fn websocket_ephemeral_chains_on_connection_and_replays_a_fresh_fork() -> Result<()> {
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
        let replay = next_json(&mut branch).await?;
        assert_eq!(replay["store"], false);
        assert!(replay.get("previous_response_id").is_none());
        let replay = replay.to_string();
        assert!(replay.contains("first prompt"));
        assert!(replay.contains("branch prompt"));
        send_final(&mut branch, "resp-branch").await
    });

    let workspace = temporary_workspace("websocket-ephemeral-fork")?;
    let responses = Responses::builder()
        .websocket_url(endpoint)
        .store(false)
        .build();
    let (agent, root_events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("websocket-ephemeral")
        .build()?;
    let first = agent.prompt("first prompt").await?.result().await?;
    assert_eq!(
        agent
            .prompt("second prompt")
            .await?
            .result()
            .await?
            .final_message,
        "done"
    );
    let (fork, fork_events) = agent.fork_from(&first).await?;
    assert_eq!(
        fork.prompt("branch prompt")
            .await?
            .result()
            .await?
            .final_message,
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
    let responses = Responses::builder()
        .websocket_url(endpoint)
        .store(false)
        .history(ResponsesHistory::FullReplay)
        .build();
    let (agent, events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("websocket-replay")
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
async fn follow_on_prompts_can_change_turn_policy_without_restarting_the_session() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let warmup = next_json(&mut socket).await?;
        assert_warmup(&warmup);
        assert_eq!(warmup["reasoning"]["effort"], "low");
        assert_eq!(warmup["input"][1]["content"][0]["text"], "custom prompt");
        send_warmup(&mut socket, "resp-warmup").await?;

        let first = next_json(&mut socket).await?;
        assert_eq!(first["previous_response_id"], "resp-warmup");
        assert_eq!(first["reasoning"]["effort"], "low");
        assert!(first.get("service_tier").is_none());
        let prompt_cache_key = first["prompt_cache_key"].clone();
        send_final(&mut socket, "resp-first").await?;

        let follow_on = next_json(&mut socket).await?;
        assert_eq!(follow_on["previous_response_id"], "resp-first");
        assert_eq!(follow_on["reasoning"]["effort"], "high");
        assert_eq!(follow_on["service_tier"], "priority");
        assert_eq!(follow_on["prompt_cache_key"], prompt_cache_key);
        assert_eq!(follow_on["input"].as_array().map(Vec::len), Some(1));
        assert_eq!(follow_on["input"][0]["role"], "user");
        assert_eq!(follow_on["input"][0]["content"][0]["text"], "second prompt");
        send_final(&mut socket, "resp-second").await?;

        let standard = next_json(&mut socket).await?;
        assert_eq!(standard["previous_response_id"], "resp-second");
        assert_eq!(standard["reasoning"]["effort"], "high");
        assert!(standard.get("service_tier").is_none());
        send_final(&mut socket, "resp-third").await
    });

    let workspace = temporary_workspace("follow-on")?;
    let responses = Responses::builder().websocket_url(endpoint).build();
    let (agent, mut events) = Nanocodex::builder("test-key")
        .instructions("custom prompt")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("model-test")
        .build()?;

    let first = agent.prompt(Prompt::new("first prompt")).await?;
    assert_eq!(first.result().await?.final_message, "done");
    agent.set_thinking(Thinking::High).await?;
    agent.set_fast_mode(true).await?;
    let second = agent.prompt(Prompt::new("second prompt")).await?;
    assert_eq!(second.result().await?.final_message, "done");
    agent.set_fast_mode(false).await?;
    let third = agent.prompt(Prompt::new("third prompt")).await?;
    assert_eq!(third.result().await?.final_message, "done");
    drop(agent);

    let mut completed = Vec::new();
    while let Some(event) = events.recv().await {
        if event.kind == nanocodex_core::AgentEventKind::RunCompleted {
            completed.push(event.decode_payload::<Value>()?);
        }
    }
    assert_eq!(completed.len(), 3);
    assert_eq!(completed[0]["connection_attempts"], 1);
    assert_eq!(completed[0]["response_attempts"], 2);
    assert_eq!(completed[0]["effort"], "low");
    assert_eq!(completed[1]["connection_attempts"], 0);
    assert_eq!(completed[1]["response_attempts"], 1);
    assert_eq!(completed[1]["effort"], "high");
    assert_eq!(completed[2]["connection_attempts"], 0);
    assert_eq!(completed[2]["response_attempts"], 1);
    assert_eq!(completed[2]["effort"], "high");

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn queued_prompts_retain_the_thinking_captured_when_accepted() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (first_started, first_started_rx) = tokio::sync::oneshot::channel();
    let (release_first, release_first_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let warmup = next_json(&mut socket).await?;
        assert_eq!(warmup["reasoning"]["effort"], "low");
        send_warmup(&mut socket, "resp-warmup").await?;

        let first = next_json(&mut socket).await?;
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
        assert_eq!(continuation["previous_response_id"], "resp-first-tool");
        assert_eq!(continuation["reasoning"]["effort"], "low");
        send_final(&mut socket, "resp-first").await?;

        let queued = next_json(&mut socket).await?;
        assert_eq!(queued["previous_response_id"], "resp-first");
        assert_eq!(queued["reasoning"]["effort"], "low");
        assert!(queued.get("service_tier").is_none());
        send_final(&mut socket, "resp-queued").await?;

        let updated = next_json(&mut socket).await?;
        assert_eq!(updated["previous_response_id"], "resp-queued");
        assert_eq!(updated["reasoning"]["effort"], "high");
        assert_eq!(updated["service_tier"], "priority");
        send_final(&mut socket, "resp-updated").await
    });

    let workspace = temporary_workspace("queued-thinking")?;
    let responses = Responses::builder().websocket_url(endpoint).build();
    let (agent, events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("model-test")
        .build()?;

    let first = agent.prompt("first prompt").await?;
    first_started_rx
        .await
        .map_err(|_| eyre!("first request was not observed"))?;
    let queued = agent.prompt("queued prompt").await?;
    agent.set_thinking(Thinking::High).await?;
    agent.set_fast_mode(true).await?;
    release_first
        .send(())
        .map_err(|()| eyre!("first request release receiver dropped"))?;
    first.result().await?;
    queued.result().await?;
    agent.prompt("updated prompt").await?.result().await?;

    drop((agent, events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn assistant_events_preserve_commentary_and_final_answer_phases() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let initial = next_json(&mut socket).await?;
        assert_eq!(initial["previous_response_id"], "resp-warmup");
        let commentary = send_assistant_output(
            &mut socket,
            0,
            "msg-commentary",
            "commentary",
            "I’ll verify.",
        )
        .await?;
        send_json(
            &mut socket,
            completed_response(
                "resp-commentary",
                &[
                    commentary,
                    json!({
                        "id": "call-item",
                        "type": "custom_tool_call",
                        "call_id": "call-exec",
                        "name": "exec",
                        "input": "text(\"observed\");"
                    }),
                ],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-commentary");
        let final_answer =
            send_assistant_output(&mut socket, 0, "msg-final", "final_answer", "Done.").await?;
        send_json(
            &mut socket,
            completed_response("resp-final", &[final_answer]),
        )
        .await
    });

    let workspace = temporary_workspace("assistant-phases")?;
    let responses = Responses::builder().websocket_url(endpoint).build();
    let (agent, mut events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("model-test")
        .build()?;
    let turn = agent.prompt("check the live state").await?;
    assert_eq!(turn.result().await?.final_message, "Done.");
    drop(agent);

    let mut deltas = Vec::new();
    let mut messages = Vec::new();
    let mut timeline = Vec::new();
    while let Some(event) = events.recv().await {
        match event.kind {
            nanocodex_core::AgentEventKind::AssistantDelta => {
                deltas.push(event.decode_payload::<Value>()?);
            }
            nanocodex_core::AgentEventKind::AssistantMessage => {
                let message = event.decode_payload::<Value>()?;
                timeline.push(message["phase"].clone());
                messages.push(message);
            }
            nanocodex_core::AgentEventKind::ToolCall => {
                timeline.push(json!("tool.call"));
            }
            nanocodex_core::AgentEventKind::ToolResult => {
                timeline.push(json!("tool.result"));
            }
            _ => {}
        }
    }
    assert_assistant_phase_events(&deltas, &messages, &timeline);

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

fn assert_assistant_phase_events(deltas: &[Value], messages: &[Value], timeline: &[Value]) {
    let expected_messages = [
        json!({
            "model_call_index": 1,
            "item_id": "msg-commentary",
            "phase": "commentary",
            "text": "I’ll verify."
        }),
        json!({
            "model_call_index": 2,
            "item_id": "msg-final",
            "phase": "final_answer",
            "text": "Done."
        }),
    ];
    assert_eq!(deltas, expected_messages);
    assert_eq!(messages, expected_messages);
    assert_eq!(
        timeline,
        [
            json!("commentary"),
            json!("tool.call"),
            json!("tool.result"),
            json!("final_answer")
        ]
    );
}

#[tokio::test]
async fn steering_is_bounded_fifo_and_joins_at_the_next_model_boundary() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (first_seen, first_seen_rx) = tokio::sync::oneshot::channel();
    let (release_first, release_first_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let first = next_json(&mut socket).await?;
        assert_eq!(first["previous_response_id"], "resp-warmup");
        assert_eq!(first["input"][2]["content"][0]["text"], "initial task");
        first_seen
            .send(())
            .map_err(|()| eyre!("first-request signal receiver dropped"))?;
        release_first_rx
            .await
            .map_err(|_| eyre!("first-request release sender dropped"))?;
        send_final(&mut socket, "resp-first").await?;

        let steered = next_json(&mut socket).await?;
        assert_eq!(steered["previous_response_id"], "resp-first");
        assert_eq!(steered["input"].as_array().map(Vec::len), Some(8));
        for index in 0..8 {
            assert_eq!(steered["input"][index]["role"], "user");
            assert_eq!(
                steered["input"][index]["content"][0]["text"],
                format!("constraint {index}")
            );
        }
        send_final(&mut socket, "resp-steered").await
    });

    let workspace = temporary_workspace("steer")?;
    let responses = Responses::builder().websocket_url(endpoint).build();
    let (agent, mut events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("model-test")
        .build()?;
    let turn = agent.prompt(Prompt::new("initial task")).await?;
    first_seen_rx
        .await
        .map_err(|_| eyre!("first request was not observed"))?;
    for index in 0..8 {
        turn.steer(format!("constraint {index}")).await?;
    }
    let overflow = turn.steer("constraint 8").await.unwrap_err();
    assert!(matches!(overflow, NanocodexError::SteerQueueFull));
    release_first
        .send(())
        .map_err(|()| eyre!("server release receiver dropped"))?;
    assert_eq!(turn.result().await?.final_message, "done");
    drop(agent);

    let mut steered_events = 0;
    let mut terminal = None;
    while let Some(event) = events.recv().await {
        match event.kind {
            nanocodex_core::AgentEventKind::RunSteered => {
                steered_events += 1;
                let payload = event.decode_payload::<Value>()?;
                assert_eq!(payload["steer_index"], steered_events);
                assert_eq!(payload["instruction_bytes"], "constraint 0".len());
            }
            nanocodex_core::AgentEventKind::RunCompleted => {
                terminal = Some(event.decode_payload::<Value>()?);
            }
            _ => {}
        }
    }
    assert_eq!(steered_events, 8);
    assert_eq!(
        terminal.as_ref().map(|payload| &payload["steers"]),
        Some(&json!(8))
    );

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn steering_during_a_tool_call_joins_after_the_tool_result() -> Result<()> {
    let workspace = temporary_workspace("steer-tool")?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let initial = next_json(&mut socket).await?;
        assert_eq!(initial["previous_response_id"], "resp-warmup");
        send_json(
            &mut socket,
            completed_response(
                "resp-tool",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "const result = await tools.exec_command({cmd: \"printf started > tool-started; while [ ! -f release-tool ]; do sleep 0.01; done; printf shit\"}); text(result.output);"
                })],
            ),
        )
        .await?;

        let steered = next_json(&mut socket).await?;
        assert_eq!(steered["previous_response_id"], "resp-tool");
        let input = steered["input"]
            .as_array()
            .ok_or_else(|| eyre!("steered request input was not an array"))?;
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "custom_tool_call_output");
        assert_eq!(input[0]["call_id"], "call-exec");
        assert!(input[0].to_string().contains("shit"));
        assert_eq!(input[1]["role"], "user");
        assert_eq!(input[1]["content"][0]["text"], "print shat instead");
        send_final(&mut socket, "resp-steered").await
    });

    let responses = Responses::builder().websocket_url(endpoint).build();
    let (agent, mut events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("model-test")
        .build()?;
    let turn = agent.prompt("print shit a lot of times").await?;
    timeout(std::time::Duration::from_secs(5), async {
        while !workspace.join("tool-started").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| eyre!("tool process did not start"))?;

    turn.steer("print shat instead").await?;
    assert!(!workspace.join("release-tool").exists());
    std::fs::write(workspace.join("release-tool"), [])?;
    assert_eq!(turn.result().await?.final_message, "done");
    drop(agent);

    let mut saw_steer = false;
    while let Some(event) = events.recv().await {
        saw_steer |= event.kind == nanocodex_core::AgentEventKind::RunSteered;
    }
    assert!(saw_steer);
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn cancellation_retains_interrupted_prompt_and_resumes_from_the_abort_boundary() -> Result<()>
{
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (second_seen, second_seen_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut first_socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut first_socket).await?);
        send_warmup(&mut first_socket, "resp-warmup").await?;

        let first = next_json(&mut first_socket).await?;
        assert_eq!(first["previous_response_id"], "resp-warmup");
        send_final(&mut first_socket, "resp-first").await?;

        let cancelled = next_json(&mut first_socket).await?;
        assert_eq!(cancelled["previous_response_id"], "resp-first");
        assert_eq!(cancelled["input"][0]["content"][0]["text"], "cancel me");
        second_seen
            .send(())
            .map_err(|()| eyre!("second-request signal receiver dropped"))?;
        send_json(
            &mut first_socket,
            json!({
                "type": "response.output_text.delta",
                "delta": "partial text that must not enter history"
            }),
        )
        .await?;

        let (stream, _) = listener.accept().await?;
        let mut replacement = accept_async(stream).await?;
        let queued = next_json(&mut replacement).await?;
        assert_interrupted_replay(&queued);
        send_final(&mut replacement, "resp-follow-up").await
    });

    let workspace = temporary_workspace("cancel-turn")?;
    let responses = Responses::builder().websocket_url(endpoint).build();
    let (agent, mut events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("model-test")
        .build()?;

    let first = agent.prompt(Prompt::new("first prompt")).await?;
    assert_eq!(first.result().await?.final_message, "done");

    let cancelled = agent.prompt("cancel me").await?;
    second_seen_rx
        .await
        .map_err(|_| eyre!("second request was not observed"))?;
    let queued = agent.prompt("cancel before running").await?;
    let queued_control = queued.control();
    let follow_up = agent.prompt("run after cancellations").await?;

    assert!(matches!(
        queued.steer("wrong target").await,
        Err(NanocodexError::TurnNotSteerable)
    ));
    queued.cancel().await?;
    assert!(matches!(
        queued_control.cancel().await,
        Err(NanocodexError::TurnNotCancellable)
    ));

    let cancellation = cancelled.control();
    cancellation.cancel().await?;
    assert!(matches!(
        cancelled.result().await,
        Err(NanocodexError::TurnCancelled)
    ));
    assert!(matches!(
        queued.result().await,
        Err(NanocodexError::TurnCancelled)
    ));
    assert!(matches!(
        cancellation.cancel().await,
        Err(NanocodexError::TurnNotCancellable)
    ));
    assert_eq!(follow_up.result().await?.final_message, "done");
    drop((queued_control, cancellation, agent));

    let mut terminal_statuses = Vec::new();
    while let Some(event) = events.recv().await {
        match event.kind {
            nanocodex_core::AgentEventKind::RunCompleted
            | nanocodex_core::AgentEventKind::RunFailed => {
                let payload = event.decode_payload::<Value>()?;
                terminal_statuses.push(payload["status"].as_str().unwrap_or_default().to_owned());
            }
            _ => {}
        }
    }
    assert_eq!(
        terminal_statuses,
        ["completed", "cancelled", "cancelled", "completed"]
    );

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

fn assert_interrupted_replay(request: &Value) {
    assert!(request.get("previous_response_id").is_none());
    assert_eq!(request["input"].as_array().map(Vec::len), Some(9));
    assert_eq!(request["input"][0]["type"], "additional_tools");
    assert_eq!(request["input"][1]["role"], "developer");
    assert_eq!(request["input"][2]["role"], "developer");
    assert_eq!(request["input"][3]["role"], "user");
    assert_eq!(request["input"][4]["content"][0]["text"], "first prompt");
    assert_eq!(request["input"][5]["content"][0]["text"], "done");
    assert_eq!(request["input"][6]["content"][0]["text"], "cancel me");
    assert!(
        request["input"][7]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("<turn_aborted>"))
    );
    assert_eq!(
        request["input"][8]["content"][0]["text"],
        "run after cancellations"
    );
    assert!(
        !request
            .to_string()
            .contains("partial text that must not enter history")
    );
}

#[tokio::test]
async fn cancellation_pairs_an_active_tool_call_before_resuming() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut first = accept_async(stream).await?;
        assert_warmup(&next_json(&mut first).await?);
        send_warmup(&mut first, "resp-warmup").await?;

        let generation = next_json(&mut first).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut first,
            completed_response(
                "resp-tool",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "const result = await tools.exec_command({cmd: \"printf started > tool-started; sleep 30\"}); text(result.output);"
                })],
            ),
        )
        .await?;

        let (stream, _) = listener.accept().await?;
        let mut replacement = accept_async(stream).await?;
        let resumed = next_json(&mut replacement).await?;
        assert!(resumed.get("previous_response_id").is_none());
        assert_eq!(resumed["input"].as_array().map(Vec::len), Some(9));
        assert_eq!(resumed["input"][4]["content"][0]["text"], "run a long tool");
        assert_eq!(resumed["input"][5]["type"], "custom_tool_call");
        assert_eq!(resumed["input"][5]["call_id"], "call-exec");
        assert_eq!(resumed["input"][6]["type"], "custom_tool_call_output");
        assert_eq!(resumed["input"][6]["call_id"], "call-exec");
        assert!(resumed["input"][6].to_string().contains("aborted by user"));
        assert!(
            resumed["input"][7]["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("<turn_aborted>"))
        );
        assert_eq!(resumed["input"][8]["content"][0]["text"], "continue");
        send_final(&mut replacement, "resp-follow-up").await
    });

    let workspace = temporary_workspace("cancel-tool")?;
    let responses = Responses::builder().websocket_url(endpoint).build();
    let (agent, mut events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("model-test")
        .build()?;

    let interrupted = agent.prompt("run a long tool").await?;
    loop {
        let event = events
            .recv()
            .await
            .ok_or_else(|| eyre!("event stream closed before the tool call"))?;
        if event.kind == nanocodex_core::AgentEventKind::ToolCall {
            break;
        }
    }
    timeout(std::time::Duration::from_secs(5), async {
        while !workspace.join("tool-started").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| eyre!("tool process did not start"))?;

    interrupted.cancel().await?;
    assert!(matches!(
        interrupted.result().await,
        Err(NanocodexError::TurnCancelled)
    ));
    assert_eq!(
        agent
            .prompt("continue")
            .await?
            .result()
            .await?
            .final_message,
        "done"
    );
    drop(agent);

    let mut saw_cancelled_tool = false;
    while let Some(event) = events.recv().await {
        if event.kind == nanocodex_core::AgentEventKind::ToolResult {
            let payload = event.decode_payload::<Value>()?;
            saw_cancelled_tool |= payload["call_id"] == "call-exec"
                && payload["status"] == "cancelled"
                && payload.to_string().contains("aborted by user");
        }
    }
    assert!(saw_cancelled_tool);
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn stored_response_local_code_mode_round_trip() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let warmup = next_json(&mut socket).await?;
        assert_warmup(&warmup);
        send_json(
            &mut socket,
            json!({
                "type": "response.metadata",
                "headers": { "x-codex-turn-state": "sticky-test" }
            }),
        )
        .await?;
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        assert_eq!(generation["store"], true);
        assert!(generation.get("generate").is_none());
        assert_eq!(generation["input"].as_array().map(Vec::len), Some(3));
        assert_eq!(generation["input"][0]["role"], "developer");
        assert_eq!(generation["input"][1]["role"], "user");
        assert_eq!(generation["input"][2]["role"], "user");
        assert_eq!(
            generation["client_metadata"]["x-codex-turn-state"],
            "sticky-test"
        );
        send_json(
            &mut socket,
            completed_response(
                "resp-tool",
                &[json!({
                    "id": "item-exec",
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "const result = await tools.exec_command({cmd: \"printf hello\"}); text(result.output);"
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-tool");
        assert_eq!(continuation["input"].as_array().map(Vec::len), Some(1));
        assert_eq!(continuation["input"][0]["type"], "custom_tool_call_output");
        assert_eq!(continuation["input"][0]["call_id"], "call-exec");
        assert!(continuation["input"][0].get("success").is_none());
        assert!(
            continuation["input"][0]["output"]
                .as_array()
                .is_some_and(|content| content.iter().any(|item| {
                    item["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("hello"))
                }))
        );
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("code-mode")?;
    let output = run_model(&endpoint, &workspace, "run a shell command").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"tool\":\"exec\""));
    assert!(output.contains("\"tool\":\"exec_command\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn unsupported_direct_tools_return_failed_results_to_the_model() -> Result<()> {
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
                "resp-unsupported",
                &[
                    json!({
                        "type": "custom_tool_call",
                        "call_id": "call-custom",
                        "name": "missing_custom",
                        "input": "raw input"
                    }),
                    json!({
                        "type": "function_call",
                        "call_id": "call-function",
                        "namespace": "example::",
                        "name": "missing_function",
                        "arguments": "not json"
                    }),
                ],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-unsupported");
        let input = continuation["input"]
            .as_array()
            .ok_or_else(|| eyre!("continuation input was not an array"))?;
        assert_eq!(
            input,
            &[
                json!({
                    "type": "custom_tool_call_output",
                    "call_id": "call-custom",
                    "output": "unsupported custom tool call: missing_custom"
                }),
                json!({
                    "type": "function_call_output",
                    "call_id": "call-function",
                    "output": "unsupported call: example::missing_function"
                }),
            ]
        );
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("unsupported-tools")?;
    let output = run_model(&endpoint, &workspace, "recover from unsupported tools").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert_eq!(
        output.matches(r#""status":"failed""#).count(),
        2,
        "{output}"
    );
    assert!(output.contains("\"tool_calls\":2"));
    assert!(output.contains("\"run.completed\""));
    assert!(!output.contains("\"run.failed\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn code_mode_notify_adds_a_named_exec_output_to_the_next_request() -> Result<()> {
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
                "resp-notify",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "notify({phase: \"working\"}); text(\"done\");"
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-notify");
        let input = continuation["input"]
            .as_array()
            .ok_or_else(|| eyre!("continuation input was not an array"))?;
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "custom_tool_call_output");
        assert_eq!(input[0]["call_id"], "call-exec");
        assert!(input[0].get("name").is_none());
        assert!(input[0].to_string().contains("done"));
        assert_eq!(input[1]["type"], "custom_tool_call_output");
        assert_eq!(input[1]["call_id"], "call-exec");
        assert_eq!(input[1]["name"], "exec");
        assert_eq!(input[1]["output"], r#"{"phase":"working"}"#);
        assert!(input[1].get("success").is_none());
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("code-mode-notify")?;
    run_model(&endpoint, &workspace, "send a progress notification").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn prepares_images_and_stops_on_invalid_image_requests() -> Result<()> {
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
                "type": "response.failed",
                "response": {
                    "id": "resp-invalid-image",
                    "status": "failed",
                    "error": {
                        "code": "invalid_image",
                        "message": "The image data you provided does not represent a valid image"
                    }
                }
            }),
        )
        .await?;

        Ok::<(), eyre::Report>(())
    });

    let workspace = temporary_workspace("images")?;
    let error = run_model(&endpoint, &workspace, "inspect images")
        .await
        .expect_err("invalid tool image should fail the turn");
    let error = error
        .downcast_ref::<NanocodexError>()
        .ok_or_else(|| eyre!("invalid image returned the wrong error type"))?;
    assert!(matches!(
        error.responses_error(),
        Some(ResponsesError::InvalidImageRequest { .. })
    ));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn yielded_exec_cell_continues_through_direct_wait_tool() -> Result<()> {
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
                "resp-exec",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "text(\"before\"); await yield_control(); const result = await tools.exec_command({cmd: \"printf after\", login: false}); text(result.output);"
                })],
            ),
        )
        .await?;

        let yielded = next_json(&mut socket).await?;
        assert_eq!(yielded["previous_response_id"], "resp-exec");
        assert_eq!(yielded["input"][0]["type"], "custom_tool_call_output");
        assert!(
            yielded
                .to_string()
                .contains("Script running with cell ID 1")
        );
        send_json(
            &mut socket,
            completed_response(
                "resp-wait",
                &[json!({
                    "type": "function_call",
                    "call_id": "call-wait",
                    "name": "wait",
                    "arguments": "{\"cell_id\":\"1\",\"yield_time_ms\":30000}"
                })],
            ),
        )
        .await?;

        let completed = next_json(&mut socket).await?;
        assert_eq!(completed["previous_response_id"], "resp-wait");
        assert_eq!(completed["input"][0]["type"], "function_call_output");
        assert_eq!(completed["input"][0]["call_id"], "call-wait");
        assert!(completed.to_string().contains("after"));
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("code-mode-wait")?;
    let output = run_model(&endpoint, &workspace, "yield and wait").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"tool\":\"wait\""));
    let nested_call = output
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| {
            event["type"] == "tool.call" && event["payload"]["call_id"] == "call-exec/code-1"
        })
        .ok_or_else(|| eyre!("nested call did not retain its original exec lineage"))?;
    assert_eq!(nested_call["payload"]["model_call_index"], 1);
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
    let output = run_model(&endpoint, &workspace, "accept missing usage").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"model.call.completed\""));
    assert!(output.contains("\"usage\":null"));
    assert!(output.contains("\"run.completed\""));
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

#[tokio::test]
async fn reconnect_drops_previous_response_id_and_replays_full_history() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut first = accept_async(stream).await?;
        let warmup = next_json(&mut first).await?;
        assert_warmup(&warmup);
        send_warmup(&mut first, "resp-warmup").await?;
        let generation = next_json(&mut first).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut first,
            completed_response(
                "resp-tool",
                &[json!({
                    "id": "server-item-id",
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "text(\"continued\")"
                })],
            ),
        )
        .await?;
        first.send(Message::Close(None)).await?;
        drop(first);

        let (stream, _) = listener.accept().await?;
        let mut second = accept_async(stream).await?;
        let replay = next_json(&mut second).await?;
        assert!(replay.get("previous_response_id").is_none());
        assert_eq!(replay["store"], true);
        assert_eq!(replay["input"].as_array().map(Vec::len), Some(7));
        assert_eq!(replay["input"][0]["type"], "additional_tools");
        assert_eq!(replay["input"][1]["role"], "developer");
        assert_eq!(replay["input"][2]["role"], "developer");
        assert_eq!(replay["input"][3]["role"], "user");
        assert_eq!(replay["input"][5]["type"], "custom_tool_call");
        assert!(replay["input"][5].get("id").is_none());
        assert_eq!(replay["input"][6]["type"], "custom_tool_call_output");
        send_final(&mut second, "resp-final").await
    });

    let workspace = temporary_workspace("reconnect")?;
    run_model(&endpoint, &workspace, "exercise reconnect").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn receive_reset_reconnects_without_replaying_completed_tools() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut first = accept_async(stream).await?;
        assert_warmup(&next_json(&mut first).await?);
        send_warmup(&mut first, "resp-warmup").await?;

        let generation = next_json(&mut first).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut first,
            completed_response(
                "resp-tool",
                &[json!({
                    "id": "server-item-id",
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "const result = await tools.exec_command({cmd: \"printf x >> marker.txt\"}); text(result.output);"
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut first).await?;
        assert_eq!(continuation["previous_response_id"], "resp-tool");
        assert_eq!(continuation["input"].as_array().map(Vec::len), Some(1));
        let tool_output = continuation["input"][0].clone();
        send_json(
            &mut first,
            json!({
                "type": "response.created",
                "response": { "id": "resp-interrupted" }
            }),
        )
        .await?;
        send_json(
            &mut first,
            json!({
                "type": "response.in_progress",
                "response": { "id": "resp-interrupted" }
            }),
        )
        .await?;
        send_json(
            &mut first,
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "reasoning" }
            }),
        )
        .await?;
        drop(first);

        let (stream, _) = listener.accept().await?;
        let mut second = accept_async(stream).await?;
        let replay = next_json(&mut second).await?;
        assert!(replay.get("previous_response_id").is_none());
        assert_eq!(replay["input"].as_array().map(Vec::len), Some(7));
        assert_eq!(replay["input"][5]["type"], "custom_tool_call");
        assert_eq!(replay["input"][5]["call_id"], "call-exec");
        assert_eq!(replay["input"][6], tool_output);
        send_final(&mut second, "resp-final").await
    });

    let workspace = temporary_workspace("receive-reconnect")?;
    let output = run_model(&endpoint, &workspace, "recover after a receive reset").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert_eq!(std::fs::read_to_string(workspace.join("marker.txt"))?, "x");
    assert!(output.contains("\"model.attempt.retrying\""));
    assert!(output.contains("failed to receive a Responses WebSocket frame"));
    assert!(output.contains("\"purpose\":\"reconnect\""));
    assert!(output.contains("\"connection_attempts\":2"));
    assert!(output.contains("\"websocket_reconnects\":1"));
    assert!(output.contains("\"model_calls\":2"));
    assert!(!output.contains("\"model.call.failed\""));
    assert!(output.contains("\"run.completed\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn sol_compacts_with_a_trigger_and_installs_the_returned_context() -> Result<()> {
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
                "resp-tool",
                &[json!({
                    "id": "item-exec",
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "await tools.apply_patch(\"*** Begin Patch\\n*** Add File: AGENTS.md\\n+fresh compacted instructions\\n*** End Patch\"); text(\"tool completed\")"
                })],
                372_001,
            ),
        )
        .await?;

        let compact = next_json(&mut socket).await?;
        assert_eq!(compact["previous_response_id"], "resp-tool");
        assert_eq!(compact["input"].as_array().map(Vec::len), Some(2));
        assert_eq!(compact["input"][0]["type"], "custom_tool_call_output");
        assert_eq!(
            compact["input"][0]["output"],
            "Output exceeded the available model context and was truncated"
        );
        assert_eq!(compact["input"][1], json!({ "type": "compaction_trigger" }));
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

        let continuation = next_json(&mut socket).await?;
        assert!(continuation.get("previous_response_id").is_none());
        assert_eq!(continuation["input"].as_array().map(Vec::len), Some(6));
        assert_eq!(continuation["input"][0]["type"], "additional_tools");
        assert_eq!(continuation["input"][1]["role"], "developer");
        assert_eq!(continuation["input"][2]["role"], "developer");
        assert_eq!(continuation["input"][3]["role"], "user");
        assert_eq!(continuation["input"][4]["role"], "user");
        assert_eq!(continuation["input"][5]["type"], "compaction");
        assert_eq!(
            continuation["input"][5]["encrypted_content"],
            "opaque-summary"
        );
        assert!(continuation["input"][5].get("id").is_none());
        assert!(continuation.to_string().contains("exercise compaction"));
        assert!(
            continuation
                .to_string()
                .contains("fresh compacted instructions")
        );
        assert!(!continuation.to_string().contains("tool completed"));
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("compaction")?;
    let output = run_model(&endpoint, &workspace, "exercise compaction").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"model.compaction.started\""));
    assert!(output.contains("\"model.compaction.completed\""));
    assert!(output.contains("\"compactions\":1"));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn latest_fork_during_streaming_inherits_the_active_prompt_delta() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (root_started, root_started_rx) = tokio::sync::oneshot::channel();
    let (release_root, release_root_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        assert_warmup(&next_json(&mut root).await?);
        send_warmup(&mut root, "resp-warmup").await?;

        let active = next_json(&mut root).await?;
        assert_eq!(active["previous_response_id"], "resp-warmup");
        assert_eq!(active["reasoning"]["effort"], "low");
        assert!(active.to_string().contains("active root prompt"));
        root_started
            .send(())
            .map_err(|()| eyre!("root request signal receiver dropped"))?;

        let (stream, _) = listener.accept().await?;
        let mut branch = accept_async(stream).await?;
        let fork = next_json(&mut branch).await?;
        assert_eq!(fork["previous_response_id"], "resp-warmup");
        assert_eq!(fork["reasoning"]["effort"], "high");
        let fork_text = fork.to_string();
        assert!(fork_text.contains("active root prompt"));
        assert!(fork_text.contains("BTW question"));
        send_final(&mut branch, "resp-branch").await?;

        release_root_rx
            .await
            .map_err(|_| eyre!("root release sender dropped"))?;
        send_final(&mut root, "resp-root").await
    });

    let workspace = temporary_workspace("active-prompt-fork")?;
    let responses = Responses::builder().websocket_url(endpoint).build();
    let (agent, root_events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("model-test")
        .build()?;
    let root = agent.prompt("active root prompt").await?;
    root_started_rx
        .await
        .map_err(|_| eyre!("root request was not observed"))?;
    agent.set_thinking(Thinking::High).await?;
    let (fork, fork_events) = agent.fork().await?;
    let branch = fork.prompt("BTW question").await?;
    assert_eq!(branch.result().await?.final_message, "done");
    release_root
        .send(())
        .map_err(|()| eyre!("root release receiver dropped"))?;
    assert_eq!(root.result().await?.final_message, "done");

    drop((agent, fork, root_events, fork_events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn active_boundary_fork_sends_tool_and_steer_delta_then_replays_on_checkpoint_miss()
-> Result<()> {
    let workspace = temporary_workspace("active-tool-steer-fork")?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (boundary_seen, boundary_seen_rx) = tokio::sync::oneshot::channel();
    let (release_root, release_root_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        assert_warmup(&next_json(&mut root).await?);
        send_warmup(&mut root, "resp-warmup").await?;

        let initial = next_json(&mut root).await?;
        assert_eq!(initial["previous_response_id"], "resp-warmup");
        send_json(
            &mut root,
            completed_response(
                "resp-tool",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "const result = await tools.exec_command({cmd: \"printf started > tool-started; while [ ! -f release-tool ]; do sleep 0.01; done; printf shit\"}); text(result.output);"
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut root).await?;
        assert_eq!(continuation["previous_response_id"], "resp-tool");
        assert_eq!(continuation["input"].as_array().map(Vec::len), Some(2));
        assert_eq!(continuation["input"][0]["type"], "custom_tool_call_output");
        assert!(continuation["input"][0].to_string().contains("shit"));
        assert_eq!(
            continuation["input"][1]["content"][0]["text"],
            "print shat instead"
        );
        boundary_seen
            .send(())
            .map_err(|()| eyre!("boundary signal receiver dropped"))?;

        let (stream, _) = listener.accept().await?;
        let mut branch = accept_async(stream).await?;
        let incremental = next_json(&mut branch).await?;
        assert_eq!(incremental["previous_response_id"], "resp-tool");
        assert_eq!(incremental["input"].as_array().map(Vec::len), Some(3));
        assert_eq!(incremental["input"][0]["type"], "custom_tool_call_output");
        assert!(incremental["input"][0].to_string().contains("shit"));
        assert_eq!(
            incremental["input"][1]["content"][0]["text"],
            "print shat instead"
        );
        assert_eq!(
            incremental["input"][2]["content"][0]["text"],
            "BTW question"
        );
        send_json(
            &mut branch,
            json!({
                "type": "error",
                "error": {
                    "code": "previous_response_not_found",
                    "message": "checkpoint expired"
                }
            }),
        )
        .await?;

        let replay = next_json(&mut branch).await?;
        assert!(replay.get("previous_response_id").is_none());
        let replay_text = replay.to_string();
        assert!(replay_text.contains("active root prompt"));
        assert!(replay_text.contains("call-exec"));
        assert!(replay_text.contains("shit"));
        assert!(replay_text.contains("print shat instead"));
        assert!(replay_text.contains("BTW question"));
        send_final(&mut branch, "resp-branch").await?;

        release_root_rx
            .await
            .map_err(|_| eyre!("root release sender dropped"))?;
        send_final(&mut root, "resp-root").await
    });

    let responses = Responses::builder().websocket_url(endpoint).build();
    let (agent, root_events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("model-test")
        .build()?;
    let root = agent.prompt("active root prompt").await?;
    timeout(std::time::Duration::from_secs(5), async {
        while !workspace.join("tool-started").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| eyre!("tool process did not start"))?;
    root.steer("print shat instead").await?;
    std::fs::write(workspace.join("release-tool"), [])?;
    boundary_seen_rx
        .await
        .map_err(|_| eyre!("root boundary request was not observed"))?;

    let (fork, fork_events) = agent.fork().await?;
    assert_eq!(
        fork.prompt("BTW question")
            .await?
            .result()
            .await?
            .final_message,
        "done"
    );
    release_root
        .send(())
        .map_err(|()| eyre!("root release receiver dropped"))?;
    assert_eq!(root.result().await?.final_message, "done");

    drop((agent, fork, root_events, fork_events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn latest_and_historical_forks_keep_distinct_boundaries_during_an_active_turn() -> Result<()>
{
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (active_seen, active_seen_rx) = tokio::sync::oneshot::channel();
    let (release_active, release_active_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        assert_warmup(&next_json(&mut root).await?);
        send_warmup(&mut root, "resp-warmup").await?;
        let first = next_json(&mut root).await?;
        assert!(first.to_string().contains("completed root prompt"));
        send_final(&mut root, "resp-first").await?;

        let active = next_json(&mut root).await?;
        assert_eq!(active["previous_response_id"], "resp-first");
        assert!(active.to_string().contains("active root prompt"));
        active_seen
            .send(())
            .map_err(|()| eyre!("active signal receiver dropped"))?;

        let (stream, _) = listener.accept().await?;
        let mut latest = accept_async(stream).await?;
        let latest_request = next_json(&mut latest).await?;
        assert_eq!(latest_request["previous_response_id"], "resp-first");
        assert_eq!(latest_request["input"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            latest_request["input"][0]["content"][0]["text"],
            "active root prompt"
        );
        assert_eq!(
            latest_request["input"][1]["content"][0]["text"],
            "latest branch prompt"
        );
        send_final(&mut latest, "resp-latest").await?;

        let (stream, _) = listener.accept().await?;
        let mut historical = accept_async(stream).await?;
        let historical_request = next_json(&mut historical).await?;
        assert_eq!(historical_request["previous_response_id"], "resp-first");
        assert_eq!(
            historical_request["input"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            historical_request["input"][0]["content"][0]["text"],
            "historical branch prompt"
        );
        assert!(
            !historical_request
                .to_string()
                .contains("active root prompt")
        );
        send_final(&mut historical, "resp-historical").await?;

        release_active_rx
            .await
            .map_err(|_| eyre!("active release sender dropped"))?;
        send_final(&mut root, "resp-active").await
    });

    let workspace = temporary_workspace("latest-vs-historical-fork")?;
    let responses = Responses::builder().websocket_url(endpoint).build();
    let (agent, root_events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("model-test")
        .build()?;
    let completed = agent
        .prompt("completed root prompt")
        .await?
        .result()
        .await?;
    let active = agent.prompt("active root prompt").await?;
    active_seen_rx
        .await
        .map_err(|_| eyre!("active root request was not observed"))?;

    let (latest, latest_events) = agent.fork().await?;
    assert_eq!(
        latest
            .prompt("latest branch prompt")
            .await?
            .result()
            .await?
            .final_message,
        "done"
    );
    let (historical, historical_events) = agent.fork_from(&completed).await?;
    assert_eq!(
        historical
            .prompt("historical branch prompt")
            .await?
            .result()
            .await?
            .final_message,
        "done"
    );
    release_active
        .send(())
        .map_err(|()| eyre!("active release receiver dropped"))?;
    assert_eq!(active.result().await?.final_message, "done");

    drop((
        agent,
        latest,
        historical,
        root_events,
        latest_events,
        historical_events,
    ));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn historical_fork_runs_while_the_mainline_turn_is_in_flight() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (root_started, root_started_rx) = tokio::sync::oneshot::channel();
    let (branch_started, branch_started_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        let warmup = next_json(&mut root).await?;
        assert_warmup(&warmup);
        let lineage = warmup["prompt_cache_key"].clone();
        let root_session = warmup["client_metadata"]["session_id"].clone();
        send_warmup(&mut root, "resp-warmup").await?;

        let first = next_json(&mut root).await?;
        assert_eq!(first["previous_response_id"], "resp-warmup");
        send_final(&mut root, "resp-first").await?;
        let second = next_json(&mut root).await?;
        assert_eq!(second["previous_response_id"], "resp-first");
        send_final(&mut root, "resp-second").await?;

        let mainline = next_json(&mut root).await?;
        assert_eq!(mainline["previous_response_id"], "resp-second");
        root_started
            .send(())
            .map_err(|()| eyre!("root signal dropped"))?;
        let root_task = tokio::spawn(async move {
            branch_started_rx
                .await
                .map_err(|_| eyre!("branch signal dropped"))?;
            send_final(&mut root, "resp-mainline").await
        });

        let (stream, _) = listener.accept().await?;
        let mut branch = accept_async(stream).await?;
        let fork = next_json(&mut branch).await?;
        assert_eq!(fork["previous_response_id"], "resp-first");
        assert_eq!(fork["prompt_cache_key"], lineage);
        assert_ne!(fork["client_metadata"]["session_id"], root_session);
        assert_eq!(fork["input"].as_array().map(Vec::len), Some(1));
        assert_eq!(fork["input"][0]["content"][0]["text"], "fork prompt");
        branch_started
            .send(())
            .map_err(|()| eyre!("branch signal receiver dropped"))?;
        send_final(&mut branch, "resp-fork").await?;
        root_task.await??;
        Result::<()>::Ok(())
    });

    let workspace = temporary_workspace("historical-fork")?;
    let responses = Responses::builder().websocket_url(endpoint).build();
    let (agent, root_events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("model-test")
        .build()?;
    let first = agent
        .prompt(Prompt::new("first prompt"))
        .await?
        .result()
        .await?;
    agent.prompt("second prompt").await?.result().await?;

    let mainline = agent.prompt("continue mainline").await?;
    root_started_rx
        .await
        .map_err(|_| eyre!("root request was not observed"))?;
    let (fork, fork_events) = agent.fork_from(&first).await?;
    let branch = fork.prompt("fork prompt").await?;
    let (mainline, branch) = tokio::join!(mainline.result(), branch.result());
    assert_eq!(mainline?.final_message, "done");
    assert_eq!(branch?.final_message, "done");

    drop((agent, fork, root_events, fork_events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn per_agent_tool_factory_binds_recursive_forks_to_the_invoking_driver() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        let warmup = next_json(&mut root).await?;
        let lineage = warmup["prompt_cache_key"].clone();
        let root_session = warmup["client_metadata"]["session_id"].clone();
        send_warmup(&mut root, "resp-warmup").await?;
        let root_turn = next_json(&mut root).await?;
        assert_eq!(root_turn["previous_response_id"], "resp-warmup");
        send_final(&mut root, "resp-root").await?;

        let (stream, _) = listener.accept().await?;
        let mut child = accept_async(stream).await?;
        let child_turn = next_json(&mut child).await?;
        let child_session = child_turn["client_metadata"]["session_id"].clone();
        assert_eq!(child_turn["previous_response_id"], "resp-root");
        assert_eq!(child_turn["prompt_cache_key"], lineage);
        assert_ne!(child_session, root_session);
        send_final(&mut child, "resp-child").await?;

        let (stream, _) = listener.accept().await?;
        let mut grandchild = accept_async(stream).await?;
        let grandchild_turn = next_json(&mut grandchild).await?;
        assert_eq!(grandchild_turn["previous_response_id"], "resp-child");
        assert_eq!(grandchild_turn["prompt_cache_key"], lineage);
        assert_ne!(
            grandchild_turn["client_metadata"]["session_id"],
            child_session
        );
        send_final(&mut grandchild, "resp-grandchild").await
    });

    let (handles, mut received_handles) = tokio::sync::mpsc::unbounded_channel::<AgentHandle>();
    let workspace = temporary_workspace("recursive-fork-tools")?;
    let responses = Responses::builder().websocket_url(endpoint).build();
    let (root, root_events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("model-test")
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
async fn clean_spawn_reuses_an_explicit_cache_key_without_history_or_lineage() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        let root_warmup = next_json(&mut root).await?;
        assert_eq!(root_warmup["prompt_cache_key"], "shared-private-prefix");
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
        assert_ne!(child_session, "root-lineage");
        assert_eq!(child_warmup["prompt_cache_key"], "shared-private-prefix");
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
    let responses = Responses::builder().websocket_url(endpoint).build();
    let (root, root_events) = Nanocodex::builder("private-test-key")
        .instructions("shared private configuration")
        .thinking(Thinking::Low)
        .responses(responses)
        .session_id("root-lineage")
        .prompt_cache_key("shared-private-prefix")
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
    let responses = Responses::builder().websocket_url(endpoint).build();
    let builder = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .prompt_cache_key("shared-prefix")
        .shared_prompt_cache();

    let (first, mut first_events) = builder.clone().build()?;
    let first_session = first.session_id().to_owned();
    first.prompt("first turn").await?.result().await?;
    drop(first);
    let mut first_warmup_source = None;
    while let Some(event) = first_events.recv().await {
        if event.kind == nanocodex_core::AgentEventKind::ModelWarmupCompleted {
            first_warmup_source = Some(event.decode_payload::<Value>()?["source"].clone());
        }
    }

    let (second, mut second_events) = builder.build()?;
    assert_ne!(second.session_id(), first_session);
    second.prompt("second turn").await?.result().await?;
    drop(second);
    let mut second_warmup_source = None;
    while let Some(event) = second_events.recv().await {
        if event.kind == nanocodex_core::AgentEventKind::ModelWarmupCompleted {
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

#[tokio::test]
async fn missing_stored_checkpoint_replays_local_history_once() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut root = accept_async(stream).await?;
        assert_warmup(&next_json(&mut root).await?);
        send_warmup(&mut root, "resp-warmup").await?;
        let first = next_json(&mut root).await?;
        send_final(&mut root, "resp-first").await?;

        let (stream, _) = listener.accept().await?;
        let mut branch = accept_async(stream).await?;
        let checkpoint = next_json(&mut branch).await?;
        assert_eq!(checkpoint["previous_response_id"], "resp-first");
        assert_eq!(checkpoint["input"].as_array().map(Vec::len), Some(1));
        send_json(
            &mut branch,
            json!({
                "type": "error",
                "error": {
                    "code": "previous_response_not_found",
                    "message": "checkpoint expired"
                }
            }),
        )
        .await?;

        let replay = next_json(&mut branch).await?;
        assert!(replay.get("previous_response_id").is_none());
        assert_eq!(replay["store"], true);
        assert_eq!(replay["input"][0]["type"], "additional_tools");
        assert_eq!(replay["input"][1]["role"], "developer");
        let replay_text = replay.to_string();
        assert!(replay_text.contains("root prompt"));
        assert!(replay_text.contains("branch after eviction"));
        assert!(
            replay["input"]
                .as_array()
                .is_some_and(|items| items.len() > 4)
        );
        send_final(&mut branch, "resp-replayed").await?;
        drop((root, first));
        Result::<()>::Ok(())
    });

    let workspace = temporary_workspace("checkpoint-miss")?;
    let responses = Responses::builder().websocket_url(endpoint).build();
    let (agent, root_events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("model-test")
        .build()?;
    let first = agent
        .prompt(Prompt::new("root prompt"))
        .await?
        .result()
        .await?;
    let (fork, mut fork_events) = agent.fork_from(&first).await?;
    let branch = fork.prompt("branch after eviction").await?;
    assert_eq!(branch.result().await?.final_message, "done");

    drop((agent, fork, root_events));
    let mut observed_checkpoint_retry = false;
    while let Some(event) = fork_events.recv().await {
        if event.kind == nanocodex_core::AgentEventKind::ModelAttemptRetrying {
            let payload = event.decode_payload::<Value>()?;
            observed_checkpoint_retry = payload["error_class"] == "checkpoint_missing"
                && payload["replay_mode"] == "full_history"
                && payload["opens_new_socket"] == false;
        }
    }
    assert!(observed_checkpoint_retry);
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn serialized_session_and_codex_rollout_share_committed_history() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut original = accept_async(stream).await?;
        let warmup = next_json(&mut original).await?;
        assert_eq!(warmup["prompt_cache_key"], "durable-cache");
        send_warmup(&mut original, "resp-warmup").await?;
        let first = next_json(&mut original).await?;
        assert_eq!(first["previous_response_id"], "resp-warmup");
        send_final(&mut original, "resp-first").await?;

        let (stream, _) = listener.accept().await?;
        let mut resumed = accept_async(stream).await?;
        let replay = next_json(&mut resumed).await?;
        assert!(replay.get("previous_response_id").is_none());
        assert_eq!(replay["prompt_cache_key"], "durable-cache");
        assert_eq!(replay["input"][0]["type"], "additional_tools");
        assert_eq!(replay["input"][1]["role"], "developer");
        assert_eq!(
            replay["input"][1]["content"][0]["text"],
            "durable instructions"
        );
        let replay_text = replay.to_string();
        assert!(replay_text.contains("first prompt"));
        assert!(replay_text.contains("resume prompt"));
        send_final(&mut resumed, "resp-resumed").await?;
        Result::<()>::Ok(())
    });

    let workspace = temporary_workspace("serialized-resume")?;
    let rollout_home = temporary_workspace("serialized-resume-rollout")?;
    let responses = Responses::builder().websocket_url(endpoint.clone()).build();
    let (agent, events) = Nanocodex::builder("test-key")
        .instructions("durable instructions")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .session_id("019c0d31-c308-7d91-bff4-5dca82d15ac6")
        .prompt_cache_key("durable-cache")
        .rollout(RolloutConfig::new(&rollout_home))
        .build()?;
    let rollout_path = agent
        .rollout()
        .ok_or_else(|| eyre!("rollout was not configured"))?
        .path()
        .to_path_buf();
    let first = agent.prompt("first prompt").await?.result().await?;
    let encoded = serde_json::to_vec(&first.snapshot())?;
    agent.flush_rollout().await?;
    let snapshot_json = serde_json::from_slice::<Value>(&encoded)?;
    let rollout_history = std::fs::read_to_string(rollout_path)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<serde_json::Result<Vec<_>>>()?
        .into_iter()
        .filter(|line| line["type"] == "response_item")
        .map(|line| line["payload"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        snapshot_json["history"].as_array(),
        Some(&rollout_history),
        "native snapshots and Codex rollouts must project the same committed history"
    );
    let snapshot: SessionSnapshot = serde_json::from_slice(&encoded)?;
    drop((agent, events, first));

    let mut unsupported: Value = serde_json::from_slice(&encoded)?;
    unsupported["version"] = json!(2);
    let unsupported: SessionSnapshot = serde_json::from_value(unsupported)?;
    let unsupported = Nanocodex::builder("test-key")
        .responses(Responses::builder().websocket_url(endpoint.clone()).build())
        .resume(unsupported)
        .build();
    assert!(matches!(
        unsupported,
        Err(NanocodexError::InvalidSessionSnapshot(message))
            if message.contains("unsupported format version")
    ));

    let incompatible = Nanocodex::builder("test-key")
        .instructions("changed instructions")
        .thinking(Thinking::Low)
        .responses(Responses::builder().websocket_url(endpoint.clone()).build())
        .resume(snapshot.clone())
        .build();
    assert!(matches!(
        incompatible,
        Err(NanocodexError::InvalidSessionSnapshot(message))
            if message.contains("instructions or tool definitions")
    ));
    let other_workspace = temporary_workspace("serialized-resume-other")?;
    let incompatible = Nanocodex::builder("test-key")
        .instructions("durable instructions")
        .thinking(Thinking::Low)
        .workspace(&other_workspace)
        .responses(Responses::builder().websocket_url(endpoint.clone()).build())
        .resume(snapshot.clone())
        .build();
    assert!(matches!(
        incompatible,
        Err(NanocodexError::WorkspaceChanged { .. })
    ));
    std::fs::remove_dir_all(other_workspace)?;
    let incompatible = Nanocodex::builder("test-key")
        .instructions("durable instructions")
        .thinking(Thinking::Low)
        .responses(Responses::builder().websocket_url(endpoint.clone()).build())
        .prompt_cache_key("changed-cache")
        .resume(snapshot.clone())
        .build();
    assert!(matches!(
        incompatible,
        Err(NanocodexError::InvalidSessionSnapshot(message))
            if message.contains("prompt cache key")
    ));

    let responses = Responses::builder().websocket_url(endpoint).build();
    let (resumed, resumed_events) = Nanocodex::builder("test-key")
        .instructions("durable instructions")
        .thinking(Thinking::Low)
        .responses(responses)
        .session_id("resumed-runtime")
        .resume(snapshot)
        .build()?;
    assert_eq!(resumed_events.request_id(), "resumed-runtime");
    assert_eq!(
        resumed
            .prompt("resume prompt")
            .await?
            .result()
            .await?
            .final_message,
        "done"
    );

    drop((resumed, resumed_events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    std::fs::remove_dir_all(rollout_home)?;
    Ok(())
}

#[tokio::test]
async fn serialized_session_resumes_over_ephemeral_https() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let first = next_http_json(&listener).await?;
        assert_eq!(first.body["store"], false);
        assert!(first.body.get("previous_response_id").is_none());
        assert!(first.body.to_string().contains("first prompt"));
        send_http_final(first.stream, "resp-first").await?;

        let resumed = next_http_json(&listener).await?;
        assert_eq!(resumed.body["store"], false);
        assert!(resumed.body.get("previous_response_id").is_none());
        let replay = resumed.body.to_string();
        assert!(replay.contains("first prompt"));
        assert!(replay.contains("done"));
        assert!(replay.contains("resume prompt"));
        send_http_final(resumed.stream, "resp-resumed").await
    });

    let workspace = temporary_workspace("serialized-resume-https")?;
    let responses = Responses::builder()
        .transport(ResponsesTransport::Https)
        .store(false)
        .api_base_url(endpoint.clone())
        .build();
    let (agent, events) = Nanocodex::builder("test-key")
        .instructions("durable instructions")
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .responses(responses)
        .prompt_cache_key("durable-cache")
        .build()?;
    let first = agent.prompt("first prompt").await?.result().await?;
    let snapshot = serde_json::from_slice(&serde_json::to_vec(&first.snapshot())?)?;
    drop((agent, events, first));

    let responses = Responses::builder()
        .transport(ResponsesTransport::Https)
        .store(false)
        .api_base_url(endpoint)
        .build();
    let (resumed, resumed_events) = Nanocodex::builder("test-key")
        .instructions("durable instructions")
        .thinking(Thinking::Low)
        .responses(responses)
        .resume(snapshot)
        .build()?;
    assert_eq!(
        resumed
            .prompt("resume prompt")
            .await?
            .result()
            .await?
            .final_message,
        "done"
    );

    drop((resumed, resumed_events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock HTTPS Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

fn assert_warmup(warmup: &Value) {
    assert_eq!(warmup["store"], true);
    assert_eq!(warmup["generate"], false);
    assert_eq!(warmup["stream"], true);
    assert_eq!(warmup["parallel_tool_calls"], false);
    assert_eq!(warmup["prompt_cache_key"], "model-test");
    assert_eq!(warmup["input"].as_array().map(Vec::len), Some(2));
    assert_eq!(warmup["input"][0]["type"], "additional_tools");
    assert_eq!(warmup["input"][0]["role"], "developer");
    assert_eq!(warmup["input"][0]["tools"][0]["type"], "custom");
    assert_eq!(warmup["input"][0]["tools"][0]["name"], "exec");
    assert!(
        warmup["input"][0]["tools"][0]["description"]
            .as_str()
            .is_some_and(|description| description.contains("`web__run`"))
    );
    assert_eq!(warmup["input"][0]["tools"][1]["type"], "function");
    assert_eq!(warmup["input"][0]["tools"][1]["name"], "wait");
    assert_eq!(warmup["input"][1]["role"], "developer");
    assert!(warmup.get("tools").is_none());
    assert!(warmup.get("instructions").is_none());
    assert!(warmup.get("context_management").is_none());
    assert!(warmup["reasoning"].get("mode").is_none());
    assert_eq!(
        warmup["client_metadata"]["ws_request_header_x_openai_internal_codex_responses_lite"],
        "true"
    );
}

async fn run_model(endpoint: &str, workspace: &Path, instruction: &str) -> Result<String> {
    let task = Prompt::new(instruction);
    let responses = Responses::builder().websocket_url(endpoint).build();
    let (agent, events) = Nanocodex::builder("test-key")
        .thinking(Thinking::Low)
        .workspace(workspace)
        .responses(responses)
        .session_id("model-test")
        .build()?;
    let turn = agent.prompt(task).await?;
    drop(agent);
    let mut output = Vec::new();
    let (event_result, turn_result) = tokio::join!(events.write_jsonl(&mut output), turn.result());
    event_result?;
    turn_result?;
    Ok(String::from_utf8(output)?)
}

async fn send_warmup<S>(socket: &mut WebSocketStream<S>, response_id: &str) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_json(
        socket,
        json!({
            "type": "response.completed",
            "response": { "id": response_id, "usage": null }
        }),
    )
    .await
}

async fn send_final<S>(socket: &mut WebSocketStream<S>, response_id: &str) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_json(
        socket,
        completed_response(
            response_id,
            &[json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "done" }]
            })],
        ),
    )
    .await
}

async fn send_assistant_output<S>(
    socket: &mut WebSocketStream<S>,
    output_index: u32,
    item_id: &str,
    phase: &str,
    text: &str,
) -> Result<Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let completed = json!({
        "id": item_id,
        "type": "message",
        "role": "assistant",
        "status": "completed",
        "phase": phase,
        "content": [{ "type": "output_text", "text": text }]
    });
    send_json(
        socket,
        json!({
            "type": "response.output_item.added",
            "output_index": output_index,
            "item": {
                "id": item_id,
                "type": "message",
                "role": "assistant",
                "status": "in_progress",
                "phase": phase,
                "content": []
            }
        }),
    )
    .await?;
    send_json(
        socket,
        json!({
            "type": "response.output_text.delta",
            "output_index": output_index,
            "content_index": 0,
            "delta": text
        }),
    )
    .await?;
    send_json(
        socket,
        json!({
            "type": "response.output_item.done",
            "output_index": output_index,
            "item": completed.clone()
        }),
    )
    .await?;
    Ok(completed)
}

fn completed_response(response_id: &str, output: &[Value]) -> Value {
    completed_response_with_usage(response_id, output, 12)
}

fn completed_response_with_usage(response_id: &str, output: &[Value], total_tokens: u64) -> Value {
    json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "status": "completed",
            "output": output,
            "usage": {
                "input_tokens": 10,
                "input_tokens_details": { "cached_tokens": 5 },
                "output_tokens": 2,
                "output_tokens_details": { "reasoning_tokens": 1 },
                "total_tokens": total_tokens
            }
        }
    })
}

struct CapturedHttpRequest {
    stream: TcpStream,
    headers: String,
    body: Value,
}

async fn next_http_json(listener: &TcpListener) -> Result<CapturedHttpRequest> {
    let (mut stream, _) = listener.accept().await?;
    let mut bytes = Vec::with_capacity(4096);
    let header_end = loop {
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let read = stream.read_buf(&mut bytes).await?;
        if read == 0 {
            return Err(eyre!("HTTPS test client closed before request headers"));
        }
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec())?.to_ascii_lowercase();
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .map(str::trim)
        .ok_or_else(|| eyre!("HTTPS test request omitted Content-Length"))?
        .parse::<usize>()?;
    while bytes.len() - header_end < content_length {
        let read = stream.read_buf(&mut bytes).await?;
        if read == 0 {
            return Err(eyre!("HTTPS test client closed before request body"));
        }
    }
    let body = serde_json::from_slice(&bytes[header_end..header_end + content_length])?;
    Ok(CapturedHttpRequest {
        stream,
        headers,
        body,
    })
}

async fn send_http_final(mut stream: TcpStream, response_id: &str) -> Result<()> {
    let event = completed_response(
        response_id,
        &[json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "done" }]
        })],
    );
    let body = format!("data: {event}\n\ndata: [DONE]\n\n");
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

async fn next_json<S>(socket: &mut WebSocketStream<S>) -> Result<Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let message = socket
            .next()
            .await
            .ok_or_else(|| eyre!("client closed before sending a request"))??;
        if let Message::Text(text) = message {
            return Ok(serde_json::from_str(text.as_str())?);
        }
    }
}

async fn send_json<S>(socket: &mut WebSocketStream<S>, value: Value) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket.send(Message::Text(value.to_string().into())).await?;
    Ok(())
}

fn temporary_workspace(label: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "nanocodex-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
