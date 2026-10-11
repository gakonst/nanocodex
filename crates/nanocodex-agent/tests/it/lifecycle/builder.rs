use super::*;

#[tokio::test]
async fn starts_dynamic_providers_before_build_returns() {
    let started = Arc::new(AtomicBool::new(false));
    let tools = Tools::builder()
        .provider(StartProbe(Arc::clone(&started)))
        .build()
        .unwrap();
    let openai = OpenAi::builder("test")
        .service(|| NeverCalled)
        .build()
        .unwrap();

    let (_agent, events) = Nanocodex::builder(openai).tools(tools).build().unwrap();

    assert!(started.load(Ordering::Acquire));
    drop(events);
}

#[tokio::test]
async fn cloned_builders_create_distinct_agents() {
    let service_builds = Arc::new(AtomicU64::new(0));
    let factory_builds = Arc::clone(&service_builds);
    let openai = OpenAi::builder("test")
        .service(move || {
            factory_builds.fetch_add(1, Ordering::Relaxed);
            NeverCalled
        })
        .build()
        .unwrap();
    let builder = Nanocodex::builder(openai);

    let (first, first_events) = builder.clone().build().unwrap();
    let (second, second_events) = builder.build().unwrap();

    assert_eq!(service_builds.load(Ordering::Relaxed), 2);
    assert_ne!(first.session_id(), second.session_id());
    assert_ne!(first_events.request_id(), second_events.request_id());
    drop((first, first_events, second, second_events));
}

#[tokio::test]
async fn rollout_uses_the_agent_session_as_the_codex_thread_id() {
    let home = tempdir().unwrap();
    let openai = OpenAi::builder("test")
        .service(|| NeverCalled)
        .build()
        .unwrap();
    let (agent, events) = Nanocodex::builder(openai)
        .rollout(RolloutConfig::new(home.path()))
        .build()
        .unwrap();

    let rollout = agent
        .persistence()
        .and_then(|persistence| persistence.rollout)
        .expect("rollout enabled");
    assert_eq!(agent.session_id().to_string(), events.request_id());
    assert_eq!(rollout.thread_id(), agent.session_id().to_string());
    assert!(rollout.path().is_file());
    agent.flush().await.unwrap();
    drop((agent, events));
}

#[tokio::test]
async fn rollout_uses_an_explicit_typed_session_id() {
    let home = tempdir().unwrap();
    let openai = OpenAi::builder("test")
        .service(|| NeverCalled)
        .build()
        .unwrap();
    let session_id = SessionId::new();
    let (agent, events) = Nanocodex::builder(openai)
        .session_id(session_id)
        .rollout(RolloutConfig::new(home.path()))
        .build()
        .unwrap();

    assert_eq!(agent.session_id(), session_id.to_string());
    assert_eq!(
        agent
            .persistence()
            .and_then(|persistence| persistence.rollout)
            .expect("rollout enabled")
            .thread_id(),
        session_id.to_string()
    );
    drop((agent, events));
}

#[tokio::test]
async fn rejects_an_empty_prompt_cache_key() {
    let openai = OpenAi::builder("test")
        .service(|| NeverCalled)
        .build()
        .unwrap();
    let outcome = Nanocodex::builder(openai).prompt_cache_key("  ").build();

    let Err(error) = outcome else {
        panic!("empty prompt cache key unexpectedly built");
    };
    assert!(error.to_string().contains("prompt_cache_key"));
}

#[tokio::test]
async fn resolves_the_owned_workspace_before_build_returns() {
    let parent = tempdir().unwrap();
    let missing = parent.path().join("missing-workspace");
    let openai = OpenAi::builder("test")
        .service(|| NeverCalled)
        .build()
        .unwrap();

    let outcome = Nanocodex::builder(openai).workspace(&missing).build();

    let Err(error) = outcome else {
        panic!("a missing workspace unexpectedly built");
    };
    assert!(matches!(
        error,
        NanocodexError::ResolveWorkspace { path, .. } if path == missing
    ));
}

#[tokio::test]
async fn side_conversation_rollouts_keep_their_recorded_provenance() {
    let home = tempdir().unwrap();
    let (retained, _attempts) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: retained.clone(),
        })
        .build()
        .unwrap();
    let (agent, events) = Nanocodex::builder(openai)
        .tools(Tools::builder().without_defaults().build().unwrap())
        .rollout(RolloutConfig::new(home.path()))
        .build()
        .unwrap();
    let persistence = agent.persistence().expect("rollout persistence");
    assert!(persistence.resumable());
    assert_eq!(persistence.durable_state_id, None);
    agent.prompt("first").await.unwrap().result().await.unwrap();

    for (request, kind, role) in [
        (ForkRequest::latest(), "fork", "branch"),
        (
            ForkRequest::latest().side_conversation(),
            "fork",
            "side_conversation",
        ),
    ] {
        let (child, child_events) = agent.fork(request).await.unwrap();
        child.flush().await.unwrap();
        let rollout = child
            .persistence()
            .and_then(|persistence| persistence.rollout)
            .expect("forks record their own rollout");
        let meta: Value = serde_json::from_str(
            std::fs::read_to_string(rollout.path())
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(meta["payload"]["origin_kind"], kind);
        assert_eq!(meta["payload"]["conversation_role"], role);
        assert_eq!(meta["payload"]["forked_from_id"], agent.session_id());
        assert_eq!(meta["payload"]["root_session_id"], agent.session_id());
        child
            .prompt("continue the child")
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        child.flush().await.unwrap();
        let listed = RolloutConfig::new(home.path())
            .list_sessions()
            .unwrap()
            .into_iter()
            .find(|session| session.thread_id() == child.session_id())
            .expect("the child rollout is listed");
        assert_eq!(listed.origin(), child.session().lineage.origin);
        assert_eq!(listed.parent_session_id(), Some(agent.session_id()));
        assert_eq!(listed.root_session_id(), agent.session_id());
        assert_eq!(
            listed.harness_family(),
            Some(nanocodex_agent::HarnessFamily::Codex)
        );
        child.shutdown().await.unwrap();
        drop(child_events);
    }
    agent.flush().await.unwrap();
    // A loaded rollout yields a portable checkpoint that resumes its conversation.
    let loaded = RolloutConfig::new(home.path())
        .load_session(agent.session_id())
        .unwrap();
    let checkpoint = loaded.checkpoint().unwrap();
    assert!(checkpoint.has_conversation());
    let (retained, _attempts) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: retained.clone(),
        })
        .build()
        .unwrap();
    let (resumed, resumed_events) = Nanocodex::builder(openai)
        .tools(Tools::builder().without_defaults().build().unwrap())
        .resume(checkpoint)
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        resumed.session_id(),
        agent.session_id(),
        "resume keeps identity"
    );
    resumed
        .prompt("after loading")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    resumed.shutdown().await.unwrap();
    drop(resumed_events);
    agent.shutdown().await.unwrap();
    drop(events);
}

#[tokio::test]
async fn harness_neutral_rollout_writer_records_a_loadable_session() {
    use nanocodex_agent::{
        HarnessModel, Lineage, Model, Thinking,
        input::Prompt,
        rollout::{RolloutSession, RolloutTurnRecord, RolloutWriter},
    };

    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let session_id = SessionId::new().to_string();
    let config = RolloutConfig::new(home.path());
    let writer = RolloutWriter::create(
        &config,
        &RolloutSession {
            session_id: session_id.clone(),
            lineage: Lineage::root(&session_id),
            cwd: workspace.path().to_path_buf(),
            instructions: "neutral instructions".to_owned(),
            prompt_cache_key: None,
        },
    )
    .unwrap();
    let mut history = vec![ResponseItem::message(
        MessageRole::User,
        [ContentItem::input_text("hello from another harness")],
    )];
    history.push(ResponseItem::message(
        MessageRole::Assistant,
        [ContentItem::output_text("recorded")],
    ));
    let prompt = Prompt::from("hello from another harness");
    writer
        .commit(
            RolloutTurnRecord::started("turn-1", &prompt, Thinking::Medium).completed("recorded"),
            HarnessModel::Codex(Model::Sol),
            history.clone(),
        )
        .await
        .unwrap();
    // A later turn passes the complete history; only its new items are appended.
    history.push(ResponseItem::message(
        MessageRole::User,
        [ContentItem::input_text("and again")],
    ));
    history.push(ResponseItem::message(
        MessageRole::Assistant,
        [ContentItem::output_text("twice")],
    ));
    writer
        .commit(
            RolloutTurnRecord::started("turn-2", &Prompt::from("and again"), Thinking::Medium)
                .completed("twice"),
            HarnessModel::Codex(Model::Sol),
            history,
        )
        .await
        .unwrap();
    writer.flush().await.unwrap();
    assert_eq!(writer.info().thread_id(), session_id);
    let response_items = std::fs::read_to_string(writer.info().path())
        .unwrap()
        .lines()
        .filter(|line| line.contains(r#""type":"response_item""#))
        .count();
    assert_eq!(response_items, 4, "committed history is never duplicated");

    let loaded = config.load_session(&session_id).unwrap();
    assert_eq!(loaded.transcript().len(), 4);
    assert_eq!(loaded.model(), Model::Sol);
    assert!(
        config
            .list_sessions()
            .unwrap()
            .iter()
            .any(|session| session.thread_id() == session_id)
    );
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_rollout_whose_workspace_is_gone_still_loads_but_cannot_resume() {
    use nanocodex_agent::{
        HarnessModel, Lineage, Model, Thinking,
        input::Prompt,
        rollout::{RolloutSession, RolloutTurnRecord, RolloutWriter},
    };

    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let cwd = workspace.path().join("removed");
    std::fs::create_dir(&cwd).unwrap();
    let session_id = SessionId::new().to_string();
    let config = RolloutConfig::new(home.path());
    let writer = RolloutWriter::create(
        &config,
        &RolloutSession {
            session_id: session_id.clone(),
            lineage: Lineage::root(&session_id),
            cwd: cwd.clone(),
            instructions: "instructions".to_owned(),
            prompt_cache_key: None,
        },
    )
    .unwrap();
    let prompt = Prompt::from("hello");
    writer
        .commit(
            RolloutTurnRecord::started("turn-1", &prompt, Thinking::Medium).completed("hi"),
            HarnessModel::Codex(Model::Sol),
            vec![
                ResponseItem::message(MessageRole::User, [ContentItem::input_text("hello")]),
                ResponseItem::message(MessageRole::Assistant, [ContentItem::output_text("hi")]),
            ],
        )
        .await
        .unwrap();
    writer.shutdown().await.unwrap();
    std::fs::remove_dir(&cwd).unwrap();

    let loaded = config.load_session(&session_id).unwrap();
    assert_eq!(loaded.transcript().len(), 2);
    assert_eq!(std::path::Path::new(loaded.workspace()), cwd.as_path());
    let Err(error) = Nanocodex::builder(test_openai())
        .resume(loaded.checkpoint().unwrap())
        .unwrap()
        .build()
    else {
        panic!("a missing workspace must not resume");
    };
    assert!(
        matches!(error, NanocodexError::ResolveWorkspace { .. }),
        "{error:?}"
    );
}
