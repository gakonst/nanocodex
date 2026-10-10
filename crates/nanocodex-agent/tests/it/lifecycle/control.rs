use super::*;

#[tokio::test]
async fn forking_before_a_completed_turn_is_typed() {
    let (agent, events) = Nanocodex::builder(test_openai()).build().unwrap();
    let Err(error) = agent.fork(ForkRequest::latest()).await else {
        panic!("fork unexpectedly succeeded");
    };
    assert!(matches!(error, NanocodexError::ForkBeforeCompletedTurn));
    drop((agent, events));
}

#[tokio::test]
async fn live_checkpoint_tracks_safe_boundaries_and_does_not_change_parent() {
    let (retained, _retained_attempts) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: retained.clone(),
        })
        .build()
        .unwrap();
    let tools = Tools::builder().without_defaults().build().unwrap();
    let (agent, events) = Nanocodex::builder(openai).tools(tools).build().unwrap();
    // Before the first boundary a checkpoint keeps identity but no conversation.
    let empty = agent.checkpoint().await.unwrap();
    assert!(!empty.has_conversation());
    assert_eq!(empty.session_id(), agent.session_id());
    assert_eq!(empty.lineage(), &agent.session().lineage);
    assert!(matches!(
        agent.fork(ForkRequest::at(empty)).await,
        Err(NanocodexError::ForkBeforeCompletedTurn)
    ));

    let first = agent
        .prompt("first request")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let first_checkpoint = first.checkpoint().unwrap();
    assert!(first_checkpoint.has_conversation());
    assert!(first_checkpoint.turn_id().is_some());
    assert_eq!(first_checkpoint.turn_id(), first.turn_id());
    let first_snapshot = serde_json::to_value(conversation(&first_checkpoint)).unwrap();
    let copied = serde_json::to_value(conversation(&agent.checkpoint().await.unwrap())).unwrap();
    assert_eq!(copied, first_snapshot);
    assert_eq!(
        serde_json::to_value(conversation(&agent.checkpoint().await.unwrap())).unwrap(),
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
    let second_snapshot =
        serde_json::to_value(conversation(&second.checkpoint().unwrap())).unwrap();
    assert_eq!(
        serde_json::to_value(conversation(&agent.checkpoint().await.unwrap())).unwrap(),
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
        first_items.as_slice(),
        [
            ..,
            ResponseItem::Message {
                role: MessageRole::User,
                ..
            },
            ResponseItem::ConfigurationUpdate { .. }
        ]
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
    let Err(error) = handle.fork(ForkRequest::latest()).await else {
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

/// A recipe whose every turn completes without network access.
macro_rules! completing_openai {
    () => {{
        let (retained, retained_attempts) = mpsc::unbounded_channel();
        let openai = OpenAi::builder("test")
            .service(move || RetainingCompletedService {
                retained: retained.clone(),
            })
            .build()
            .unwrap();
        (openai, retained_attempts)
    }};
}

macro_rules! tool_free_agent {
    ($openai:expr) => {
        Nanocodex::builder($openai)
            .tools(Tools::builder().without_defaults().build().unwrap())
            .build()
            .unwrap()
    };
}

#[tokio::test]
async fn sessions_report_identity_lineage_capabilities_and_persistence() {
    use nanocodex_agent::{Capabilities, HarnessFamily, Mutability, Origin};

    let (openai, _attempts) = completing_openai!();
    let (agent, events) = tool_free_agent!(openai);
    let session = agent.session();
    assert_eq!(session.session_id, agent.session_id());
    assert_eq!(session.family, HarnessFamily::Codex);
    assert_eq!(session.lineage.root_session_id, agent.session_id());
    assert_eq!(session.lineage.parent_session_id, None);
    assert_eq!(session.lineage.origin, Origin::Root);
    assert_eq!(session.lineage.depth, 0);
    let capabilities: Capabilities = agent.capabilities();
    assert!(
        capabilities.checkpoint
            && capabilities.resume
            && capabilities.fork
            && capabilities.fork_at
            && capabilities.side_conversation
            && capabilities.spawn
            && capabilities.steering
            && capabilities.identified_steering
            && capabilities.compaction
            && capabilities.developer_messages
            && capabilities.context
            && capabilities.ultrafast_service_tier,
        "a local Codex session supports every lifecycle operation: {capabilities:?}"
    );
    assert_eq!(capabilities.model, Mutability::BeforeFirstPrompt);
    assert_eq!(capabilities.thinking, Mutability::Anytime);
    assert_eq!(capabilities.service_tier, Mutability::Anytime);
    assert!(
        agent.persistence().is_none(),
        "no rollout or durable policy"
    );
    agent.flush().await.unwrap();

    agent.prompt("first").await.unwrap().result().await.unwrap();
    let (side, side_events) = agent
        .fork(ForkRequest::latest().side_conversation())
        .await
        .unwrap();
    let lineage = &side.session().lineage;
    assert_eq!(lineage.origin, Origin::SideConversation);
    assert_eq!(
        lineage.parent_session_id.as_deref(),
        Some(agent.session_id())
    );
    assert_eq!(lineage.root_session_id, agent.session_id());
    assert_eq!(lineage.depth, 1);
    let (grandchild, grandchild_events) = side.fork(ForkRequest::latest()).await.unwrap();
    let lineage = &grandchild.session().lineage;
    assert_eq!(lineage.origin, Origin::Fork);
    assert_eq!(
        lineage.parent_session_id.as_deref(),
        Some(side.session_id())
    );
    assert_eq!(lineage.root_session_id, agent.session_id());
    assert_eq!(lineage.depth, 2);
    let (spawned, spawned_events) = agent.spawn().await.unwrap();
    assert_eq!(spawned.session().lineage.origin, Origin::Subagent);
    assert_eq!(spawned.session().lineage.depth, 1);
    for child in [&side, &grandchild, &spawned] {
        child.shutdown().await.unwrap();
    }
    agent.shutdown().await.unwrap();
    drop((events, side_events, grandchild_events, spawned_events));
}

#[tokio::test]
async fn portable_checkpoints_resume_and_fork_only_within_their_conversation() {
    let (openai, _attempts) = completing_openai!();
    let (agent, events) = tool_free_agent!(openai.clone());
    let first = agent.prompt("first").await.unwrap().result().await.unwrap();
    let checkpoint =
        SessionCheckpoint::from_json(&first.checkpoint().unwrap().to_json().unwrap()).unwrap();
    assert_eq!(checkpoint.session_id(), agent.session_id());
    assert_eq!(checkpoint.family(), nanocodex_agent::HarnessFamily::Codex);
    assert_eq!(checkpoint.turn_id(), first.turn_id());

    // A portable checkpoint and a live turn boundary both fork this conversation.
    let (from_checkpoint, from_checkpoint_events) = agent
        .fork(ForkRequest::at(checkpoint.clone()))
        .await
        .unwrap();
    assert_eq!(
        from_checkpoint
            .session()
            .lineage
            .parent_session_id
            .as_deref(),
        Some(agent.session_id())
    );
    let branched = from_checkpoint
        .prompt("continue the branch")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(
        branched.checkpoint().unwrap().conversation_id(),
        checkpoint.conversation_id(),
        "forks share the conversation tree"
    );
    let (from_turn, from_turn_events) = agent.fork(ForkRequest::at_turn(&first)).await.unwrap();

    // Resume reopens the same session (identity, lineage, model, thinking)
    // in a fresh runtime with the retained conversation.
    let (resumed, resumed_events) = Nanocodex::builder(openai.clone())
        .tools(Tools::builder().without_defaults().build().unwrap())
        .resume(checkpoint.clone())
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(resumed.session_id(), agent.session_id());
    assert_eq!(resumed.session().lineage, agent.session().lineage);
    let resumed_checkpoint = resumed.checkpoint().await.unwrap();
    assert!(resumed_checkpoint.has_conversation());
    assert_eq!(resumed_checkpoint.model(), checkpoint.model());
    assert_eq!(resumed_checkpoint.thinking(), checkpoint.thinking());
    assert_eq!(
        resumed_checkpoint.conversation_id(),
        checkpoint.conversation_id()
    );
    resumed
        .prompt("continue after resume")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();

    // Boundaries of an unrelated conversation are rejected.
    let (other, other_events) = tool_free_agent!(openai.clone());
    let other_first = other.prompt("other").await.unwrap().result().await.unwrap();
    assert!(matches!(
        agent
            .fork(ForkRequest::at(other.checkpoint().await.unwrap()))
            .await,
        Err(NanocodexError::CheckpointLineageMismatch)
    ));
    assert!(matches!(
        agent.fork(ForkRequest::at_turn(&other_first)).await,
        Err(NanocodexError::CheckpointLineageMismatch)
    ));

    // A durable store's native snapshot and a legacy child record convert to
    // checkpoints of the same conversation.
    let native = checkpoint.codex_snapshot().unwrap().expect("conversation");
    let rewrapped = SessionCheckpoint::codex(
        agent.session_id(),
        agent.session().lineage.clone(),
        checkpoint.thinking(),
        native.clone(),
    )
    .unwrap();
    assert_eq!(rewrapped.conversation_id(), checkpoint.conversation_id());
    let legacy = serde_json::json!({
        "session_id": agent.session_id(),
        "model": match checkpoint.model() {
            nanocodex_agent::HarnessModel::Codex(model) => model,
            other => panic!("unexpected model {other:?}"),
        },
        "thinking": checkpoint.thinking(),
        "fast_mode": true,
        "conversation": native,
    });
    let legacy =
        SessionCheckpoint::from_legacy_codex_child(legacy, agent.session().lineage.clone())
            .unwrap();
    assert_eq!(
        legacy.payload()["service_tier"],
        serde_json::to_value(nanocodex_agent::ServiceTier::Fast).unwrap()
    );
    let (from_legacy, from_legacy_events) = agent.fork(ForkRequest::at(legacy)).await.unwrap();
    from_legacy.shutdown().await.unwrap();
    drop(from_legacy_events);

    // Another family's checkpoint is rejected before its payload is decoded.
    let mut foreign = serde_json::to_value(&checkpoint).unwrap();
    foreign["model"] = serde_json::json!(
        nanocodex_agent::HarnessFamily::Claude
            .default_model()
            .as_str()
    );
    let foreign = SessionCheckpoint::from_json(&foreign.to_string()).unwrap();
    assert!(matches!(
        agent.fork(ForkRequest::at(foreign.clone())).await,
        Err(NanocodexError::CheckpointFamilyMismatch { .. })
    ));
    assert!(matches!(
        Nanocodex::builder(openai).resume(foreign),
        Err(NanocodexError::CheckpointFamilyMismatch { .. })
    ));

    for handle in [&from_checkpoint, &from_turn, &resumed, &other, &agent] {
        handle.shutdown().await.unwrap();
    }
    drop((
        events,
        from_checkpoint_events,
        from_turn_events,
        resumed_events,
        other_events,
    ));
}
