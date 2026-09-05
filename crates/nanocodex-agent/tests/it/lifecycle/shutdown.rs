use super::*;

#[tokio::test(flavor = "current_thread")]
async fn flush_is_durable_without_stopping_the_agent() {
    let home = tempdir().unwrap();
    let (retained, _retained_attempts) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: retained.clone(),
        })
        .build()
        .unwrap();
    let (agent, events) = Nanocodex::builder(openai.clone())
        .rollout(RolloutConfig::new(home.path()))
        .build()
        .unwrap();
    let thread_id = agent.session_id().to_string();

    agent
        .prompt("persist this turn")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    agent.flush_rollout().await.unwrap();
    let durable = RolloutConfig::new(home.path())
        .load_session(&thread_id)
        .expect("flush must make the rollout readable and durable");
    assert_eq!(durable.transcript().len(), 2);

    agent
        .prompt("persist another turn")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    agent.flush_rollout().await.unwrap();
    let durable = RolloutConfig::new(home.path())
        .load_session(&thread_id)
        .expect("the second flush must advance the durable boundary");
    assert_eq!(durable.transcript().len(), 4);

    agent.shutdown().await.unwrap();
    drop((agent, events));
}

struct DropProvider {
    dropped: Arc<AtomicBool>,
}

impl Drop for DropProvider {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}

#[async_trait]
impl DynamicToolProvider for DropProvider {
    fn start(&self) {}

    fn direct_tools(&self) -> Vec<Arc<dyn nanocodex_tools::Tool>> {
        Vec::new()
    }

    fn available_definitions(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }

    async fn execute(
        &self,
        _name: &str,
        _input: Value,
        _context: ToolContext<'_>,
    ) -> Option<ToolOutput> {
        None
    }
}

#[tokio::test]
async fn explicit_shutdown_joins_resources_and_flushes_the_rollout() {
    let home = tempdir().unwrap();
    let model_started = Arc::new(AtomicBool::new(false));
    let model_dropped = Arc::new(AtomicBool::new(false));
    let tools_dropped = Arc::new(AtomicBool::new(false));
    let service = {
        let model_started = Arc::clone(&model_started);
        let model_dropped = Arc::clone(&model_dropped);
        move || DropPendingService {
            started: Arc::clone(&model_started),
            dropped: Arc::clone(&model_dropped),
        }
    };
    let openai = OpenAi::builder("test").service(service).build().unwrap();
    let tools = Tools::builder()
        .without_defaults()
        .provider(DropProvider {
            dropped: Arc::clone(&tools_dropped),
        })
        .build()
        .unwrap();
    let (agent, mut events) = Nanocodex::builder(openai.clone())
        .tools(tools)
        .rollout(RolloutConfig::new(home.path()))
        .build()
        .unwrap();
    let clone = agent.clone();
    let thread_id = agent.session_id().to_string();
    let active = agent.prompt("active turn").await.unwrap();

    tokio::time::timeout(Duration::from_secs(1), async {
        while !model_started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the active model attempt should start");

    let queued = agent.prompt("second queued turn").await.unwrap();
    clone.shutdown().await.unwrap();

    assert!(
        model_dropped.load(Ordering::Acquire),
        "shutdown must drop the active model attempt before returning"
    );
    assert!(
        tools_dropped.load(Ordering::Acquire),
        "shutdown must drop the per-driver tools before returning"
    );
    assert!(matches!(
        agent.prompt("after shutdown").await,
        Err(NanocodexError::AgentStopped)
    ));
    assert!(matches!(
        active.result().await,
        Err(NanocodexError::TurnCancelled)
    ));
    assert!(matches!(
        queued.result().await,
        Err(NanocodexError::TurnCancelled)
    ));

    let mut started = Vec::new();
    let mut terminals = Vec::new();
    while let Some(timed) = events.try_recv_timed() {
        match timed.event.kind {
            nanocodex_agent::events::AgentEventKind::RunStarted => {
                let run: nanocodex_agent::events::RunStarted =
                    timed.event.decode_payload().unwrap();
                started.push(run.instruction_bytes);
            }
            nanocodex_agent::events::AgentEventKind::RunCompleted
            | nanocodex_agent::events::AgentEventKind::RunFailed => {
                let terminal: nanocodex_agent::events::RunTerminal =
                    timed.event.decode_payload().unwrap();
                terminals.push(terminal.status);
            }
            _ => {}
        }
    }
    assert_eq!(
        started,
        [11, 18],
        "shutdown must terminalize accepted turns in FIFO order"
    );
    assert_eq!(
        terminals,
        [nanocodex_agent::events::RunStatus::Cancelled; 2],
        "every accepted turn must emit exactly one terminal event"
    );

    let durable = RolloutConfig::new(home.path())
        .load_session(&thread_id)
        .expect("shutdown must flush the interrupted turn");
    let (thread_id, snapshot, rollout) = durable.into_parts();
    let resumed_tools = Tools::builder().without_defaults().build().unwrap();
    let (resumed, resumed_events) = Nanocodex::builder(openai)
        .tools(resumed_tools)
        .session_id(thread_id.parse().unwrap())
        .resume(snapshot)
        .rollout(rollout)
        .build()
        .expect("shutdown must leave a resumable durable boundary");
    resumed.shutdown().await.unwrap();
    drop((agent, clone, events, resumed, resumed_events));
}

#[tokio::test]
async fn shutdown_is_idempotent_across_cloned_handles() {
    let openai = OpenAi::builder("test")
        .service(|| PendingService)
        .build()
        .unwrap();
    let tools = Tools::builder().without_defaults().build().unwrap();
    let (agent, events) = Nanocodex::builder(openai).tools(tools).build().unwrap();
    let clone = agent.clone();

    let (first, concurrent) = tokio::join!(agent.shutdown(), clone.shutdown());
    first.unwrap();
    concurrent.unwrap();
    agent.shutdown().await.unwrap();
    clone.shutdown().await.unwrap();

    drop((agent, clone, events));
}

#[tokio::test]
async fn implicit_shutdown_emits_one_terminal_event_for_every_accepted_turn() {
    let openai = OpenAi::builder("test")
        .service(|| PendingService)
        .build()
        .unwrap();
    let tools = Tools::builder().without_defaults().build().unwrap();
    let (agent, mut events) = Nanocodex::builder(openai).tools(tools).build().unwrap();

    let active = agent.prompt("active turn").await.unwrap();
    let queued_first = agent.prompt("first queued turn").await.unwrap();
    let queued_second = agent.prompt("second queued turn").await.unwrap();

    // `AgentEvents` is independent from the command channel, so it can observe
    // shutdown after every owning command handle and result receiver is gone.
    drop((active, queued_first, queued_second, agent));

    let (started, terminals) = tokio::time::timeout(Duration::from_secs(1), async {
        let mut started = Vec::new();
        let mut terminals = Vec::new();
        while let Some(event) = events.recv().await {
            match event.kind {
                nanocodex_agent::events::AgentEventKind::RunStarted => {
                    let run: nanocodex_agent::events::RunStarted = event.decode_payload().unwrap();
                    started.push(run.instruction_bytes);
                }
                nanocodex_agent::events::AgentEventKind::RunCompleted
                | nanocodex_agent::events::AgentEventKind::RunFailed => {
                    let terminal: nanocodex_agent::events::RunTerminal =
                        event.decode_payload().unwrap();
                    terminals.push(terminal.status);
                }
                _ => {}
            }
        }
        (started, terminals)
    })
    .await
    .expect("implicit shutdown should close the independent event stream");

    assert_eq!(started, [11, 17, 18], "accepted prompts must remain FIFO");
    assert_eq!(
        terminals,
        [nanocodex_agent::events::RunStatus::Cancelled; 3],
        "every accepted prompt must emit exactly one cancellation terminal"
    );
}
