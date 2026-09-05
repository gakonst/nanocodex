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
