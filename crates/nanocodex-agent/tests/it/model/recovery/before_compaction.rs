use std::{
    collections::HashMap,
    future::{Ready, ready},
    sync::{
        Mutex,
        atomic::{AtomicU32, Ordering},
    },
    task::{Context, Poll},
};

use nanocodex_agent::execution::{
    BeforeCompaction, BeforeCompactionRequest, CompactionReceipt, ExecutionAdmission,
    ExecutionContinuation, ExecutionFuture, ExecutionOutput, ExecutionPolicy,
    ExecutionStepAdmission,
};
use nanocodex_oai_api::{
    responses::{ContentItem, MessageRole, ResponseItem, ResponseItemId, Usage},
    tower::{
        CompactionOutput, GenerationOutput, ResponsePipelineStats, ResponsesAttempt,
        ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse,
    },
};
use tokio::sync::{Notify, Semaphore};
use tower::Service;

use super::*;

#[derive(Default)]
struct Evidence {
    order: Mutex<Vec<&'static str>>,
    requests: Mutex<Vec<BeforeCompactionRequest>>,
    generations: AtomicU32,
    compactions: AtomicU32,
    dropped_hooks: AtomicU32,
    // 0: success, 1: rejection, 2: invalid receipt.
    hook_outcome: AtomicU32,
    gate: Option<Semaphore>,
    started: Notify,
}

#[derive(Clone)]
struct Preserve(Arc<Evidence>);

impl BeforeCompaction for Preserve {
    fn preserve(
        &self,
        request: BeforeCompactionRequest,
    ) -> ExecutionFuture<'_, nanocodex_agent::Result<CompactionReceipt>> {
        Box::pin(async move {
            struct InFlight<'a>(&'a Evidence, bool);
            impl Drop for InFlight<'_> {
                fn drop(&mut self) {
                    if !self.1 {
                        self.0.dropped_hooks.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
            let mut in_flight = InFlight(&self.0, false);
            self.0.requests.lock().unwrap().push(request);
            self.0.order.lock().unwrap().push("preserve_started");
            self.0.started.notify_one();
            if let Some(gate) = &self.0.gate {
                gate.acquire().await.unwrap().forget();
            }
            in_flight.1 = true;
            match self.0.hook_outcome.load(Ordering::SeqCst) {
                1 => Err(NanocodexError::BeforeCompactionFailed(
                    "durable write failed".into(),
                )),
                2 => Ok(CompactionReceipt {
                    receipt_id: " ".into(),
                }),
                _ => {
                    self.0.order.lock().unwrap().push("preserved");
                    Ok(CompactionReceipt {
                        receipt_id: "durable-receipt-1".into(),
                    })
                }
            }
        })
    }
}

#[derive(Clone, Copy)]
enum Trigger {
    Manual,
    PreTurn,
    MidTurn,
}

#[derive(Clone)]
struct Provider {
    evidence: Arc<Evidence>,
    trigger: Trigger,
}

impl Service<ResponsesAttempt> for Provider {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Ready<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        let output = match request.kind() {
            ResponsesAttemptKind::Generation => {
                let index = self.evidence.generations.fetch_add(1, Ordering::SeqCst);
                self.evidence.order.lock().unwrap().push("generation");
                let answer = format!("source answer {index}");
                ResponsesOutput::Generation(GenerationOutput {
                    id: format!("resp-{index}"),
                    status: "completed".into(),
                    end_turn: Some(!(index == 0 && matches!(self.trigger, Trigger::MidTurn))),
                    final_message: Some(answer.clone()),
                    output_items: vec![ResponseItem::message(
                        MessageRole::Assistant,
                        [ContentItem::output_text(answer)],
                    )],
                    code_calls: vec![],
                    usage: Some(Usage {
                        total_tokens: if index == 0 && !matches!(self.trigger, Trigger::Manual) {
                            244_800
                        } else {
                            120
                        },
                        ..Usage::default()
                    }),
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: None,
                    pipeline_stats: ResponsePipelineStats::default(),
                })
            }
            ResponsesAttemptKind::Compaction => {
                self.evidence.compactions.fetch_add(1, Ordering::SeqCst);
                self.evidence
                    .order
                    .lock()
                    .unwrap()
                    .push("provider_compaction");
                ResponsesOutput::Compaction(CompactionOutput {
                    id: "resp-compaction".into(),
                    status: "completed".into(),
                    item: ResponseItem::Compaction {
                        id: Some(ResponseItemId::from("cmp-preserved")),
                        encrypted_content: "opaque-summary".into(),
                        created_by: None,
                        internal_chat_message_metadata_passthrough: None,
                    },
                    usage: None,
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: None,
                    pipeline_stats: ResponsePipelineStats::default(),
                })
            }
            kind => panic!("unexpected attempt: {kind:?}"),
        };
        ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

fn provider(
    evidence: &Arc<Evidence>,
    trigger: Trigger,
) -> Result<
    OpenAi<
        impl nanocodex_oai_api::tower::ResponsesServiceFactory<Service = Provider> + Send + Sync + 'static,
    >,
> {
    let evidence = evidence.clone();
    Ok(OpenAi::builder("test-key")
        .transport(ResponsesTransport::Https)
        .service(move || Provider {
            evidence: evidence.clone(),
            trigger,
        })
        .build()?)
}

async fn wait_for_hook(evidence: &Evidence) -> Result<()> {
    timeout(
        std::time::Duration::from_secs(5),
        evidence.started.notified(),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn before_compaction_awaits_receipt_for_manual_pre_turn_and_mid_turn() -> Result<()> {
    for trigger in [Trigger::Manual, Trigger::PreTurn, Trigger::MidTurn] {
        let workspace = tempfile::tempdir()?;
        let evidence = Arc::new(Evidence {
            gate: Some(Semaphore::new(0)),
            ..Evidence::default()
        });
        let (agent, events) = Nanocodex::builder(provider(&evidence, trigger)?)
            .session_id(test_session_id())
            .workspace(workspace.path())
            .before_compaction(Preserve(evidence.clone()))
            .build()?;
        drop(events);
        let running = if matches!(trigger, Trigger::MidTurn) {
            let turn = agent.prompt("source user statement").await?;
            tokio::spawn(async move { turn.result().await.map(|_| ()) })
        } else {
            agent
                .prompt("source user statement")
                .await?
                .result()
                .await?;
            let handle = agent.clone();
            tokio::spawn(async move {
                if matches!(trigger, Trigger::Manual) {
                    handle.compact().await
                } else {
                    handle
                        .prompt("next user prompt")
                        .await?
                        .result()
                        .await
                        .map(|_| ())
                }
            })
        };
        wait_for_hook(&evidence).await?;
        assert!(
            !running.is_finished(),
            "compaction must await host acknowledgement"
        );
        assert_eq!(evidence.compactions.load(Ordering::SeqCst), 0);
        {
            let requests = evidence.requests.lock().unwrap();
            let request = &requests[0];
            assert_eq!(request.session_id, TEST_SESSION_ID);
            assert_eq!(request.root_session_id, TEST_SESSION_ID);
            assert!(!request.truncated);
            assert_eq!(request.messages.len(), 2);
            assert_eq!(request.messages[0].role, MessageRole::User);
            assert_eq!(request.messages[0].text, "source user statement");
            assert_eq!(request.messages[1].text, "source answer 0");
        }
        evidence.gate.as_ref().unwrap().add_permits(1);
        timeout(std::time::Duration::from_secs(5), running).await???;
        let order = evidence.order.lock().unwrap().clone();
        assert!(
            order.iter().position(|item| *item == "preserved").unwrap()
                < order
                    .iter()
                    .position(|item| *item == "provider_compaction")
                    .unwrap()
        );
        assert_eq!(evidence.compactions.load(Ordering::SeqCst), 1);
        let context = serde_json::to_value(agent.context().await?.history())?;
        assert!(
            context
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["type"] == "compaction")
        );
        agent.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn before_compaction_rejection_and_invalid_receipt_retain_original_context() -> Result<()> {
    for outcome in [1, 2] {
        let workspace = tempfile::tempdir()?;
        let evidence = Arc::new(Evidence::default());
        evidence.hook_outcome.store(outcome, Ordering::SeqCst);
        let (agent, events) = Nanocodex::builder(provider(&evidence, Trigger::Manual)?)
            .workspace(workspace.path())
            .before_compaction(Preserve(evidence.clone()))
            .build()?;
        drop(events);
        agent.prompt("keep this source").await?.result().await?;
        let before = serde_json::to_value(agent.context().await?.history())?;
        assert!(matches!(
            agent.compact().await,
            Err(NanocodexError::BeforeCompactionFailed(_))
        ));
        assert_eq!(evidence.compactions.load(Ordering::SeqCst), 0);
        assert_eq!(
            before,
            serde_json::to_value(agent.context().await?.history())?
        );
        evidence.hook_outcome.store(0, Ordering::SeqCst);
        agent.compact().await?;
        assert_eq!(evidence.compactions.load(Ordering::SeqCst), 1);
        agent.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn before_compaction_cancellation_drops_hook_without_discarding_source() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let evidence = Arc::new(Evidence {
        gate: Some(Semaphore::new(0)),
        ..Evidence::default()
    });
    let (agent, events) = Nanocodex::builder(provider(&evidence, Trigger::Manual)?)
        .workspace(workspace.path())
        .before_compaction(Preserve(evidence.clone()))
        .build()?;
    drop(events);
    agent
        .prompt("source survives interruption")
        .await?
        .result()
        .await?;
    let handle = agent.clone();
    let first = tokio::spawn(async move { handle.compact().await });
    wait_for_hook(&evidence).await?;
    // A replacement cancels the first barrier. The next barrier still sees the original source.
    let handle = agent.clone();
    let second = tokio::spawn(async move { handle.compact().await });
    wait_for_hook(&evidence).await?;
    assert!(matches!(first.await?, Err(NanocodexError::TurnCancelled)));
    assert_eq!(evidence.dropped_hooks.load(Ordering::SeqCst), 1);
    assert_eq!(evidence.compactions.load(Ordering::SeqCst), 0);
    {
        let requests = evidence.requests.lock().unwrap();
        assert_eq!(
            serde_json::to_value(&requests[0].messages)?,
            serde_json::to_value(&requests[1].messages)?
        );
        assert_ne!(
            requests[0].boundary_id, requests[1].boundary_id,
            "a new compaction operation has a new boundary"
        );
    }
    evidence.gate.as_ref().unwrap().add_permits(1);
    second.await??;
    agent.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn before_compaction_is_not_inherited_by_spawn_fork_or_restored_children() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let evidence = Arc::new(Evidence::default());
    let (agent, events) = Nanocodex::builder(provider(&evidence, Trigger::Manual)?)
        .workspace(workspace.path())
        .before_compaction(Preserve(evidence.clone()))
        .build()?;
    drop(events);
    agent.prompt("root only").await?.result().await?;
    let (spawned, events) = agent.spawn().await?;
    drop(events);
    spawned.compact().await?;
    let (fork, events) = agent.fork().await?;
    drop(events);
    fork.compact().await?;
    let snapshot = spawned.child_snapshot().await?;
    spawned.shutdown().await?;
    let (restored, events) = agent.restore_child(snapshot, None).await?;
    drop(events);
    restored.compact().await?;
    assert!(evidence.requests.lock().unwrap().is_empty());
    agent.compact().await?;
    assert_eq!(evidence.requests.lock().unwrap().len(), 1);
    fork.shutdown().await?;
    restored.shutdown().await?;
    agent.shutdown().await?;
    Ok(())
}

// A durable journal fixture injects losses on both sides of receipt persistence.
// Each retry constructs a new real agent driver from the retained continuation.
#[derive(Default)]
struct Journal {
    continuation: Mutex<Option<ExecutionContinuation>>,
    inputs: Mutex<Vec<Value>>,
    receipts: Mutex<HashMap<String, String>>,
    attempt: AtomicU32,
    order: Mutex<Vec<&'static str>>,
}

impl ExecutionPolicy for Journal {
    fn admit<'a>(
        &'a self,
        _: String,
        _: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<ExecutionAdmission>> {
        Box::pin(async { Ok(ExecutionAdmission::Execute) })
    }
    fn admit_automatic<'a>(
        &'a self,
        _: String,
        _: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<(String, ExecutionAdmission)>> {
        Box::pin(async {
            Ok((
                "stable-compaction-operation".into(),
                if self.attempt.load(Ordering::SeqCst) == 0 {
                    ExecutionAdmission::Execute
                } else {
                    ExecutionAdmission::Resume
                },
            ))
        })
    }
    fn release<'a>(&'a self, _: String) -> ExecutionFuture<'a, ()> {
        Box::pin(async {})
    }
    fn begin_attempt<'a>(&'a self, _: String) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn continuation<'a>(
        &'a self,
        _: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<Option<ExecutionContinuation>>> {
        Box::pin(async {
            Ok(self
                .continuation
                .lock()
                .unwrap()
                .as_ref()
                .map(|saved| ExecutionContinuation {
                    state_json: saved.state_json.clone(),
                    history: saved.history.clone(),
                    prefix: saved.prefix.clone(),
                }))
        })
    }
    fn advance<'a>(
        &'a self,
        _: String,
        continuation: ExecutionContinuation,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async move {
            *self.continuation.lock().unwrap() = Some(continuation);
            Ok(())
        })
    }
    fn begin_step<'a>(
        &'a self,
        _: String,
        step_id: String,
        kind: String,
        input_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<ExecutionStepAdmission>> {
        Box::pin(async move {
            if kind == "before_compaction" {
                self.inputs
                    .lock()
                    .unwrap()
                    .push(serde_json::from_str(&input_json).unwrap());
            }
            if kind == "compaction" {
                assert!(
                    self.receipts
                        .lock()
                        .unwrap()
                        .contains_key("before-compaction-0"),
                    "compaction is admitted only after a persisted preservation receipt"
                );
            }
            if kind == "compaction" && self.attempt.load(Ordering::SeqCst) == 1 {
                return Err(NanocodexError::InvalidExecutionPolicy(
                    "lost process after receipt".into(),
                ));
            }
            Ok(self.receipts.lock().unwrap().get(&step_id).cloned().map_or(
                ExecutionStepAdmission::Execute,
                ExecutionStepAdmission::Replay,
            ))
        })
    }
    fn complete_step<'a>(
        &'a self,
        _: String,
        step_id: String,
        output_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async move {
            if step_id.starts_with("before-") {
                if self.attempt.load(Ordering::SeqCst) == 0 {
                    return Err(NanocodexError::InvalidExecutionPolicy(
                        "lost receipt after host commit".into(),
                    ));
                }
                self.order.lock().unwrap().push("receipt_persisted");
            }
            self.receipts.lock().unwrap().insert(step_id, output_json);
            Ok(())
        })
    }
    fn complete<'a>(
        &'a self,
        _: String,
        _: SessionSnapshot,
        _: ExecutionOutput,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn fail_attempt<'a>(
        &'a self,
        _: String,
        _: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn fail<'a>(
        &'a self,
        _: String,
        _: SessionSnapshot,
        _: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn before_compaction_boundary_is_stable_and_durable_receipt_replays_after_restart()
-> Result<()> {
    let workspace = tempfile::tempdir()?;
    let evidence = Arc::new(Evidence::default());
    let journal = Arc::new(Journal::default());
    let (seed, events) = Nanocodex::builder(provider(&evidence, Trigger::Manual)?)
        .session_id(test_session_id())
        .workspace(workspace.path())
        .build()?;
    drop(events);
    let completed = seed
        .prompt("durable source before restart")
        .await?
        .result()
        .await?;
    let snapshot = completed.snapshot().expect("completed source snapshot");
    seed.shutdown().await?;
    for attempt in 0..3 {
        journal.attempt.store(attempt, Ordering::SeqCst);
        let (agent, events) = Nanocodex::builder(provider(&evidence, Trigger::Manual)?)
            .session_id(test_session_id())
            .workspace(workspace.path())
            .execution_policy(journal.clone())
            .resume(snapshot.clone())
            .before_compaction(Preserve(evidence.clone()))
            .build()?;
        drop(events);
        let result = agent.compact().await;
        if attempt < 2 {
            assert!(
                matches!(result, Err(NanocodexError::InvalidExecutionPolicy(_))),
                "{result:?}"
            );
            assert_eq!(evidence.compactions.load(Ordering::SeqCst), 0);
        } else {
            result?;
        }
        agent.shutdown().await?;
    }
    let inputs = journal.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 3);
    assert_eq!(inputs[0], inputs[1]);
    assert_eq!(inputs[1], inputs[2]);
    assert_eq!(
        inputs[0]["messages"],
        json!([
            {"role": "user", "text": "durable source before restart"},
            {"role": "assistant", "text": "source answer 0"},
        ])
    );
    assert_eq!(
        inputs[0]["boundaryId"],
        format!("{TEST_SESSION_ID}:stable-compaction-operation:before-compaction-0")
    );
    assert_eq!(
        evidence.requests.lock().unwrap().len(),
        2,
        "completed journal receipt suppresses the third host call"
    );
    assert_eq!(evidence.compactions.load(Ordering::SeqCst), 1);
    assert_eq!(
        journal.order.lock().unwrap().as_slice(),
        ["receipt_persisted"]
    );
    let receipts = journal.receipts.lock().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&receipts["before-compaction-0"])?["receiptId"],
        "durable-receipt-1"
    );
    Ok(())
}
