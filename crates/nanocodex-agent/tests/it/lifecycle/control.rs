use super::*;

#[tokio::test]
async fn forking_before_a_completed_turn_is_typed() {
    let (agent, events) = Nanocodex::builder(test_openai()).build().unwrap();
    let Err(error) = agent.fork().await else {
        panic!("fork unexpectedly succeeded");
    };
    assert!(matches!(error, NanocodexError::ForkBeforeCompletedTurn));
    drop((agent, events));
}

#[tokio::test]
async fn live_snapshot_requires_a_safe_boundary_and_does_not_change_parent() {
    let (retained, _retained_attempts) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: retained.clone(),
        })
        .build()
        .unwrap();
    let tools = Tools::builder().without_defaults().build().unwrap();
    let (agent, events) = Nanocodex::builder(openai).tools(tools).build().unwrap();
    assert!(matches!(
        agent.snapshot().await,
        Err(NanocodexError::ForkBeforeCompletedTurn)
    ));

    let first = agent
        .prompt("first request")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let first_snapshot = serde_json::to_value(first.snapshot().unwrap()).unwrap();
    let copied = serde_json::to_value(agent.snapshot().await.unwrap()).unwrap();
    assert_eq!(copied, first_snapshot);
    assert_eq!(
        serde_json::to_value(agent.snapshot().await.unwrap()).unwrap(),
        copied
    );

    // A second turn must still be accepted by the unchanged parent, and the
    // exported boundary must advance rather than inheriting its old value.
    let second = agent
        .prompt("second request")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let second_snapshot = serde_json::to_value(second.snapshot().unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(agent.snapshot().await.unwrap()).unwrap(),
        second_snapshot
    );
    assert_ne!(copied["history"], second_snapshot["history"]);
    drop((agent, events));
}

#[tokio::test]
async fn steering_without_an_active_turn_is_typed() {
    let openai = OpenAi::builder("test")
        .service(|| PendingService)
        .build()
        .unwrap();
    let (agent, events) = Nanocodex::builder(openai).build().unwrap();
    let turn = agent.prompt("wait for cancellation").await.unwrap();
    let control = turn.control();
    turn.cancel().await.unwrap();
    assert!(matches!(
        turn.result().await,
        Err(NanocodexError::TurnCancelled)
    ));
    let Err(error) = control.steer("additional direction").await else {
        panic!("steer unexpectedly succeeded");
    };
    assert!(matches!(error, NanocodexError::TurnNotSteerable));
    drop((agent, events));
}

#[tokio::test]
async fn caller_service_factory_supports_cancellation() {
    let builds = Arc::new(AtomicU64::new(0));
    let factory_builds = Arc::clone(&builds);
    let openai = OpenAi::builder("test")
        .service(move || {
            factory_builds.fetch_add(1, Ordering::Relaxed);
            PendingService
        })
        .build()
        .unwrap();
    let (agent, events) = Nanocodex::builder(openai).build().unwrap();
    let turn = agent.prompt("keep running").await.unwrap();

    turn.cancel().await.unwrap();
    assert!(matches!(
        turn.result().await,
        Err(NanocodexError::TurnCancelled)
    ));
    assert_eq!(builds.load(Ordering::Relaxed), 2);
    drop((agent, events));
}

#[tokio::test]
async fn cancel_on_admission_never_dispatches_model_work() {
    let openai = OpenAi::builder("test")
        .service(|| NeverCalled)
        .build()
        .unwrap();
    let (agent, mut events) = Nanocodex::builder(openai).build().unwrap();
    let turn = agent
        .prompt(PromptRequest::new("cancel before work").cancel_on_admission())
        .await
        .unwrap();

    assert!(matches!(
        turn.result().await,
        Err(NanocodexError::TurnCancelled)
    ));
    let mut observed = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("cancelled admission should publish its terminal events")
            .expect("event stream should remain open");
        let terminal = event.kind.is_terminal();
        observed.push(event.kind);
        if terminal {
            break;
        }
    }
    assert!(observed.contains(&nanocodex_agent::events::AgentEventKind::RunStarted));
    assert!(observed.contains(&nanocodex_agent::events::AgentEventKind::RunFailed));
    assert!(!observed.contains(&nanocodex_agent::events::AgentEventKind::ModelAttemptStarted));
    drop((agent, events));
}

#[tokio::test]
async fn turn_result_does_not_wait_for_attempt_event_producers_to_close() {
    let (retained, mut retained_attempts) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: retained.clone(),
        })
        .build()
        .unwrap();
    let tools = Tools::builder().without_defaults().build().unwrap();
    let (agent, mut events) = Nanocodex::builder(openai).tools(tools).build().unwrap();
    let turn = agent.prompt("reply with done").await.unwrap();
    let retained_attempt = tokio::time::timeout(Duration::from_secs(1), retained_attempts.recv())
        .await
        .expect("the generation attempt should start")
        .expect("the service should retain its generation attempt clone");

    let result = tokio::time::timeout(Duration::from_secs(1), turn.result())
        .await
        .expect("a completed result must not wait for event producers to close")
        .unwrap();
    assert_eq!(result.final_message(), "done");
    assert!(matches!(
        retained_attempt.kind(),
        nanocodex_agent::transport::ResponsesAttemptKind::Generation
    ));

    let mut observed_start = false;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("root events should remain independently consumable")
            .expect("the root stream should remain open");
        match event.kind {
            nanocodex_agent::events::AgentEventKind::RunStarted => observed_start = true,
            nanocodex_agent::events::AgentEventKind::RunCompleted => {
                assert!(
                    observed_start,
                    "the terminal event must follow the turn's start event"
                );
                break;
            }
            _ => {}
        }
    }
    drop((retained_attempt, agent, events));
}

#[tokio::test]
async fn adapter_developer_context_is_visible_at_safe_model_boundaries() {
    let (retained, mut retained_attempts) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: retained.clone(),
        })
        .build()
        .unwrap();
    let tools = Tools::builder().without_defaults().build().unwrap();
    let (agent, events) = Nanocodex::builder(openai).tools(tools).build().unwrap();

    let initial = agent
        .append_developer_message("adapter session started")
        .await
        .unwrap();
    assert!(initial.history().iter().any(|item| matches!(
        item,
        ResponseItem::Message {
            role: MessageRole::Developer,
            content,
            ..
        } if content.iter().any(|part| matches!(
            part,
            ContentItem::InputText { text } if text.as_ref() == "adapter session started"
        ))
    )));
    assert!(!initial.workspace().is_empty());

    agent
        .prompt("first request")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let first = retained_attempts.recv().await.unwrap();
    let first_items = first.input_items().collect::<Vec<_>>();
    assert!(first_items.iter().any(|item| matches!(
        item,
        ResponseItem::Message {
            role: MessageRole::Developer,
            content,
            ..
        } if content.iter().any(|part| matches!(
            part,
            ContentItem::InputText { text } if text.as_ref() == "adapter session started"
        ))
    )));
    assert!(matches!(
        first_items.last(),
        Some(ResponseItem::Message {
            role: MessageRole::User,
            ..
        })
    ));

    let completed = agent
        .append_developer_message("adapter session ended")
        .await
        .unwrap();
    assert!(completed.history().iter().any(|item| matches!(
        item,
        ResponseItem::Message {
            role: MessageRole::Developer,
            content,
            ..
        } if content.iter().any(|part| matches!(
            part,
            ContentItem::InputText { text } if text.as_ref() == "adapter session ended"
        ))
    )));

    agent.shutdown().await.unwrap();
    drop((agent, events));
}

#[tokio::test]
async fn dropping_every_command_handle_cancels_an_in_flight_attempt() {
    let started = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let service_started = Arc::clone(&started);
    let service_dropped = Arc::clone(&dropped);
    let openai = OpenAi::builder("test")
        .service(move || DropPendingService {
            started: Arc::clone(&service_started),
            dropped: Arc::clone(&service_dropped),
        })
        .build()
        .unwrap();
    let (agent, events) = Nanocodex::builder(openai).build().unwrap();
    drop(events);
    let turn = agent.prompt("keep running").await.unwrap();

    tokio::time::timeout(Duration::from_secs(1), async {
        while !started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the model attempt should start");

    drop(turn);
    drop(agent);

    tokio::time::timeout(Duration::from_secs(1), async {
        while !dropped.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("closing the command channel should drop the in-flight attempt");
}

#[tokio::test]
async fn accepts_a_caller_service_factory_for_future_children() {
    let openai = OpenAi::builder("test")
        .service(|| NeverCalled)
        .build()
        .unwrap();
    let (agent, events) = Nanocodex::builder(openai).build().unwrap();
    drop((agent, events));
}

#[tokio::test]
async fn caller_service_factory_supports_clean_spawn() {
    let (handles, mut received_handles) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test")
        .service(|| NeverCalled)
        .build()
        .unwrap();
    let (agent, events) = Nanocodex::builder(openai)
        .tools_factory(move |handle| {
            drop(handles.send(handle));
            Tools::builder().without_defaults().build()
        })
        .build()
        .unwrap();
    let handle = received_handles.recv().await.unwrap();

    let (child, child_events) = handle.spawn().await.unwrap();
    drop((child, child_events, agent, events));
}

#[tokio::test]
async fn owning_agent_supports_clean_spawn() {
    let openai = OpenAi::builder("test")
        .service(|| NeverCalled)
        .build()
        .unwrap();
    let (agent, events) = Nanocodex::builder(openai).build().unwrap();

    let (sibling, sibling_events) = agent.spawn().await.unwrap();

    drop((sibling, sibling_events, agent, events));
}

#[tokio::test]
async fn an_agent_handle_does_not_keep_its_driver_alive() {
    let (handles, mut received_handles) = mpsc::unbounded_channel();
    let (agent, events) = Nanocodex::builder(test_openai())
        .tools_factory(move |handle| {
            drop(handles.send(handle));
            Tools::builder().without_defaults().build()
        })
        .build()
        .unwrap();
    let handle = received_handles.recv().await.unwrap();

    drop(agent);
    let Err(error) = handle.spawn().await else {
        panic!("agent handle unexpectedly kept its driver alive");
    };
    assert!(matches!(error, NanocodexError::AgentStopped));
    let Err(error) = handle.fork().await else {
        panic!("agent handle unexpectedly kept its driver alive");
    };
    assert!(matches!(error, NanocodexError::AgentStopped));
    drop(events);
}

#[test]
fn building_requires_a_tokio_runtime() {
    assert!(matches!(
        Nanocodex::builder(test_openai()).build(),
        Err(NanocodexError::TokioRuntimeUnavailable)
    ));
}

#[tokio::test]
async fn late_function_output_without_opted_in_staged_call_fails_closed() {
    let (agent, events) = Nanocodex::builder(test_openai()).build().unwrap();
    let output = nanocodex_oai_api::responses::FunctionOutputBody::Text("done".into());
    assert!(matches!(
        agent
            .submit_late_function_output("job-1", output.clone(), "op-1")
            .await,
        Err(NanocodexError::InvalidRequest(_))
    ));
    assert!(matches!(
        agent.submit_late_function_output("", output, "op-1").await,
        Err(NanocodexError::InvalidRequest(_))
    ));
    drop((agent, events));
}

#[derive(Clone)]
struct WakeGateService {
    attempts: mpsc::UnboundedSender<ResponsesAttempt>,
    gate: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
}

impl Service<ResponsesAttempt> for WakeGateService {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future =
        Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        let gate = if matches!(request.kind(), ResponsesAttemptKind::Generation) {
            self.gate.lock().unwrap().take()
        } else {
            None
        };
        if matches!(request.kind(), ResponsesAttemptKind::Generation) {
            self.attempts.send(request.clone()).unwrap();
        }
        let (unused, keep) = mpsc::unbounded_channel();
        let mut completed = RetainingCompletedService { retained: unused };
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.await.unwrap();
            }
            let _keep = keep;
            completed.call(request).await
        })
    }
}

#[tokio::test]
async fn late_terminal_wakes_without_prompt_and_driver_polls_commands() {
    let (retained, _retained_attempts) = mpsc::unbounded_channel();
    let first = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: retained.clone(),
        })
        .build()
        .unwrap();
    let (agent, events) = Nanocodex::builder(first)
        .tools(Tools::builder().without_defaults().build().unwrap())
        .build()
        .unwrap();
    agent
        .prompt("initial")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let mut snapshot = serde_json::to_value(agent.snapshot().await.unwrap()).unwrap();
    drop((agent, events));
    snapshot["unreal_function_outputs"] = serde_json::json!(true);
    let history = snapshot["history"].as_array_mut().unwrap();
    history.push(serde_json::json!({
        "type":"function_call", "call_id":"job-1", "name":"job", "arguments":"{}"
    }));
    history.push(serde_json::json!({
        "type":"function_call_output", "call_id":"job-1",
        "output":"Tool call is still running. Its result arrives in a later turn: continue with independent work, or end your turn to wait for it."
    }));
    let snapshot = serde_json::from_value(snapshot).unwrap();
    let (attempts, mut observed) = mpsc::unbounded_channel();
    let (release, gate) = tokio::sync::oneshot::channel();
    let gate = Arc::new(std::sync::Mutex::new(Some(gate)));
    let openai = OpenAi::builder("test")
        .service(move || WakeGateService {
            attempts: attempts.clone(),
            gate: Arc::clone(&gate),
        })
        .build()
        .unwrap();
    let (agent, events) = Nanocodex::builder(openai)
        .resume(snapshot)
        .tools(Tools::builder().without_defaults().build().unwrap())
        .build()
        .unwrap();
    let receipt = agent
        .submit_late_function_output(
            "job-1",
            nanocodex_oai_api::responses::FunctionOutputBody::Text("finished".into()),
            "operation-1",
        )
        .await
        .unwrap();
    assert!(!receipt.replayed);
    let wake = tokio::time::timeout(Duration::from_secs(5), observed.recv())
        .await
        .unwrap()
        .unwrap();
    let wake_items = wake
        .input_items()
        .map(|item| serde_json::to_value(item).unwrap())
        .collect::<Vec<_>>();
    assert!(
        wake_items
            .iter()
            .any(|item| item["type"] == "function_call_output" && item["output"] == "finished")
    );
    // The gate holds the model call; this must not wait for it.
    tokio::time::timeout(Duration::from_secs(5), agent.context())
        .await
        .unwrap()
        .unwrap();
    let next = tokio::time::timeout(Duration::from_secs(1), agent.prompt("after wake"))
        .await
        .unwrap()
        .unwrap();
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), next.result())
        .await
        .unwrap()
        .unwrap();
    let second = observed.recv().await.unwrap();
    assert!(
        second
            .input_items()
            .any(|item| serde_json::to_string(item).unwrap().contains("after wake"))
    );
    drop((agent, events));
}

#[tokio::test]
async fn invalid_later_cohort_member_never_checkpoints_or_wakes_valid_prefix() {
    let (seed_attempts, _seed_rx) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: seed_attempts.clone(),
        })
        .build()
        .unwrap();
    let (seed, seed_events) = Nanocodex::builder(openai)
        .tools(Tools::builder().without_defaults().build().unwrap())
        .build()
        .unwrap();
    seed.prompt("seed").await.unwrap().result().await.unwrap();
    let mut snapshot = serde_json::to_value(seed.snapshot().await.unwrap()).unwrap();
    seed.shutdown().await.unwrap();
    drop(seed_events);
    snapshot["unreal_function_outputs"] = serde_json::json!(true);
    snapshot["history"].as_array_mut().unwrap().extend([
        serde_json::json!({
            "type":"function_call", "call_id":"valid-call", "name":"job", "arguments":"{}"
        }),
        serde_json::json!({
            "type":"function_call_output", "call_id":"valid-call", "output":
            "Tool call is still running. Its result arrives in a later turn: continue with independent work, or end your turn to wait for it."
        }),
    ]);
    let (attempts, mut observed) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: attempts.clone(),
        })
        .build()
        .unwrap();
    let (agent, events) = Nanocodex::builder(openai)
        .resume(serde_json::from_value(snapshot).unwrap())
        .tools(Tools::builder().without_defaults().build().unwrap())
        .build()
        .unwrap();
    let error = agent
        .submit_late_function_outputs(vec![
            nanocodex_agent::LateFunctionOutput {
                call_id: "valid-call".into(),
                operation_id: "valid-operation".into(),
                output: nanocodex_oai_api::responses::FunctionOutputBody::Text(
                    "valid result".into(),
                ),
            },
            nanocodex_agent::LateFunctionOutput {
                call_id: "unknown-call".into(),
                operation_id: "unknown-operation".into(),
                output: nanocodex_oai_api::responses::FunctionOutputBody::Text(
                    "invalid result".into(),
                ),
            },
        ])
        .await
        .unwrap_err();
    assert!(
        matches!(error, NanocodexError::InvalidRequest(_)),
        "{error}"
    );
    let saved = serde_json::to_value(agent.snapshot().await.unwrap()).unwrap();
    assert!(saved["pending_late_wake"].is_null());
    assert!(!saved["history"].to_string().contains("valid result"));
    assert!(
        observed.try_recv().is_err(),
        "no partial prefix may reach the provider"
    );
    agent.shutdown().await.unwrap();
    drop(events);
}

#[tokio::test]
async fn two_late_terminal_outputs_share_one_promptless_wake() {
    let (retained, _retained_attempts) = mpsc::unbounded_channel();
    let first = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: retained.clone(),
        })
        .build()
        .unwrap();
    let (seed, seed_events) = Nanocodex::builder(first)
        .tools(Tools::builder().without_defaults().build().unwrap())
        .build()
        .unwrap();
    seed.prompt("initial")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let mut snapshot = serde_json::to_value(seed.snapshot().await.unwrap()).unwrap();
    drop((seed, seed_events));
    snapshot["unreal_function_outputs"] = serde_json::json!(true);
    let history = snapshot["history"].as_array_mut().unwrap();
    for call_id in ["job-1", "job-2"] {
        history.push(serde_json::json!({
            "type":"function_call", "call_id":call_id, "name":"job", "arguments":"{}"
        }));
        history.push(serde_json::json!({
            "type":"function_call_output", "call_id":call_id,
            "output":"Tool call is still running. Its result arrives in a later turn: continue with independent work, or end your turn to wait for it."
        }));
    }

    let (attempts, mut observed) = mpsc::unbounded_channel();
    let (release, gate) = tokio::sync::oneshot::channel();
    let gate = Arc::new(std::sync::Mutex::new(Some(gate)));
    let openai = OpenAi::builder("test")
        .service(move || WakeGateService {
            attempts: attempts.clone(),
            gate: Arc::clone(&gate),
        })
        .build()
        .unwrap();
    let (agent, events) = Nanocodex::builder(openai)
        .resume(serde_json::from_value(snapshot).unwrap())
        .tools(Tools::builder().without_defaults().build().unwrap())
        .build()
        .unwrap();
    let receipts = agent
        .submit_late_function_outputs(vec![
            nanocodex_agent::LateFunctionOutput {
                call_id: "job-1".into(),
                operation_id: "operation-1".into(),
                output: nanocodex_oai_api::responses::FunctionOutputBody::Text(
                    "first finished".into(),
                ),
            },
            nanocodex_agent::LateFunctionOutput {
                call_id: "job-2".into(),
                operation_id: "operation-2".into(),
                output: nanocodex_oai_api::responses::FunctionOutputBody::Text(
                    "second finished".into(),
                ),
            },
        ])
        .await
        .unwrap();
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0].call_id, "job-1");
    assert_eq!(receipts[1].call_id, "job-2");
    assert!(receipts.iter().all(|receipt| !receipt.replayed));

    let wake = tokio::time::timeout(Duration::from_secs(5), observed.recv())
        .await
        .unwrap()
        .unwrap();
    let wake_items = wake
        .input_items()
        .map(|item| serde_json::to_value(item).unwrap())
        .collect::<Vec<_>>();
    for (call_id, output) in [("job-1", "first finished"), ("job-2", "second finished")] {
        assert_eq!(
            wake_items
                .iter()
                .filter(|item| item["type"] == "function_call_output"
                    && item["call_id"] == call_id
                    && item["output"] == output)
                .count(),
            1,
            "the single prompt-less wake must see {call_id}'s terminal output"
        );
    }
    assert!(
        !wake_items
            .iter()
            .any(|item| item.to_string().contains("after cohort"))
    );

    // The first generation is still gated. The next generation must belong to
    // this queued user prompt, not a second prompt-less wake for job-2.
    tokio::time::timeout(Duration::from_secs(5), agent.context())
        .await
        .unwrap()
        .unwrap();
    let next = tokio::time::timeout(Duration::from_secs(5), agent.prompt("after cohort"))
        .await
        .unwrap()
        .unwrap();
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), next.result())
        .await
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(5), observed.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(second.input_items().any(|item| {
        serde_json::to_string(item)
            .unwrap()
            .contains("after cohort")
    }));
    assert!(observed.try_recv().is_err(), "unexpected additional wake");
    drop((agent, events));
}

#[tokio::test]
async fn late_terminal_submitted_during_active_turn_wakes_before_queued_prompt() {
    let (retained, _retained_attempts) = mpsc::unbounded_channel();
    let first = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: retained.clone(),
        })
        .build()
        .unwrap();
    let (agent, events) = Nanocodex::builder(first)
        .tools(Tools::builder().without_defaults().build().unwrap())
        .build()
        .unwrap();
    agent
        .prompt("initial")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let mut snapshot = serde_json::to_value(agent.snapshot().await.unwrap()).unwrap();
    drop((agent, events));
    snapshot["unreal_function_outputs"] = serde_json::json!(true);
    let history = snapshot["history"].as_array_mut().unwrap();
    history.push(serde_json::json!({
        "type":"function_call", "call_id":"job-1", "name":"job", "arguments":"{}"
    }));
    history.push(serde_json::json!({
        "type":"function_call_output", "call_id":"job-1",
        "output":"Tool call is still running. Its result arrives in a later turn: continue with independent work, or end your turn to wait for it."
    }));
    let (attempts, mut observed) = mpsc::unbounded_channel();
    let (release, gate) = tokio::sync::oneshot::channel();
    let gate = Arc::new(std::sync::Mutex::new(Some(gate)));
    let openai = OpenAi::builder("test")
        .service(move || WakeGateService {
            attempts: attempts.clone(),
            gate: Arc::clone(&gate),
        })
        .build()
        .unwrap();
    let (agent, events) = Nanocodex::builder(openai)
        .resume(serde_json::from_value(snapshot).unwrap())
        .tools(Tools::builder().without_defaults().build().unwrap())
        .build()
        .unwrap();
    let active = agent.prompt("active").await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), observed.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        first
            .input_items()
            .any(|item| serde_json::to_string(item).unwrap().contains("active"))
    );
    let queued = agent.prompt("queued").await.unwrap();
    let late_agent = agent.clone();
    let late = tokio::spawn(async move {
        late_agent
            .submit_late_function_output(
                "job-1",
                nanocodex_oai_api::responses::FunctionOutputBody::Text("finished".into()),
                "operation-1",
            )
            .await
    });
    tokio::task::yield_now().await;
    // A command received after the submission confirms its position in the driver queue.
    agent.context().await.unwrap();
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), active.result())
        .await
        .unwrap()
        .unwrap();
    let receipt = tokio::time::timeout(Duration::from_secs(5), late)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!receipt.replayed);
    let wake = tokio::time::timeout(Duration::from_secs(5), observed.recv())
        .await
        .unwrap()
        .unwrap();
    let wake_items = wake
        .input_items()
        .map(|item| serde_json::to_value(item).unwrap())
        .collect::<Vec<_>>();
    assert!(
        wake_items
            .iter()
            .any(|item| item["type"] == "function_call_output" && item["output"] == "finished")
    );
    assert!(
        !wake_items
            .iter()
            .any(|item| item.to_string().contains("queued"))
    );
    tokio::time::timeout(Duration::from_secs(5), queued.result())
        .await
        .unwrap()
        .unwrap();
    let after = observed.recv().await.unwrap();
    assert!(
        after
            .input_items()
            .any(|item| serde_json::to_string(item).unwrap().contains("queued"))
    );
    drop((agent, events));
}
