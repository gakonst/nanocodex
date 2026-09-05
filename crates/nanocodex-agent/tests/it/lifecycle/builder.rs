use super::*;

#[tokio::test]
async fn accepts_a_caller_composed_tower_service_factory() {
    let openai = OpenAi::builder("test")
        .service(|| {
            ServiceBuilder::new()
                .layer(TimeoutLayer::new(Duration::from_secs(30)))
                .layer(ConcurrencyLimitLayer::new(1))
                .service(NeverCalled)
        })
        .build()
        .unwrap();

    let (_agent, events) = Nanocodex::builder(openai).build().unwrap();
    drop(events);
}

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
async fn defers_layers_until_the_standard_service_is_built() {
    let openai = OpenAi::builder("test")
        .layer(TimeoutLayer::new(Duration::from_secs(30)))
        .layer(ConcurrencyLimitLayer::new(1))
        .build()
        .unwrap();

    let (_agent, events) = Nanocodex::builder(openai).build().unwrap();
    drop(events);
}

#[test]
fn builder_variants_are_cloneable() {
    fn assert_clone<T: Clone>(_: &T) {}

    let standard = Nanocodex::builder(test_openai());
    assert_clone(&standard);

    let layered_openai = OpenAi::builder("test")
        .layer(TimeoutLayer::new(Duration::from_secs(30)))
        .build()
        .unwrap();
    let layered = Nanocodex::builder(layered_openai);
    assert_clone(&layered);

    let factory_openai = OpenAi::builder("test")
        .service(|| NeverCalled)
        .build()
        .unwrap();
    let factory = Nanocodex::builder(factory_openai);
    assert_clone(&factory);
}

#[tokio::test]
async fn configured_openai_recipe_preserves_its_concrete_service_factory() {
    let service_builds = Arc::new(AtomicU64::new(0));
    let factory_builds = Arc::clone(&service_builds);
    let openai = OpenAi::builder("test")
        .service(move || {
            factory_builds.fetch_add(1, Ordering::Relaxed);
            NeverCalled
        })
        .build()
        .unwrap();
    let builder = Nanocodex::builder(openai)
        .instructions("Answer only from supplied facts and preserve exact identifiers.");

    let (first, first_events) = builder.clone().build().unwrap();
    let (second, second_events) = builder.build().unwrap();

    assert_eq!(service_builds.load(Ordering::Relaxed), 2);
    assert_ne!(first.session_id(), second.session_id());
    drop((first, first_events, second, second_events));
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

    let rollout = agent.rollout().expect("rollout enabled");
    assert_eq!(agent.session_id().to_string(), events.request_id());
    assert_eq!(rollout.thread_id(), agent.session_id().to_string());
    assert!(uuid::Uuid::parse_str(rollout.thread_id()).is_ok());
    assert!(rollout.path().is_file());
    agent.flush_rollout().await.unwrap();
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

    assert_eq!(agent.local_session_id(), Some(session_id));
    assert_eq!(
        agent.rollout().expect("rollout enabled").thread_id(),
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
