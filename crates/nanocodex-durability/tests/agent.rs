use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use eyre::{Result, eyre};
use nanocodex_agent::{
    ExecutionPolicyDisposition, Nanocodex, NanocodexError, OpenAi, PromptRequest, PromptRoute,
    ResponseError, Tools,
    events::{AgentEventKind, AgentEvents, RunStatus, RunTerminal},
    execution::{
        ExecutionAdmission, ExecutionFuture, ExecutionOutput, ExecutionPolicy,
        ExecutionStepAdmission,
    },
    input::Prompt,
    session::{SessionId, SessionSnapshot},
};
use serde_json::json;

use nanocodex_durability::{
    DurableAgentExt, DurableSession, MemoryStore, OperationStatus, OwnedState, OwnerId, OwnerToken,
    StateStore, StepStatus, StoreError, StoreFuture,
};

fn temporary_workspace(label: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("{label}-{}", SessionId::default()));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

fn test_session_id() -> SessionId {
    SessionId::default()
}

#[derive(Clone)]
struct CrashAtReplace {
    inner: MemoryStore,
    revision: u64,
    after_commit: bool,
    fired: Arc<AtomicBool>,
}

impl StateStore for CrashAtReplace {
    fn acquire<'a>(
        &'a mut self,
        id: &'a str,
        owner: OwnerId,
    ) -> StoreFuture<'a, std::result::Result<OwnedState, StoreError>> {
        self.inner.acquire(id, owner)
    }

    fn replace<'a>(
        &'a mut self,
        id: &'a str,
        owner: &'a OwnerToken,
        revision: u64,
        payload: &'a str,
    ) -> StoreFuture<'a, std::result::Result<u64, StoreError>> {
        Box::pin(async move {
            if revision == self.revision && !self.fired.swap(true, Ordering::SeqCst) {
                if self.after_commit {
                    self.inner.replace(id, owner, revision, payload).await?;
                    return Err(StoreError::Backend(
                        "lost acknowledgement after commit".into(),
                    ));
                }
                return Err(StoreError::NotCommitted(
                    "write rejected before commit".into(),
                ));
            }
            self.inner.replace(id, owner, revision, payload).await
        })
    }
}

#[tokio::test]
async fn every_first_turn_write_recovers_before_and_after_commit() -> Result<()> {
    for after_commit in [false, true] {
        // Admission, warmup intent/output, model intent/output, terminal.
        for revision in 0..6 {
            let store = CrashAtReplace {
                inner: MemoryStore::new()?,
                revision,
                after_commit,
                fired: Arc::new(AtomicBool::new(false)),
            };
            let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let openai = || {
                let generations = Arc::clone(&generations);
                OpenAi::builder("test-key")
                    .service(move || DurableReplayService {
                        generations: Arc::clone(&generations),
                    })
                    .build()
            };
            let workspace = temporary_workspace("commit-crash-matrix")?;
            let state = DurableSession::open(store.clone(), "crash-matrix").await?;
            let (agent, events) = Nanocodex::builder(openai()?)
                .workspace(&workspace)
                .durability(state)
                .await?
                .build()?;
            let request = || PromptRequest::new("exact crash recovery input").request_id("crashed");
            let first = match agent.prompt(request()).await {
                Ok(turn) => turn.result().await.map(|_| ()),
                Err(error) => Err(error),
            };
            assert!(
                store.fired.load(Ordering::SeqCst),
                "fault {revision}/{after_commit} was not reached"
            );
            let error = first.expect_err("lost/rejected writes cannot be acknowledged");
            assert!(
                matches!(
                    error.execution_policy_disposition(),
                    Some(ExecutionPolicyDisposition::Retry | ExecutionPolicyDisposition::Reopen)
                ),
                "{revision}/{after_commit}: {error}"
            );
            let _ = agent.shutdown().await;
            drop((agent, events));

            let state = DurableSession::open(store, "crash-matrix").await?;
            let (agent, events) = Nanocodex::builder(openai()?)
                .workspace(&workspace)
                .durability(state.clone())
                .await?
                .build()?;
            assert_eq!(
                agent
                    .prompt(request())
                    .await?
                    .result()
                    .await?
                    .final_message(),
                "durably replayed"
            );
            let before_replay = state.state().await?.revision();
            let calls_before_replay = generations.load(Ordering::SeqCst);
            assert_eq!(
                agent
                    .prompt(request())
                    .await?
                    .result()
                    .await?
                    .final_message(),
                "durably replayed"
            );
            assert_eq!(state.state().await?.revision(), before_replay);
            assert_eq!(generations.load(Ordering::SeqCst), calls_before_replay);
            agent
                .prompt(PromptRequest::new("new work after crash").request_id("next"))
                .await?
                .result()
                .await?;
            assert!(state.state().await?.pending_operations().is_empty());
            agent.shutdown().await?;
            drop((agent, events));
            std::fs::remove_dir_all(workspace)?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn six_hundred_turns_cross_retention_and_twenty_four_owner_changes() -> Result<()> {
    let store = MemoryStore::new()?;
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let workspace = temporary_workspace("long-durable-session")?;
    for epoch in 0..24 {
        let openai = OpenAi::builder("test-key")
            .service({
                let generations = Arc::clone(&generations);
                move || DurableReplayService {
                    generations: Arc::clone(&generations),
                }
            })
            .build()?;
        let state =
            DurableSession::open_with_terminal_receipt_limit(store.clone(), "long-session", 16)
                .await?;
        let (agent, events) = Nanocodex::builder(openai)
            .workspace(&workspace)
            .durability(state.clone())
            .await?
            .build()?;
        for index in (epoch * 25)..((epoch + 1) * 25) {
            let id = format!("turn-{index}");
            let input = format!("remember ordered turn {index}");
            let result = agent
                .prompt(PromptRequest::new(input.clone()).request_id(id.clone()))
                .await?
                .result()
                .await?;
            assert_eq!(result.final_message(), "durably replayed");
            let retained = state.state().await?;
            assert!(retained.pending_operations().is_empty());
            assert!(retained.operations().len() <= 16);
            assert!(
                retained
                    .operations()
                    .values()
                    .all(|operation| operation.steps.is_empty() && operation.steers.is_empty())
            );
            if index % 19 == 0 {
                let replay = agent
                    .prompt(PromptRequest::new(input).request_id(id))
                    .await?
                    .result()
                    .await?;
                assert_eq!(
                    serde_json::to_value(replay.snapshot())?,
                    serde_json::to_value(result.snapshot())?
                );
                assert_eq!(state.state().await?.revision(), retained.revision());
            }
        }
        agent.shutdown().await?;
        drop((agent, events));
    }
    assert_eq!(
        generations.load(Ordering::SeqCst),
        600,
        "replay and reopen must not regenerate completed work"
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[derive(Clone)]
struct FailReplaceOnce {
    inner: crate::MemoryStore,
    expected_revision: u64,
    failed: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone)]
struct CountingAcquires {
    inner: crate::MemoryStore,
    acquisitions: Arc<std::sync::Mutex<Vec<String>>>,
}

#[derive(Clone)]
struct GateFirstChildAcquire {
    inner: crate::MemoryStore,
    root_state_id: &'static str,
    gated: Arc<AtomicBool>,
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl crate::StateStore for GateFirstChildAcquire {
    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner_id: crate::OwnerId,
    ) -> crate::StoreFuture<'a, std::result::Result<crate::OwnedState, crate::StoreError>> {
        if state_id != self.root_state_id && !self.gated.swap(true, Ordering::SeqCst) {
            let started = Arc::clone(&self.started);
            let release = Arc::clone(&self.release);
            return Box::pin(async move {
                started.notify_one();
                release.notified().await;
                self.inner.acquire(state_id, owner_id).await
            });
        }
        self.inner.acquire(state_id, owner_id)
    }

    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a crate::OwnerToken,
        expected_revision: u64,
        payload: &'a str,
    ) -> crate::StoreFuture<'a, std::result::Result<u64, crate::StoreError>> {
        self.inner
            .replace(state_id, owner, expected_revision, payload)
    }
}

impl crate::StateStore for CountingAcquires {
    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner_id: crate::OwnerId,
    ) -> crate::StoreFuture<'a, std::result::Result<crate::OwnedState, crate::StoreError>> {
        self.acquisitions
            .lock()
            .expect("acquisition recorder lock is not poisoned")
            .push(state_id.to_owned());
        self.inner.acquire(state_id, owner_id)
    }

    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a crate::OwnerToken,
        expected_revision: u64,
        payload: &'a str,
    ) -> crate::StoreFuture<'a, std::result::Result<u64, crate::StoreError>> {
        self.inner
            .replace(state_id, owner, expected_revision, payload)
    }
}

#[derive(Clone)]
struct FailEntryOnce {
    inner: crate::MemoryStore,
    entry_tag: &'static str,
    operation_id: &'static str,
    failed: Arc<AtomicBool>,
}

impl crate::StateStore for FailEntryOnce {
    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner_id: crate::OwnerId,
    ) -> crate::StoreFuture<'a, std::result::Result<crate::OwnedState, crate::StoreError>> {
        self.inner.acquire(state_id, owner_id)
    }

    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a crate::OwnerToken,
        expected_revision: u64,
        payload: &'a str,
    ) -> crate::StoreFuture<'a, std::result::Result<u64, crate::StoreError>> {
        let state: serde_json::Value = serde_json::from_str(payload)
            .expect("durability fault injection receives a complete state value");
        let operation_status =
            &state["nanocodex_durable_state"]["operations"][self.operation_id]["status"];
        let matches_entry = match self.entry_tag {
            "\"operation_cancelled\"" => operation_status.get("cancelled").is_some(),
            "\"operation_completed\"" => operation_status.get("completed").is_some(),
            other => payload.contains(other),
        };
        if matches_entry && !self.failed.swap(true, Ordering::SeqCst) {
            return Box::pin(async {
                Err(crate::StoreError::NotCommitted(
                    "injected state replacement failure".to_owned(),
                ))
            });
        }
        self.inner
            .replace(state_id, owner, expected_revision, payload)
    }
}

#[derive(Clone)]
struct GateCompactionAuthorization {
    inner: crate::MemoryStore,
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl crate::StateStore for GateCompactionAuthorization {
    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner_id: crate::OwnerId,
    ) -> crate::StoreFuture<'a, std::result::Result<crate::OwnedState, crate::StoreError>> {
        self.inner.acquire(state_id, owner_id)
    }

    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a crate::OwnerToken,
        expected_revision: u64,
        payload: &'a str,
    ) -> crate::StoreFuture<'a, std::result::Result<u64, crate::StoreError>> {
        if payload.contains("\"status\":\"effect_pending\"")
            && payload.contains("\"kind\":\"compaction\"")
        {
            let started = Arc::clone(&self.started);
            let release = Arc::clone(&self.release);
            return Box::pin(async move {
                started.notify_one();
                release.notified().await;
                self.inner
                    .replace(state_id, owner, expected_revision, payload)
                    .await
            });
        }
        self.inner
            .replace(state_id, owner, expected_revision, payload)
    }
}

impl crate::StateStore for FailReplaceOnce {
    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner_id: crate::OwnerId,
    ) -> crate::StoreFuture<'a, std::result::Result<crate::OwnedState, crate::StoreError>> {
        self.inner.acquire(state_id, owner_id)
    }

    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a crate::OwnerToken,
        expected_revision: u64,
        payload: &'a str,
    ) -> crate::StoreFuture<'a, std::result::Result<u64, crate::StoreError>> {
        if expected_revision == self.expected_revision
            && !self.failed.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Box::pin(async {
                Err(crate::StoreError::NotCommitted(
                    "injected replacement failure".to_owned(),
                ))
            });
        }
        self.inner
            .replace(state_id, owner, expected_revision, payload)
    }
}

#[derive(Clone)]
struct DurableReplayService {
    generations: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Clone)]
struct ReplayContinuationService {
    generations: Arc<std::sync::atomic::AtomicUsize>,
}

struct GatedCompletedPolicy {
    snapshot: SessionSnapshot,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

struct FailClosedDefaultsPolicy {
    releases: Arc<std::sync::atomic::AtomicUsize>,
}

struct CountingDurableTool {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

struct BlockingDurableTool {
    started: Arc<tokio::sync::Notify>,
}

struct RecordedHiddenTool {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[nanocodex_tools::contract::async_trait]
impl nanocodex_agent::Tool for CountingDurableTool {
    fn definition(&self) -> nanocodex_tools::ToolDefinition {
        nanocodex_tools::ToolDefinition::function(
            "count_once",
            "Increment a test-side effect exactly once.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    async fn execute(
        &self,
        _input: nanocodex_tools::ToolInput,
        _context: nanocodex_tools::ToolContext<'_>,
    ) -> nanocodex_tools::ToolResult {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(nanocodex_tools::ToolOutput::text("counted"))
    }
}

#[nanocodex_tools::contract::async_trait]
impl nanocodex_agent::Tool for BlockingDurableTool {
    fn definition(&self) -> nanocodex_tools::ToolDefinition {
        nanocodex_tools::ToolDefinition::function(
            "count_once",
            "Block until the durable operation is cancelled.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    async fn execute(
        &self,
        _input: nanocodex_tools::ToolInput,
        _context: nanocodex_tools::ToolContext<'_>,
    ) -> nanocodex_tools::ToolResult {
        self.started.notify_one();
        std::future::pending().await
    }
}

#[nanocodex_tools::contract::async_trait]
impl nanocodex_agent::Tool for RecordedHiddenTool {
    fn definition(&self) -> nanocodex_tools::ToolDefinition {
        nanocodex_tools::ToolDefinition::function(
            "recorded_hidden_tool",
            "Return one result whose replay must not depend on the current tool catalog.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    async fn execute(
        &self,
        _input: nanocodex_tools::ToolInput,
        _context: nanocodex_tools::ToolContext<'_>,
    ) -> nanocodex_tools::ToolResult {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(nanocodex_tools::ToolOutput::from_json(
            json!({
                "receipt": "durably recorded"
            }),
            true,
        ))
    }
}

#[derive(Clone)]
struct DurableToolService {
    generations: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Clone)]
struct RemovedToolRecoveryService {
    generations: Arc<std::sync::atomic::AtomicUsize>,
    requests: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
}

#[derive(Clone)]
struct PendingGenerationService {
    started: Arc<AtomicBool>,
}

#[derive(Clone)]
struct GatedGenerationService {
    generations: Arc<std::sync::atomic::AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[derive(Clone)]
struct GatedWarmupService {
    warmups: Arc<std::sync::atomic::AtomicUsize>,
    generations: Arc<std::sync::atomic::AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[derive(Clone)]
struct DurableCompactionService;

#[derive(Clone)]
struct PendingStandaloneCompactionService {
    compactions: Arc<std::sync::atomic::AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
}

#[derive(Clone)]
struct AutomaticCompactionService {
    generations: Arc<std::sync::atomic::AtomicUsize>,
    compactions: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Clone)]
struct SteeredDurableService {
    generations: Arc<std::sync::atomic::AtomicUsize>,
    started: Arc<AtomicBool>,
    release_first: Arc<tokio::sync::Notify>,
    observed_steer: Arc<AtomicBool>,
}

fn unexpected_policy<T>() -> ExecutionFuture<'static, nanocodex_agent::Result<T>>
where
    T: 'static,
{
    Box::pin(async {
        Err(NanocodexError::InvalidExecutionPolicy(
            "unexpected test policy operation".to_owned(),
        ))
    })
}

impl ExecutionPolicy for GatedCompletedPolicy {
    fn admit<'a>(
        &'a self,
        _operation_id: String,
        _input_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<ExecutionAdmission>> {
        let snapshot = self.snapshot.clone();
        let entered = Arc::clone(&self.entered);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            Ok(ExecutionAdmission::Completed {
                snapshot,
                output: ExecutionOutput {
                    final_message: "retained terminal".to_owned(),
                    usage: nanocodex_agent::usage::TurnUsage::default(),
                },
            })
        })
    }

    fn admit_automatic<'a>(
        &'a self,
        candidate_operation_id: String,
        _input_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<(String, ExecutionAdmission)>> {
        let snapshot = self.snapshot.clone();
        let entered = Arc::clone(&self.entered);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            Ok((
                candidate_operation_id,
                ExecutionAdmission::Completed {
                    snapshot,
                    output: ExecutionOutput {
                        final_message: "retained terminal".to_owned(),
                        usage: nanocodex_agent::usage::TurnUsage::default(),
                    },
                },
            ))
        })
    }

    fn release<'a>(&'a self, _operation_id: String) -> ExecutionFuture<'a, ()> {
        Box::pin(async {})
    }

    fn cancel<'a>(
        &'a self,
        _operation_id: String,
        _snapshot: Option<SessionSnapshot>,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        unexpected_policy()
    }

    fn begin_attempt<'a>(
        &'a self,
        _operation_id: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        unexpected_policy()
    }

    fn begin_step<'a>(
        &'a self,
        _operation_id: String,
        _step_id: String,
        _kind: String,
        _input_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<ExecutionStepAdmission>> {
        unexpected_policy()
    }

    fn complete_step<'a>(
        &'a self,
        _operation_id: String,
        _step_id: String,
        _output_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        unexpected_policy()
    }

    fn complete<'a>(
        &'a self,
        _operation_id: String,
        _snapshot: SessionSnapshot,
        _output: ExecutionOutput,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        unexpected_policy()
    }

    fn fail_attempt<'a>(
        &'a self,
        _operation_id: String,
        _error: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        unexpected_policy()
    }

    fn fail<'a>(
        &'a self,
        _operation_id: String,
        _snapshot: SessionSnapshot,
        _error: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        unexpected_policy()
    }
}

impl ExecutionPolicy for FailClosedDefaultsPolicy {
    fn admit<'a>(
        &'a self,
        _operation_id: String,
        _input_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<ExecutionAdmission>> {
        unexpected_policy()
    }

    fn admit_automatic<'a>(
        &'a self,
        _candidate_operation_id: String,
        _input_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<(String, ExecutionAdmission)>> {
        unexpected_policy()
    }

    fn release<'a>(&'a self, _operation_id: String) -> ExecutionFuture<'a, ()> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }

    fn begin_attempt<'a>(
        &'a self,
        _operation_id: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        unexpected_policy()
    }

    fn begin_step<'a>(
        &'a self,
        _operation_id: String,
        _step_id: String,
        _kind: String,
        _input_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<ExecutionStepAdmission>> {
        unexpected_policy()
    }

    fn complete_step<'a>(
        &'a self,
        _operation_id: String,
        _step_id: String,
        _output_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        unexpected_policy()
    }

    fn complete<'a>(
        &'a self,
        _operation_id: String,
        _snapshot: SessionSnapshot,
        _output: ExecutionOutput,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        unexpected_policy()
    }

    fn fail_attempt<'a>(
        &'a self,
        _operation_id: String,
        _error: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        unexpected_policy()
    }

    fn fail<'a>(
        &'a self,
        _operation_id: String,
        _snapshot: SessionSnapshot,
        _error: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        unexpected_policy()
    }
}

impl tower::Service<nanocodex_oai_api::tower::ResponsesAttempt> for PendingGenerationService {
    type Response = nanocodex_oai_api::tower::ResponsesServiceResponse;
    type Error = ResponseError;
    type Future =
        Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: nanocodex_oai_api::tower::ResponsesAttempt) -> Self::Future {
        use nanocodex_oai_api::{
            responses::WarmupResponse,
            tower::{ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse},
        };
        match request.kind() {
            ResponsesAttemptKind::Warmup => Box::pin(async {
                Ok(ResponsesServiceResponse::new(ResponsesOutput::Warmup(
                    WarmupResponse {
                        id: "warmup".to_owned(),
                        usage: None,
                    },
                )))
            }),
            ResponsesAttemptKind::Generation => {
                self.started.store(true, Ordering::Release);
                Box::pin(std::future::pending())
            }
            ResponsesAttemptKind::Compaction => panic!("unexpected compaction request"),
            _ => panic!("unexpected Responses attempt kind"),
        }
    }
}

fn successful_attempt(
    kind: nanocodex_oai_api::tower::ResponsesAttemptKind,
) -> nanocodex_oai_api::tower::ResponsesServiceResponse {
    use nanocodex_oai_api::{
        responses::{ContentItem, MessageRole, ResponseItem, ResponseItemId, WarmupResponse},
        tower::{
            CompactionOutput, GenerationOutput, ResponsePipelineStats, ResponsesAttemptKind,
            ResponsesOutput, ResponsesServiceResponse,
        },
    };
    let output = match kind {
        ResponsesAttemptKind::Warmup => ResponsesOutput::Warmup(WarmupResponse {
            id: "warmup".to_owned(),
            usage: None,
        }),
        ResponsesAttemptKind::Generation => ResponsesOutput::Generation(GenerationOutput {
            id: "durable-response".to_owned(),
            status: "completed".to_owned(),
            end_turn: Some(true),
            final_message: Some("durably replayed".to_owned()),
            output_items: vec![ResponseItem::message(
                MessageRole::Assistant,
                [ContentItem::output_text("durably replayed")],
            )],
            code_calls: Vec::new(),
            usage: None,
            time_to_first_event_ns: 0,
            time_to_first_output_ns: None,
            pipeline_stats: ResponsePipelineStats::default(),
        }),
        ResponsesAttemptKind::Compaction => ResponsesOutput::Compaction(CompactionOutput {
            id: "durable-compaction".to_owned(),
            status: "completed".to_owned(),
            item: ResponseItem::Compaction {
                id: Some(ResponseItemId::from("cmp-durable")),
                encrypted_content: "retained-compaction".into(),
                created_by: None,
                internal_chat_message_metadata_passthrough: None,
            },
            usage: None,
            time_to_first_event_ns: 0,
            time_to_first_output_ns: None,
            pipeline_stats: ResponsePipelineStats::default(),
        }),
        kind => panic!("unexpected test attempt: {kind:?}"),
    };
    ResponsesServiceResponse::new(output)
}

impl tower::Service<nanocodex_oai_api::tower::ResponsesAttempt> for GatedGenerationService {
    type Response = nanocodex_oai_api::tower::ResponsesServiceResponse;
    type Error = ResponseError;
    type Future =
        Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: nanocodex_oai_api::tower::ResponsesAttempt) -> Self::Future {
        use nanocodex_oai_api::tower::ResponsesAttemptKind;
        let kind = request.kind();
        match kind {
            ResponsesAttemptKind::Generation => {
                let generation = self.generations.fetch_add(1, Ordering::SeqCst);
                let started = Arc::clone(&self.started);
                let release = Arc::clone(&self.release);
                Box::pin(async move {
                    if generation == 0 {
                        started.notify_one();
                        release.notified().await;
                    }
                    Ok(successful_attempt(ResponsesAttemptKind::Generation))
                })
            }
            kind => Box::pin(async move { Ok(successful_attempt(kind)) }),
        }
    }
}

impl tower::Service<nanocodex_oai_api::tower::ResponsesAttempt> for GatedWarmupService {
    type Response = nanocodex_oai_api::tower::ResponsesServiceResponse;
    type Error = ResponseError;
    type Future =
        Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: nanocodex_oai_api::tower::ResponsesAttempt) -> Self::Future {
        use nanocodex_oai_api::tower::ResponsesAttemptKind;
        let kind = request.kind();
        match kind {
            ResponsesAttemptKind::Warmup => {
                self.warmups.fetch_add(1, Ordering::SeqCst);
                let started = Arc::clone(&self.started);
                let release = Arc::clone(&self.release);
                Box::pin(async move {
                    started.notify_one();
                    release.notified().await;
                    Ok(successful_attempt(kind))
                })
            }
            ResponsesAttemptKind::Generation => {
                self.generations.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move { Ok(successful_attempt(kind)) })
            }
            _ => Box::pin(async move { Ok(successful_attempt(kind)) }),
        }
    }
}

impl tower::Service<nanocodex_oai_api::tower::ResponsesAttempt> for DurableCompactionService {
    type Response = nanocodex_oai_api::tower::ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = std::future::Ready<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: nanocodex_oai_api::tower::ResponsesAttempt) -> Self::Future {
        std::future::ready(Ok(successful_attempt(request.kind())))
    }
}

impl tower::Service<nanocodex_oai_api::tower::ResponsesAttempt>
    for PendingStandaloneCompactionService
{
    type Response = nanocodex_oai_api::tower::ResponsesServiceResponse;
    type Error = ResponseError;
    type Future =
        Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: nanocodex_oai_api::tower::ResponsesAttempt) -> Self::Future {
        use nanocodex_oai_api::tower::ResponsesAttemptKind;
        let kind = request.kind();
        match kind {
            ResponsesAttemptKind::Compaction => {
                let attempt = self.compactions.fetch_add(1, Ordering::SeqCst);
                self.started.notify_one();
                if attempt == 0 {
                    Box::pin(std::future::pending())
                } else {
                    Box::pin(async move { Ok(successful_attempt(kind)) })
                }
            }
            kind => Box::pin(async move { Ok(successful_attempt(kind)) }),
        }
    }
}

impl tower::Service<nanocodex_oai_api::tower::ResponsesAttempt> for AutomaticCompactionService {
    type Response = nanocodex_oai_api::tower::ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = std::future::Ready<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: nanocodex_oai_api::tower::ResponsesAttempt) -> Self::Future {
        use nanocodex_oai_api::{
            responses::{
                ContentItem, MessageRole, ResponseItem, ResponseItemId, Usage, WarmupResponse,
            },
            tower::{
                CompactionOutput, GenerationOutput, ResponsePipelineStats, ResponsesAttemptKind,
                ResponsesOutput, ResponsesServiceResponse,
            },
        };
        let output = match request.kind() {
            ResponsesAttemptKind::Warmup => ResponsesOutput::Warmup(WarmupResponse {
                id: "automatic-compaction-warmup".to_owned(),
                usage: None,
            }),
            ResponsesAttemptKind::Generation => {
                let call = self.generations.fetch_add(1, Ordering::SeqCst) + 1;
                let message = format!("automatic-generation-{call}");
                ResponsesOutput::Generation(GenerationOutput {
                    id: format!("automatic-generation-{call}"),
                    status: "completed".to_owned(),
                    end_turn: Some(true),
                    final_message: Some(message.clone()),
                    output_items: vec![ResponseItem::message(
                        MessageRole::Assistant,
                        [ContentItem::output_text(message)],
                    )],
                    code_calls: Vec::new(),
                    usage: Some(Usage {
                        total_tokens: if call == 1 { 244_800 } else { 120 },
                        ..Usage::default()
                    }),
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: None,
                    pipeline_stats: ResponsePipelineStats::default(),
                })
            }
            ResponsesAttemptKind::Compaction => {
                let call = self.compactions.fetch_add(1, Ordering::SeqCst);
                let label = if call == 0 { "A" } else { "B" };
                ResponsesOutput::Compaction(CompactionOutput {
                    id: format!("automatic-compaction-{label}"),
                    status: "completed".to_owned(),
                    item: ResponseItem::Compaction {
                        id: Some(ResponseItemId::from(format!("cmp-{label}"))),
                        encrypted_content: format!("compaction-{label}").into(),
                        created_by: None,
                        internal_chat_message_metadata_passthrough: None,
                    },
                    usage: Some(Usage {
                        total_tokens: 120,
                        ..Usage::default()
                    }),
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: None,
                    pipeline_stats: ResponsePipelineStats::default(),
                })
            }
            kind => panic!("unexpected automatic compaction attempt: {kind:?}"),
        };
        std::future::ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

impl tower::Service<nanocodex_oai_api::tower::ResponsesAttempt> for SteeredDurableService {
    type Response = nanocodex_oai_api::tower::ResponsesServiceResponse;
    type Error = ResponseError;
    type Future =
        Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: nanocodex_oai_api::tower::ResponsesAttempt) -> Self::Future {
        use nanocodex_oai_api::{
            responses::{ContentItem, MessageRole, ResponseItem, WarmupResponse},
            tower::{
                GenerationOutput, ResponsePipelineStats, ResponsesAttemptKind, ResponsesOutput,
                ResponsesServiceResponse,
            },
        };
        match request.kind() {
            ResponsesAttemptKind::Warmup => Box::pin(async {
                Ok(ResponsesServiceResponse::new(ResponsesOutput::Warmup(
                    WarmupResponse {
                        id: "warmup".to_owned(),
                        usage: None,
                    },
                )))
            }),
            ResponsesAttemptKind::Generation => {
                let generation = self.generations.fetch_add(1, Ordering::SeqCst);
                if generation == 0 {
                    self.started.store(true, Ordering::Release);
                    let release_first = Arc::clone(&self.release_first);
                    Box::pin(async move {
                        release_first.notified().await;
                        Ok(ResponsesServiceResponse::new(ResponsesOutput::Generation(
                            GenerationOutput {
                                id: "steer-boundary".to_owned(),
                                status: "completed".to_owned(),
                                end_turn: Some(false),
                                final_message: None,
                                output_items: Vec::new(),
                                code_calls: Vec::new(),
                                usage: None,
                                time_to_first_event_ns: 0,
                                time_to_first_output_ns: None,
                                pipeline_stats: ResponsePipelineStats::default(),
                            },
                        )))
                    })
                } else {
                    let observed = request.input_items().any(|item| {
                        serde_json::to_string(item)
                            .is_ok_and(|encoded| encoded.contains("retain this routed steer"))
                    });
                    self.observed_steer.store(observed, Ordering::Release);
                    Box::pin(async move {
                        Ok(ResponsesServiceResponse::new(ResponsesOutput::Generation(
                            GenerationOutput {
                                id: "steered-response".to_owned(),
                                status: "completed".to_owned(),
                                end_turn: Some(true),
                                final_message: Some("steer retained".to_owned()),
                                output_items: vec![ResponseItem::message(
                                    MessageRole::Assistant,
                                    [ContentItem::output_text("steer retained")],
                                )],
                                code_calls: Vec::new(),
                                usage: None,
                                time_to_first_event_ns: 0,
                                time_to_first_output_ns: None,
                                pipeline_stats: ResponsePipelineStats::default(),
                            },
                        )))
                    })
                }
            }
            kind => panic!("unexpected steered durable attempt: {kind:?}"),
        }
    }
}

impl tower::Service<nanocodex_oai_api::tower::ResponsesAttempt> for RemovedToolRecoveryService {
    type Response = nanocodex_oai_api::tower::ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = std::future::Ready<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: nanocodex_oai_api::tower::ResponsesAttempt) -> Self::Future {
        use nanocodex_oai_api::{
            responses::{
                ContentItem, FunctionOutputBody, MessageRole, ResponseItem, WarmupResponse,
            },
            tower::{
                CodeCall, CodeCallKind, GenerationOutput, ResponsePipelineStats,
                ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse,
            },
        };

        if matches!(request.kind(), ResponsesAttemptKind::Generation) {
            let items = request
                .input_items()
                .cloned()
                .map(|mut item| {
                    item.strip_id();
                    item
                })
                .collect::<Vec<_>>();
            self.requests
                .lock()
                .unwrap()
                .push(serde_json::to_value(items).unwrap());
        }
        let output = match request.kind() {
            ResponsesAttemptKind::Warmup => ResponsesOutput::Warmup(WarmupResponse {
                id: "warmup".to_owned(),
                usage: None,
            }),
            ResponsesAttemptKind::Generation => {
                let generation = self
                    .generations
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if generation == 0 {
                    let item = serde_json::from_value(json!({
                        "type": "function_call",
                        "call_id": "call-recorded-hidden-tool",
                        "name": "recorded_hidden_tool",
                        "arguments": "{}"
                    }))
                    .expect("recorded tool call item decodes");
                    ResponsesOutput::Generation(GenerationOutput {
                        id: "recorded-tool-response".to_owned(),
                        status: "completed".to_owned(),
                        end_turn: Some(false),
                        final_message: None,
                        output_items: vec![item],
                        code_calls: vec![CodeCall {
                            call_id: "call-recorded-hidden-tool".to_owned(),
                            name: "recorded_hidden_tool".to_owned(),
                            namespace: None,
                            input: "{}".to_owned(),
                            kind: CodeCallKind::Function,
                        }],
                        usage: None,
                        time_to_first_event_ns: 0,
                        time_to_first_output_ns: None,
                        pipeline_stats: ResponsePipelineStats::default(),
                    })
                } else {
                    let recovered_output = request.input_items().find_map(|item| match item {
                        ResponseItem::FunctionCallOutput {
                            call_id,
                            output: FunctionOutputBody::Text(output),
                            ..
                        } if &**call_id == "call-recorded-hidden-tool" => Some(output.as_ref()),
                        _ => None,
                    });
                    if request.model_call_index() != Some(1) {
                        let recovered_output = recovered_output
                            .expect("recovery must replay the completed tool result");
                        assert!(recovered_output.contains("durably recorded"));
                    }
                    ResponsesOutput::Generation(GenerationOutput {
                        id: "recovered-response".to_owned(),
                        status: "completed".to_owned(),
                        end_turn: Some(true),
                        final_message: Some("recovered with the recorded tool output".to_owned()),
                        output_items: vec![ResponseItem::message(
                            MessageRole::Assistant,
                            [ContentItem::output_text(
                                "recovered with the recorded tool output",
                            )],
                        )],
                        code_calls: Vec::new(),
                        usage: None,
                        time_to_first_event_ns: 0,
                        time_to_first_output_ns: None,
                        pipeline_stats: ResponsePipelineStats::default(),
                    })
                }
            }
            kind => panic!("unexpected recovered-spawn attempt: {kind:?}"),
        };
        std::future::ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

impl tower::Service<nanocodex_oai_api::tower::ResponsesAttempt> for DurableToolService {
    type Response = nanocodex_oai_api::tower::ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = std::future::Ready<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: nanocodex_oai_api::tower::ResponsesAttempt) -> Self::Future {
        use nanocodex_oai_api::{
            responses::{
                ContentItem, FunctionOutputBody, MessageRole, ResponseItem, WarmupResponse,
            },
            tower::{
                CodeCall, CodeCallKind, GenerationOutput, ResponsePipelineStats,
                ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse,
            },
        };
        let output = match request.kind() {
            ResponsesAttemptKind::Warmup => ResponsesOutput::Warmup(WarmupResponse {
                id: "warmup".to_owned(),
                usage: None,
            }),
            ResponsesAttemptKind::Generation => {
                let generation = self
                    .generations
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if generation == 0 {
                    let item = serde_json::from_value(json!({
                        "type": "function_call",
                        "call_id": "call-count-once",
                        "name": "count_once",
                        "arguments": "{}"
                    }))
                    .expect("durable tool call item decodes");
                    ResponsesOutput::Generation(GenerationOutput {
                        id: "durable-tool-response".to_owned(),
                        status: "completed".to_owned(),
                        end_turn: Some(false),
                        final_message: None,
                        output_items: vec![item],
                        code_calls: vec![CodeCall {
                            call_id: "call-count-once".to_owned(),
                            name: "count_once".to_owned(),
                            namespace: None,
                            input: "{}".to_owned(),
                            kind: CodeCallKind::Function,
                        }],
                        usage: None,
                        time_to_first_event_ns: 0,
                        time_to_first_output_ns: None,
                        pipeline_stats: ResponsePipelineStats::default(),
                    })
                } else {
                    let recovered_output = request.input_items().find_map(|item| match item {
                        ResponseItem::FunctionCallOutput {
                            call_id,
                            output: FunctionOutputBody::Text(output),
                            ..
                        } if &**call_id == "call-count-once" => Some(output.as_ref()),
                        _ => None,
                    });
                    assert!(
                        recovered_output
                            .expect("recovery must include the retried tool result")
                            .contains("counted")
                    );
                    ResponsesOutput::Generation(GenerationOutput {
                        id: "durable-tool-recovered-response".to_owned(),
                        status: "completed".to_owned(),
                        end_turn: Some(true),
                        final_message: Some("recovered after retrying the tool".to_owned()),
                        output_items: vec![ResponseItem::message(
                            MessageRole::Assistant,
                            [ContentItem::output_text(
                                "recovered after retrying the tool",
                            )],
                        )],
                        code_calls: Vec::new(),
                        usage: None,
                        time_to_first_event_ns: 0,
                        time_to_first_output_ns: None,
                        pipeline_stats: ResponsePipelineStats::default(),
                    })
                }
            }
            kind => panic!("unexpected durable tool attempt: {kind:?}"),
        };
        std::future::ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

impl tower::Service<nanocodex_oai_api::tower::ResponsesAttempt> for ReplayContinuationService {
    type Response = nanocodex_oai_api::tower::ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = std::future::Ready<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: nanocodex_oai_api::tower::ResponsesAttempt) -> Self::Future {
        use nanocodex_oai_api::{
            responses::{ContentItem, MessageRole, ResponseItem, WarmupResponse},
            tower::{
                CodeCall, CodeCallKind, GenerationOutput, ResponsePipelineStats,
                ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse,
            },
        };

        let output = match request.kind() {
            ResponsesAttemptKind::Warmup => ResponsesOutput::Warmup(WarmupResponse {
                id: "warmup".to_owned(),
                usage: None,
            }),
            ResponsesAttemptKind::Generation => {
                let generation = self.generations.fetch_add(1, Ordering::SeqCst);
                if generation == 0 {
                    let item = serde_json::from_value(json!({
                        "type": "function_call",
                        "call_id": "call-replay-fence",
                        "name": "count_once",
                        "arguments": "{}"
                    }))
                    .expect("replay-fence tool call item decodes");
                    ResponsesOutput::Generation(GenerationOutput {
                        id: "old-socket-response".to_owned(),
                        status: "completed".to_owned(),
                        end_turn: Some(false),
                        final_message: None,
                        output_items: vec![item],
                        code_calls: vec![CodeCall {
                            call_id: "call-replay-fence".to_owned(),
                            name: "count_once".to_owned(),
                            namespace: None,
                            input: "{}".to_owned(),
                            kind: CodeCallKind::Function,
                        }],
                        usage: None,
                        time_to_first_event_ns: 0,
                        time_to_first_output_ns: None,
                        pipeline_stats: ResponsePipelineStats::default(),
                    })
                } else {
                    assert_eq!(generation, 1, "recovery must make exactly one continuation");
                    assert_eq!(request.previous_response_id(), None);
                    assert!(
                        request.is_full_replay(),
                        "a replacement transport must replay authoritative typed history"
                    );
                    let input = request
                        .input_items()
                        .map(|item| serde_json::to_value(item).expect("request item encodes"))
                        .collect::<Vec<_>>();
                    let prompt_index = input
                        .iter()
                        .position(|item| item.to_string().contains("replay the response chain"))
                        .expect("full replay retains the original prompt");
                    let model_index = input
                        .iter()
                        .position(|item| {
                            item["type"] == "function_call"
                                && item["call_id"] == "call-replay-fence"
                        })
                        .expect("full replay retains the durable model output");
                    let tool_index = input
                        .iter()
                        .position(|item| {
                            item["type"] == "function_call_output"
                                && item["call_id"] == "call-replay-fence"
                                && item.to_string().contains("counted")
                        })
                        .expect("full replay retains the recovered tool result");
                    assert!(prompt_index < model_index && model_index < tool_index);
                    ResponsesOutput::Generation(GenerationOutput {
                        id: "replacement-socket-response".to_owned(),
                        status: "completed".to_owned(),
                        end_turn: Some(true),
                        final_message: Some("continued from full typed history".to_owned()),
                        output_items: vec![ResponseItem::message(
                            MessageRole::Assistant,
                            [ContentItem::output_text(
                                "continued from full typed history",
                            )],
                        )],
                        code_calls: Vec::new(),
                        usage: None,
                        time_to_first_event_ns: 0,
                        time_to_first_output_ns: None,
                        pipeline_stats: ResponsePipelineStats::default(),
                    })
                }
            }
            kind => panic!("unexpected replay-continuation attempt: {kind:?}"),
        };
        std::future::ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

impl tower::Service<nanocodex_oai_api::tower::ResponsesAttempt> for DurableReplayService {
    type Response = nanocodex_oai_api::tower::ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = std::future::Ready<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: nanocodex_oai_api::tower::ResponsesAttempt) -> Self::Future {
        use nanocodex_oai_api::responses::WarmupResponse;
        use nanocodex_oai_api::tower::{
            GenerationOutput, ResponsePipelineStats, ResponsesAttemptKind, ResponsesOutput,
            ResponsesServiceResponse,
        };
        let output = match request.kind() {
            ResponsesAttemptKind::Warmup => ResponsesOutput::Warmup(WarmupResponse {
                id: "warmup".to_owned(),
                usage: None,
            }),
            ResponsesAttemptKind::Generation => {
                self.generations
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ResponsesOutput::Generation(GenerationOutput {
                    id: "durable-response".to_owned(),
                    status: "completed".to_owned(),
                    end_turn: Some(true),
                    final_message: Some("durably replayed".to_owned()),
                    output_items: vec![nanocodex_oai_api::responses::ResponseItem::message(
                        nanocodex_oai_api::responses::MessageRole::Assistant,
                        [nanocodex_oai_api::responses::ContentItem::output_text(
                            "durably replayed",
                        )],
                    )],
                    code_calls: Vec::new(),
                    usage: None,
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: None,
                    pipeline_stats: ResponsePipelineStats::default(),
                })
            }
            kind => panic!("unexpected durable replay attempt: {kind:?}"),
        };
        std::future::ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

#[tokio::test]
async fn durability_attached_builder_is_safe_single_use_across_clones() -> Result<()> {
    let store = MemoryStore::new()?;
    let state = DurableSession::open(store, "single-use-builder").await?;
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = OpenAi::builder("test-key")
        .service({
            let generations = Arc::clone(&generations);
            move || DurableReplayService {
                generations: Arc::clone(&generations),
            }
        })
        .build()?;
    let workspace = temporary_workspace("durability-single-use-builder")?;
    let builder = Nanocodex::builder(openai)
        .workspace(&workspace)
        .durability(state)
        .await?;
    let duplicate = builder.clone();

    let (agent, events) = builder.build()?;
    let error = match duplicate.build() {
        Ok(_) => return Err(eyre!("a cloned attached builder built a second agent")),
        Err(error) => error,
    };
    assert!(error.to_string().contains("can build only one agent"));

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn configured_durability_automatically_persists_plain_prompts() -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let state = crate::DurableSession::open(store, "automatic-prompt").await?;
    let durable_state = state.clone();
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = OpenAi::builder("test-key")
        .service({
            let generations = Arc::clone(&generations);
            move || DurableReplayService {
                generations: Arc::clone(&generations),
            }
        })
        .build()?;
    let workspace = temporary_workspace("automatic-portable-durability")?;
    let builder = Nanocodex::builder(openai)
        .workspace(&workspace)
        .session_id(test_session_id())
        .durability(state)
        .await?;
    let (agent, events) = builder.build()?;

    let turn = agent.prompt("state this automatically").await?;
    let generated_request_id = turn
        .request_id()
        .ok_or_else(|| eyre!("automatic durable request ID is missing"))?
        .to_owned();
    let result = turn.result().await?;
    assert_eq!(result.final_message(), "durably replayed");
    assert_eq!(result.request_id(), Some(generated_request_id.as_str()));
    let state = durable_state.state().await?;
    assert_eq!(state.operations().len(), 1);
    let generated_id = state
        .operations()
        .keys()
        .next()
        .ok_or_else(|| eyre!("automatic durable operation is missing"))?;
    assert_eq!(generated_id, &generated_request_id);
    assert!(generated_request_id.parse::<SessionId>().is_ok());
    assert!(durable_state.latest_checkpoint().await?.is_some());

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn acknowledged_developer_context_survives_a_cold_reopen() -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let state = crate::DurableSession::open(store.clone(), "durable-developer-context").await?;
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = || {
        let generations = Arc::clone(&generations);
        OpenAi::builder("test-key")
            .service(move || DurableReplayService {
                generations: Arc::clone(&generations),
            })
            .build()
    };
    let workspace = temporary_workspace("durable-developer-context")?;
    let (agent, events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(state.clone())
        .await?
        .build()?;

    agent
        .append_developer_message("durable adapter marker")
        .await?;
    agent.shutdown().await?;
    drop((agent, events));

    let retained = state
        .latest_checkpoint()
        .await?
        .ok_or_else(|| eyre!("developer context was acknowledged without a checkpoint"))?;
    assert!(retained.json().contains("durable adapter marker"));

    let reopened = crate::DurableSession::open(store, "durable-developer-context").await?;
    let (resumed, resumed_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(reopened)
        .await?
        .build()?;
    assert!(resumed.context().await?.history().iter().any(|item| {
        serde_json::to_string(item).is_ok_and(|encoded| encoded.contains("durable adapter marker"))
    }));
    resumed.shutdown().await?;
    drop((resumed, resumed_events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn execution_policy_authority_defaults_fail_closed() -> Result<()> {
    let releases = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let policy = Arc::new(FailClosedDefaultsPolicy {
        releases: Arc::clone(&releases),
    });
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = OpenAi::builder("test-key")
        .service({
            let generations = Arc::clone(&generations);
            move || DurableReplayService {
                generations: Arc::clone(&generations),
            }
        })
        .build()?;
    let workspace = temporary_workspace("fail-closed-policy-defaults")?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .execution_policy(policy.clone())
        .build()?;

    assert!(matches!(
        agent.append_developer_message("must not acknowledge").await,
        Err(NanocodexError::ExecutionPolicyCapabilityUnsupported {
            capability: "commit_checkpoint"
        })
    ));
    assert!(matches!(
        ExecutionPolicy::cancel(policy.as_ref(), "turn".to_owned(), None).await,
        Err(NanocodexError::ExecutionPolicyCapabilityUnsupported {
            capability: "cancel"
        })
    ));
    assert_eq!(releases.load(Ordering::SeqCst), 0);
    assert_eq!(generations.load(Ordering::SeqCst), 0);

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn developer_context_during_an_active_turn_acks_only_after_durable_commit() -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let state = crate::DurableSession::open(store, "active-developer-context").await?;
    let started = Arc::new(AtomicBool::new(false));
    let openai = OpenAi::builder("test-key")
        .service({
            let started = Arc::clone(&started);
            move || PendingGenerationService {
                started: Arc::clone(&started),
            }
        })
        .build()?;
    let workspace = temporary_workspace("active-developer-context")?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .durability(state.clone())
        .await?
        .build()?;
    let turn = agent.prompt("hold this turn open").await?;
    while !started.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }

    let append_agent = agent.clone();
    let append = tokio::spawn(async move {
        append_agent
            .append_developer_message("active durable marker")
            .await
    });
    tokio::task::yield_now().await;
    assert!(
        !append.is_finished(),
        "active developer context must not acknowledge early"
    );

    turn.cancel().await?;
    assert!(matches!(
        turn.result().await,
        Err(NanocodexError::TurnCancelled)
    ));
    append.await??;
    assert!(
        state
            .latest_checkpoint()
            .await?
            .is_some_and(|checkpoint| checkpoint.json().contains("active durable marker"))
    );

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn queued_developer_context_waits_for_provider_retry_to_terminalize() -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let failing = FailReplaceOnce {
        inner: store.clone(),
        expected_revision: 4,
        failed: Arc::new(AtomicBool::new(false)),
    };
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let openai = || {
        let generations = Arc::clone(&generations);
        let started = Arc::clone(&started);
        let release = Arc::clone(&release);
        OpenAi::builder("test-key")
            .service(move || GatedGenerationService {
                generations: Arc::clone(&generations),
                started: Arc::clone(&started),
                release: Arc::clone(&release),
            })
            .build()
    };
    let workspace = temporary_workspace("developer-context-retry-barrier")?;
    let state = DurableSession::open(failing, "developer-context-retry-barrier").await?;
    let (agent, events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(state.clone())
        .await?
        .build()?;

    let turn = agent
        .prompt(PromptRequest::new("retry before developer context").request_id("retry-turn"))
        .await?;
    started.notified().await;
    let append = {
        let agent = agent.clone();
        tokio::spawn(async move {
            agent
                .append_developer_message("ordered developer marker")
                .await
        })
    };
    tokio::task::yield_now().await;
    assert!(!append.is_finished());
    release.notify_one();
    let first = turn
        .result()
        .await
        .expect_err("the first model-step settlement must be retryable");
    assert!(first.to_string().contains("injected replacement failure"));
    assert!(
        !append.is_finished(),
        "developer context must remain unacknowledged while the operation is pending"
    );

    let recovered = agent
        .prompt(PromptRequest::new("retry before developer context").request_id("retry-turn"))
        .await?
        .result()
        .await?;
    assert_eq!(recovered.final_message(), "durably replayed");
    append.await??;
    assert_eq!(
        generations.load(Ordering::SeqCst),
        2,
        "durable recovery must retry a provider effect whose output was not committed",
    );
    let checkpoint = state
        .latest_checkpoint()
        .await?
        .ok_or_else(|| eyre!("developer acknowledgment omitted its checkpoint"))?;
    assert!(checkpoint.json().contains("ordered developer marker"));

    agent.shutdown().await?;
    drop((agent, events));
    let reopened = DurableSession::open(store, "developer-context-retry-barrier").await?;
    let (resumed, resumed_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(reopened)
        .await?
        .build()?;
    assert!(resumed.context().await?.history().iter().any(|item| {
        serde_json::to_string(item).is_ok_and(|json| json.contains("ordered developer marker"))
    }));
    resumed.shutdown().await?;
    drop((resumed, resumed_events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn idle_routed_prompt_is_durably_admitted_and_checkpointed() -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let state = crate::DurableSession::open(store, "automatic-routed-prompt").await?;
    let durable_state = state.clone();
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = OpenAi::builder("test-key")
        .service({
            let generations = Arc::clone(&generations);
            move || DurableReplayService {
                generations: Arc::clone(&generations),
            }
        })
        .build()?;
    let workspace = temporary_workspace("automatic-routed-durability")?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .session_id(test_session_id())
        .durability(state)
        .await?
        .build()?;

    let turn = match agent.route_prompt("state this routed prompt").await? {
        PromptRoute::Started(turn) => turn,
        PromptRoute::Steered => return Err(eyre!("idle durable input unexpectedly steered")),
    };
    let accepted_request_id = turn
        .request_id()
        .ok_or_else(|| eyre!("routed durable turn is missing its request ID"))?
        .to_owned();
    let result = turn.result().await?;
    assert_eq!(result.final_message(), "durably replayed");
    let request_id = result
        .request_id()
        .ok_or_else(|| eyre!("routed durable result is missing its request ID"))?;
    assert_eq!(accepted_request_id, request_id);
    let state = durable_state.state().await?;
    assert!(
        state
            .operation(request_id)
            .is_some_and(|operation| operation.status.is_terminal())
    );
    assert!(durable_state.latest_checkpoint().await?.is_some());
    assert_eq!(generations.load(Ordering::SeqCst), 1);

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn cold_reopen_recovers_idle_routed_prompt_without_a_second_model_call() -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let failing_store = FailReplaceOnce {
        inner: store.clone(),
        expected_revision: 5,
        failed: Arc::new(AtomicBool::new(false)),
    };
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = || {
        let generations = Arc::clone(&generations);
        OpenAi::builder("test-key")
            .service(move || DurableReplayService {
                generations: Arc::clone(&generations),
            })
            .build()
    };
    let workspace = temporary_workspace("routed-durability-cold-reopen")?;

    let state = crate::DurableSession::open(failing_store, "routed-cold-reopen").await?;
    let (first, first_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .session_id(test_session_id())
        .durability(state)
        .await?
        .build()?;
    let first_turn = match first.route_prompt("recover routed input").await? {
        PromptRoute::Started(turn) => turn,
        PromptRoute::Steered => return Err(eyre!("idle durable input unexpectedly steered")),
    };
    let first_error = first_turn
        .result()
        .await
        .expect_err("the injected terminal replacement must fail the routed attempt");
    assert!(
        first_error
            .to_string()
            .contains("injected replacement failure")
    );
    first.shutdown().await?;
    drop((first, first_events));

    let state = crate::DurableSession::open(store, "routed-cold-reopen").await?;
    let (reopened, reopened_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .session_id(test_session_id())
        .durability(state.clone())
        .await?
        .build()?;
    let recovered_turn = match reopened.route_prompt("recover routed input").await? {
        PromptRoute::Started(turn) => turn,
        PromptRoute::Steered => return Err(eyre!("cold idle durable input unexpectedly steered")),
    };
    let recovered = recovered_turn.result().await?;
    assert_eq!(recovered.final_message(), "durably replayed");
    assert_eq!(
        generations.load(Ordering::SeqCst),
        1,
        "cold recovery must replay the durable model output",
    );
    assert!(state.latest_checkpoint().await?.is_some());

    reopened.shutdown().await?;
    drop((reopened, reopened_events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn active_routed_input_is_retained_in_the_durable_checkpoint() -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let state = crate::DurableSession::open(store, "active-routed-input").await?;
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let started = Arc::new(AtomicBool::new(false));
    let release_first = Arc::new(tokio::sync::Notify::new());
    let observed_steer = Arc::new(AtomicBool::new(false));
    let openai = OpenAi::builder("test-key")
        .service({
            let generations = Arc::clone(&generations);
            let started = Arc::clone(&started);
            let release_first = Arc::clone(&release_first);
            let observed_steer = Arc::clone(&observed_steer);
            move || SteeredDurableService {
                generations: Arc::clone(&generations),
                started: Arc::clone(&started),
                release_first: Arc::clone(&release_first),
                observed_steer: Arc::clone(&observed_steer),
            }
        })
        .build()?;
    let workspace = temporary_workspace("active-routed-durability")?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .session_id(test_session_id())
        .durability(state.clone())
        .await?
        .build()?;

    let turn = match agent.route_prompt("start durable routed turn").await? {
        PromptRoute::Started(turn) => turn,
        PromptRoute::Steered => return Err(eyre!("idle durable input unexpectedly steered")),
    };
    while !started.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        agent.route_prompt("retain this routed steer").await?,
        PromptRoute::Steered
    ));
    release_first.notify_one();
    assert_eq!(turn.result().await?.final_message(), "steer retained");
    assert!(observed_steer.load(Ordering::Acquire));
    assert_eq!(generations.load(Ordering::SeqCst), 2);
    let checkpoint = state
        .latest_checkpoint()
        .await?
        .ok_or_else(|| eyre!("active routed turn did not commit a checkpoint"))?
        .decode::<nanocodex_agent::session::SessionSnapshot>()?;
    assert!(serde_json::to_string(&checkpoint)?.contains("retain this routed steer"));

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn exact_id_retry_replays_steer_at_its_original_model_boundary() -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let failing = FailReplaceOnce {
        inner: store.clone(),
        expected_revision: 9,
        failed: Arc::new(AtomicBool::new(false)),
    };
    let state = crate::DurableSession::open(failing, "steered-exact-id-retry").await?;
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let started = Arc::new(AtomicBool::new(false));
    let release_first = Arc::new(tokio::sync::Notify::new());
    let observed_steer = Arc::new(AtomicBool::new(false));
    let openai = OpenAi::builder("test-key")
        .service({
            let generations = Arc::clone(&generations);
            let started = Arc::clone(&started);
            let release_first = Arc::clone(&release_first);
            let observed_steer = Arc::clone(&observed_steer);
            move || SteeredDurableService {
                generations: Arc::clone(&generations),
                started: Arc::clone(&started),
                release_first: Arc::clone(&release_first),
                observed_steer: Arc::clone(&observed_steer),
            }
        })
        .build()?;
    let workspace = temporary_workspace("steered-exact-id-retry")?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .session_id(test_session_id())
        .durability(state.clone())
        .await?
        .build()?;

    let first = agent
        .prompt(PromptRequest::new("start durable steered turn").request_id("steered-turn"))
        .await?;
    while !started.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
    first.steer("retain this routed steer").await?;
    release_first.notify_one();
    let error = first
        .result()
        .await
        .expect_err("the first terminal replacement must fail");
    assert!(error.to_string().contains("injected replacement failure"));
    let recovered = agent
        .prompt(PromptRequest::new("start durable steered turn").request_id("steered-turn"))
        .await?
        .result()
        .await?;
    assert_eq!(recovered.final_message(), "steer retained");
    assert!(observed_steer.load(Ordering::Acquire));
    assert_eq!(
        generations.load(Ordering::SeqCst),
        2,
        "retry must replay both completed model effects without changing their definitions"
    );
    let checkpoint = state
        .latest_checkpoint()
        .await?
        .ok_or_else(|| eyre!("steered retry did not commit a checkpoint"))?;
    assert!(checkpoint.json().contains("retain this routed steer"));

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn shutdown_reclaims_a_definitely_uncommitted_queued_terminalization() -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let failing = FailReplaceOnce {
        inner: store.clone(),
        expected_revision: 8,
        failed: Arc::new(AtomicBool::new(false)),
    };
    let started = Arc::new(AtomicBool::new(false));
    let openai = OpenAi::builder("test-key")
        .service({
            let started = Arc::clone(&started);
            move || PendingGenerationService {
                started: Arc::clone(&started),
            }
        })
        .build()?;
    let workspace = temporary_workspace("durable-shutdown-failure")?;
    let state = DurableSession::open(failing, "shutdown-failure").await?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .durability(state)
        .await?
        .build()?;
    let active = agent
        .prompt(PromptRequest::new("active").request_id("turn-1"))
        .await?;
    while !started.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
    let queued = agent
        .prompt(PromptRequest::new("queued").request_id("turn-2"))
        .await?;

    agent.shutdown().await?;
    assert!(matches!(
        active.result().await,
        Err(NanocodexError::TurnCancelled)
    ));
    assert!(queued.result().await.is_err());
    drop((agent, events));

    let reopened = DurableSession::open(store, "shutdown-failure").await?;
    let state = reopened.state().await?;
    assert!(
        state
            .operation("turn-1")
            .is_some_and(|operation| operation.status.is_terminal())
    );
    assert!(
        state
            .operation("turn-2")
            .is_some_and(|operation| operation.status.is_terminal())
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn durable_terminal_replays_emit_one_terminal_without_model_execution() -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let state = crate::DurableSession::open(store, "terminal-replay-events").await?;
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = || {
        let generations = Arc::clone(&generations);
        OpenAi::builder("test-key")
            .service(move || DurableReplayService {
                generations: Arc::clone(&generations),
            })
            .build()
    };
    let workspace = temporary_workspace("durable-terminal-replay-events")?;

    let (seed, seed_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(state.clone())
        .await?
        .build()?;
    let completed = seed
        .prompt(PromptRequest::new("completed replay").request_id("completed-replay"))
        .await?
        .result()
        .await?;
    let snapshot = completed
        .snapshot()
        .expect("local turns always retain a snapshot");
    assert_eq!(generations.load(std::sync::atomic::Ordering::SeqCst), 1);
    seed.shutdown().await?;
    drop((seed, seed_events));

    let failed_prompt = Prompt::from("failed replay");
    state.admit("failed-replay", &failed_prompt).await?;
    state.begin_attempt("failed-replay").await?;
    state
        .fail("failed-replay", &snapshot, "retained failure")
        .await?;
    let cancelled_prompt = Prompt::from("cancelled replay");
    state.admit("cancelled-replay", &cancelled_prompt).await?;
    state.cancel("cancelled-replay").await?;

    let (resumed, mut events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(state)
        .await?
        .build()?;

    let result = resumed
        .prompt(PromptRequest::new("completed replay").request_id("completed-replay"))
        .await?
        .result()
        .await?;
    assert_eq!(result.final_message(), "durably replayed");
    assert_replay_terminal(
        &mut events,
        AgentEventKind::RunCompleted,
        RunStatus::Completed,
    )?;

    let error = resumed
        .prompt(PromptRequest::new("failed replay").request_id("failed-replay"))
        .await?
        .result()
        .await
        .expect_err("failed terminal must replay its retained error");
    assert!(matches!(error, NanocodexError::ReplayedExecutionFailed(_)));
    assert_replay_terminal(&mut events, AgentEventKind::RunFailed, RunStatus::Failed)?;

    let error = resumed
        .prompt(PromptRequest::new("cancelled replay").request_id("cancelled-replay"))
        .await?
        .result()
        .await
        .expect_err("cancelled terminal must replay cancellation");
    assert!(matches!(error, NanocodexError::TurnCancelled));
    assert_replay_terminal(&mut events, AgentEventKind::RunFailed, RunStatus::Cancelled)?;

    assert_eq!(
        generations.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "terminal admission replay must not execute the model",
    );
    resumed.shutdown().await?;
    drop((resumed, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

fn assert_replay_terminal(
    events: &mut AgentEvents,
    expected_kind: AgentEventKind,
    expected_status: RunStatus,
) -> Result<()> {
    let replay_events = std::iter::from_fn(|| events.try_recv_timed()).collect::<Vec<_>>();
    assert_eq!(
        replay_events.len(),
        1,
        "a terminal admission replay must publish exactly one lifecycle event",
    );
    let event = &replay_events[0].event;
    assert_eq!(event.kind, expected_kind);
    assert_eq!(
        event.decode_payload::<RunTerminal>()?.status,
        expected_status
    );
    Ok(())
}

#[tokio::test]
async fn newer_agent_acquisition_fences_an_older_live_model_before_execution() -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let state = crate::DurableSession::open(store, "fenced-live-agents").await?;
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = || {
        let generations = Arc::clone(&generations);
        OpenAi::builder("test-key")
            .service(move || DurableReplayService {
                generations: Arc::clone(&generations),
            })
            .build()
    };
    let workspace = temporary_workspace("fenced-live-agents")?;

    let (older, older_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(state.clone())
        .await?
        .build()?;
    let (newer, newer_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(state.clone())
        .await?
        .build()?;

    let error = match older.prompt("stale owner must not execute").await {
        Ok(_) => panic!("the newer acquisition must fence the older Agent"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("model owner was fenced"));
    assert_eq!(
        generations.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a fenced owner must fail before starting a model effect",
    );
    let developer_error = older
        .append_developer_message("stale developer context")
        .await
        .expect_err("a stale owner must not acknowledge developer context");
    assert_eq!(
        developer_error.execution_policy_disposition(),
        Some(ExecutionPolicyDisposition::Reopen)
    );
    let compact_error = older
        .compact()
        .await
        .expect_err("a fenced owner must not start model-only compaction");
    assert_eq!(
        compact_error.execution_policy_disposition(),
        Some(ExecutionPolicyDisposition::Reopen)
    );
    assert_eq!(
        generations.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "fenced compaction must fail before calling the model service",
    );

    let result = newer
        .prompt("authoritative owner executes")
        .await?
        .result()
        .await?;
    assert_eq!(result.final_message(), "durably replayed");
    assert_eq!(generations.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(state.state().await?.operations().len(), 1);

    let _ = older.shutdown().await;
    newer.shutdown().await?;
    drop((older, older_events, newer, newer_events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn independent_session_takeover_fences_standalone_compaction_before_execution() -> Result<()>
{
    let store = crate::MemoryStore::new()?;
    let state = crate::DurableSession::open(store.clone(), "independent-compaction-fence").await?;
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = OpenAi::builder("test-key")
        .service({
            let generations = Arc::clone(&generations);
            move || DurableReplayService {
                generations: Arc::clone(&generations),
            }
        })
        .build()?;
    let workspace = temporary_workspace("independent-compaction-fence")?;
    let (older, events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .durability(state)
        .await?
        .build()?;

    let takeover = crate::DurableSession::open(store, "independent-compaction-fence").await?;
    let error = older
        .compact()
        .await
        .expect_err("the independently fenced owner must not enter compaction");
    assert_eq!(
        error.execution_policy_disposition(),
        Some(ExecutionPolicyDisposition::Reopen)
    );
    assert_eq!(
        generations.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "store-fenced compaction must fail before calling the model service",
    );

    let _ = older.shutdown().await;
    drop((older, events, takeover));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn cancelled_standalone_compaction_does_not_block_a_cold_follow_on() -> Result<()> {
    let store = MemoryStore::new()?;
    let compactions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let openai = || {
        let compactions = Arc::clone(&compactions);
        let started = Arc::clone(&started);
        OpenAi::builder("test-key")
            .service(move || PendingStandaloneCompactionService {
                compactions: Arc::clone(&compactions),
                started: Arc::clone(&started),
            })
            .build()
    };
    let workspace = temporary_workspace("standalone-compaction-cold-reopen")?;
    let state_id = "standalone-compaction-cold-reopen";
    let state = DurableSession::open(store.clone(), state_id).await?;
    let (agent, events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .instructions("original compaction instructions")
        .durability(state)
        .await?
        .build()?;
    agent
        .prompt("seed compaction input")
        .await?
        .result()
        .await?;

    let compacting = tokio::spawn({
        let agent = agent.clone();
        async move { agent.compact().await }
    });
    started.notified().await;
    assert_eq!(compactions.load(Ordering::SeqCst), 1);
    agent.shutdown().await?;
    assert!(matches!(
        compacting.await?,
        Err(NanocodexError::TurnCancelled)
    ));
    drop((agent, events));

    let reopened = DurableSession::open(store, state_id).await?;
    let retained = reopened.state().await?;
    assert!(retained.pending_operations().is_empty());
    let compaction = retained
        .operations()
        .iter()
        .find(|(_, operation)| {
            operation
                .steps
                .values()
                .any(|step| step.kind == "compaction")
        })
        .ok_or_else(|| eyre!("cancelled standalone compaction receipt was not retained"))?
        .1;
    assert!(matches!(
        compaction.status,
        OperationStatus::Cancelled { .. }
    ));
    let provider_step = compaction
        .steps
        .values()
        .find(|step| step.kind == "compaction")
        .ok_or_else(|| eyre!("pending compaction has no provider step"))?;
    assert!(matches!(provider_step.status, StepStatus::EffectPending));
    assert_eq!(provider_step.attempts, 1);
    drop(retained);

    let (resumed, resumed_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .instructions("updated compaction instructions")
        .durability(reopened)
        .await?
        .build()?;
    // A normal prompt must make progress without knowing that maintenance was
    // interrupted. This used to produce OperationBlocked forever.
    resumed
        .prompt("continue the conversation")
        .await?
        .result()
        .await?;
    assert_eq!(compactions.load(Ordering::SeqCst), 1);
    resumed.compact().await?;
    assert_eq!(
        compactions.load(Ordering::SeqCst),
        2,
        "a later explicit compaction is a fresh request"
    );

    resumed.shutdown().await?;
    drop((resumed, resumed_events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn live_replacement_resubmits_a_pending_standalone_compaction() -> Result<()> {
    let store = MemoryStore::new()?;
    let compactions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let openai = OpenAi::builder("test-key")
        .service({
            let compactions = Arc::clone(&compactions);
            let started = Arc::clone(&started);
            move || PendingStandaloneCompactionService {
                compactions: Arc::clone(&compactions),
                started: Arc::clone(&started),
            }
        })
        .build()?;
    let workspace = temporary_workspace("standalone-compaction-live-replacement")?;
    let state = DurableSession::open(store, "standalone-compaction-live-replacement").await?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .durability(state)
        .await?
        .build()?;
    agent
        .prompt("seed compaction input")
        .await?
        .result()
        .await?;

    let first = tokio::spawn({
        let agent = agent.clone();
        async move { agent.compact().await }
    });
    started.notified().await;
    assert_eq!(compactions.load(Ordering::SeqCst), 1);
    agent.compact().await?;
    assert!(matches!(first.await?, Err(NanocodexError::TurnCancelled)));
    assert_eq!(
        compactions.load(Ordering::SeqCst),
        2,
        "same-live replacement must resubmit an unfinished provider call"
    );

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn takeover_after_warmup_authorization_fences_the_result_not_the_in_flight_call() -> Result<()>
{
    let store = crate::MemoryStore::new()?;
    let state = DurableSession::open(store, "warmup-authorization-takeover").await?;
    let warmups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let openai = || {
        let warmups = Arc::clone(&warmups);
        let generations = Arc::clone(&generations);
        let started = Arc::clone(&started);
        let release = Arc::clone(&release);
        OpenAi::builder("test-key")
            .service(move || GatedWarmupService {
                warmups: Arc::clone(&warmups),
                generations: Arc::clone(&generations),
                started: Arc::clone(&started),
                release: Arc::clone(&release),
            })
            .build()
    };
    let workspace = temporary_workspace("warmup-authorization-takeover")?;
    let (older, older_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(state.clone())
        .await?
        .build()?;
    let turn = older.prompt("authorize warmup then take over").await?;
    started.notified().await;
    assert_eq!(warmups.load(Ordering::SeqCst), 1);

    let (newer, newer_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(state)
        .await?
        .build()?;
    release.notify_one();
    let error = turn
        .result()
        .await
        .expect_err("takeover must fence work after the authorized warmup returns");
    assert!(error.to_string().contains("model owner was fenced"));
    assert_eq!(
        generations.load(Ordering::SeqCst),
        0,
        "takeover must fence the next generation before transport entry"
    );

    older.shutdown().await?;
    newer.shutdown().await?;
    drop((older, older_events, newer, newer_events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn sequential_model_owners_preserve_history_and_cache_lineage() -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let state = crate::DurableSession::open(store, "sequential-model-owners").await?;
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = || {
        let generations = Arc::clone(&generations);
        OpenAi::builder("test-key")
            .service(move || DurableReplayService {
                generations: Arc::clone(&generations),
            })
            .build()
    };
    let workspace = temporary_workspace("sequential-model-owners")?;

    let (first, first_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(state.clone())
        .await?
        .build()?;
    first.prompt("first retained turn").await?.result().await?;
    let first_checkpoint = state
        .latest_checkpoint()
        .await?
        .ok_or_else(|| eyre!("first owner did not commit a checkpoint"))?
        .decode::<nanocodex_agent::session::SessionSnapshot>()?;
    let first_json = serde_json::to_value(&first_checkpoint)?;
    let cache_key = first_json["prompt_cache_key"]
        .as_str()
        .ok_or_else(|| eyre!("first checkpoint has no cache key"))?
        .to_owned();
    first.shutdown().await?;
    drop((first, first_events));

    let (second, second_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(state.clone())
        .await?
        .build()?;
    second
        .prompt("second retained turn")
        .await?
        .result()
        .await?;
    let second_checkpoint = state
        .latest_checkpoint()
        .await?
        .ok_or_else(|| eyre!("second owner did not commit a checkpoint"))?
        .decode::<nanocodex_agent::session::SessionSnapshot>()?;
    let second_json = serde_json::to_value(&second_checkpoint)?;
    assert_eq!(second_json["prompt_cache_key"], cache_key);
    let encoded = serde_json::to_string(&second_checkpoint)?;
    assert!(encoded.contains("first retained turn"));
    assert!(encoded.contains("second retained turn"));
    assert_eq!(generations.load(std::sync::atomic::Ordering::SeqCst), 2);

    second.shutdown().await?;
    drop((second, second_events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn failed_completed_compaction_persistence_restores_the_committed_live_boundary() -> Result<()>
{
    let store = MemoryStore::new()?;
    let failing = FailReplaceOnce {
        inner: store.clone(),
        expected_revision: 6,
        failed: Arc::new(AtomicBool::new(false)),
    };
    let openai = || {
        OpenAi::builder("test-key")
            .service(|| DurableCompactionService)
            .build()
    };
    let workspace = temporary_workspace("completed-compaction-rollback")?;
    let state = DurableSession::open(failing, "completed-compaction-rollback").await?;
    let (agent, events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(state)
        .await?
        .build()?;
    agent
        .prompt("seed committed boundary")
        .await?
        .result()
        .await?;
    let committed_history = serde_json::to_value(agent.context().await?.history())?;

    let error = agent
        .compact()
        .await
        .expect_err("the completed compaction checkpoint replacement must fail once");
    assert!(error.to_string().contains("injected replacement failure"));
    assert_eq!(
        serde_json::to_value(agent.context().await?.history())?,
        committed_history,
        "the live model must roll back to the last committed boundary"
    );
    agent.shutdown().await?;
    drop((agent, events));

    let reopened = DurableSession::open(store, "completed-compaction-rollback").await?;
    let (resumed, resumed_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(reopened)
        .await?
        .build()?;
    assert_eq!(
        serde_json::to_value(resumed.context().await?.history())?,
        committed_history,
        "cold reopen and the repaired live model must expose the same checkpoint"
    );
    resumed.shutdown().await?;
    drop((resumed, resumed_events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn active_cancel_reclaims_a_definitely_uncommitted_terminal_before_follow_on() -> Result<()> {
    let store = MemoryStore::new()?;
    let failing = FailEntryOnce {
        inner: store.clone(),
        entry_tag: "\"operation_cancelled\"",
        operation_id: "cancel-not-committed",
        failed: Arc::new(AtomicBool::new(false)),
    };
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let openai = OpenAi::builder("test-key")
        .service({
            let generations = Arc::clone(&generations);
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            move || GatedGenerationService {
                generations: Arc::clone(&generations),
                started: Arc::clone(&started),
                release: Arc::clone(&release),
            }
        })
        .build()?;
    let workspace = temporary_workspace("active-cancel-not-committed")?;
    let state = DurableSession::open(failing, "active-cancel-not-committed").await?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .durability(state)
        .await?
        .build()?;
    let turn = agent
        .prompt(PromptRequest::new("cancel after admission").request_id("cancel-not-committed"))
        .await?;
    started.notified().await;
    let follow_on = agent
        .prompt(PromptRequest::new("continue after cancellation").request_id("active-follow-on"))
        .await?;

    turn.cancel().await?;
    assert!(matches!(
        turn.result().await,
        Err(NanocodexError::TurnCancelled)
    ));
    let followed = tokio::time::timeout(Duration::from_secs(2), follow_on.result())
        .await
        .expect("the follow-on must not strand behind the released cancellation claim")?;
    assert_eq!(followed.final_message(), "durably replayed");

    agent.shutdown().await?;
    drop((agent, events));
    let reopened = DurableSession::open(store, "active-cancel-not-committed").await?;
    assert!(
        reopened
            .state()
            .await?
            .operation("cancel-not-committed")
            .is_some_and(|operation| operation.status.is_terminal()),
        "the exact cancellation retry must commit before the follow-on runs"
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn active_cancel_does_not_invent_an_outcome_for_an_unfinished_tool() -> Result<()> {
    let store = MemoryStore::new()?;
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tool_started = Arc::new(tokio::sync::Notify::new());
    let openai = OpenAi::builder("test-key")
        .service({
            let generations = Arc::clone(&generations);
            move || DurableToolService {
                generations: Arc::clone(&generations),
            }
        })
        .build()?;
    let tools = Tools::builder()
        .without_defaults()
        .tool(BlockingDurableTool {
            started: Arc::clone(&tool_started),
        })
        .build()?;
    let workspace = temporary_workspace("active-cancel-pending-tool")?;
    let state_id = "active-cancel-pending-tool";
    let state = DurableSession::open(store.clone(), state_id).await?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(tools)
        .durability(state)
        .await?
        .build()?;
    let turn = agent
        .prompt(PromptRequest::new("run the blocker").request_id("cancel-never-tool"))
        .await?;
    tool_started.notified().await;

    turn.cancel().await?;
    assert!(matches!(
        turn.result().await,
        Err(NanocodexError::TurnCancelled)
    ));
    let cancellation_events = std::iter::from_fn(|| events.try_recv_timed()).collect::<Vec<_>>();
    let tool_results = cancellation_events
        .iter()
        .enumerate()
        .filter(|(_, event)| event.event.kind == AgentEventKind::ToolResult)
        .collect::<Vec<_>>();
    assert_eq!(
        tool_results.len(),
        1,
        "cancellation must emit the cancelled live tool result exactly once"
    );
    let (tool_result_index, tool_result) = tool_results[0];
    let tool_result = tool_result.event.decode_payload::<serde_json::Value>()?;
    assert_eq!(tool_result["call_id"], "call-count-once");
    assert_eq!(tool_result["status"], "cancelled");
    let emitted_duration_ns = tool_result["duration_ns"]
        .as_u64()
        .expect("the cancelled tool result retains its elapsed duration");
    assert!(
        emitted_duration_ns > 0,
        "active cancellation must not reset elapsed tool work"
    );
    let run_error_index = cancellation_events
        .iter()
        .position(|event| event.event.kind == AgentEventKind::RunError)
        .expect("explicit cancellation emits RunError");
    assert!(
        tool_result_index < run_error_index,
        "the live tool result must precede the cancellation error"
    );
    agent.shutdown().await?;
    drop((agent, events));

    let reopened = DurableSession::open(store, state_id).await?;
    let state = reopened.state().await?;
    let operation = state
        .operation("cancel-never-tool")
        .expect("cancelled operation remains retained");
    let checkpoint = match &operation.status {
        OperationStatus::Cancelled {
            checkpoint: Some(checkpoint),
        } => checkpoint,
        status => panic!("expected terminal cancellation checkpoint, found {status:?}"),
    };
    let tool_step = operation
        .steps
        .values()
        .find(|step| step.kind == "tool_call")
        .expect("the unfinished tool step remains retained");
    assert!(matches!(tool_step.status, StepStatus::EffectPending));
    assert_eq!(tool_step.attempts, 1);
    assert!(
        !checkpoint.json().contains("external outcome"),
        "cancellation must not invent a synthetic tool outcome"
    );

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn queued_cancel_reclaims_a_definitely_uncommitted_terminal_before_follow_on() -> Result<()> {
    let store = MemoryStore::new()?;
    let failing = FailEntryOnce {
        inner: store,
        entry_tag: "\"operation_cancelled\"",
        operation_id: "queued-cancel-not-committed",
        failed: Arc::new(AtomicBool::new(false)),
    };
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let openai = OpenAi::builder("test-key")
        .service({
            let generations = Arc::clone(&generations);
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            move || GatedGenerationService {
                generations: Arc::clone(&generations),
                started: Arc::clone(&started),
                release: Arc::clone(&release),
            }
        })
        .build()?;
    let workspace = temporary_workspace("queued-cancel-not-committed")?;
    let state = DurableSession::open(failing, "queued-cancel-not-committed").await?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .durability(state)
        .await?
        .build()?;
    let active = agent
        .prompt(PromptRequest::new("active predecessor").request_id("queued-predecessor"))
        .await?;
    started.notified().await;
    let cancelled = agent
        .prompt(PromptRequest::new("cancel while queued").request_id("queued-cancel-not-committed"))
        .await?;
    let follow_on = agent
        .prompt(PromptRequest::new("run after queued cancellation").request_id("queued-follow-on"))
        .await?;

    cancelled.cancel().await?;
    release.notify_one();
    active.result().await?;
    assert!(matches!(
        cancelled.result().await,
        Err(NanocodexError::TurnCancelled)
    ));
    let followed = tokio::time::timeout(Duration::from_secs(2), follow_on.result())
        .await
        .expect("the follow-on must not strand behind a claimless queued command")?;
    assert_eq!(followed.final_message(), "durably replayed");

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn automatic_compaction_replays_a_after_terminal_not_committed_instead_of_running_b()
-> Result<()> {
    let store = MemoryStore::new()?;
    let failing = FailEntryOnce {
        inner: store,
        entry_tag: "\"operation_completed\"",
        operation_id: "compaction-terminal-retry",
        failed: Arc::new(AtomicBool::new(false)),
    };
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let compactions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = OpenAi::builder("test-key")
        .service({
            let generations = Arc::clone(&generations);
            let compactions = Arc::clone(&compactions);
            move || AutomaticCompactionService {
                generations: Arc::clone(&generations),
                compactions: Arc::clone(&compactions),
            }
        })
        .build()?;
    let workspace = temporary_workspace("automatic-compaction-terminal-retry")?;
    let state = DurableSession::open(failing, "automatic-compaction-terminal-retry").await?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .durability(state)
        .await?
        .build()?;
    agent
        .prompt(PromptRequest::new("seed high usage").request_id("compaction-seed"))
        .await?
        .result()
        .await?;

    let first = match agent
        .prompt(
            PromptRequest::new("reuse the exact compaction")
                .request_id("compaction-terminal-retry"),
        )
        .await
    {
        Ok(turn) => turn
            .result()
            .await
            .expect_err("the first terminal write must be definitely uncommitted"),
        Err(error) => error,
    };
    assert!(
        first
            .to_string()
            .contains("injected state replacement failure")
    );
    assert_eq!(compactions.load(Ordering::SeqCst), 1);
    assert_eq!(generations.load(Ordering::SeqCst), 2);

    let recovered = agent
        .prompt(
            PromptRequest::new("reuse the exact compaction")
                .request_id("compaction-terminal-retry"),
        )
        .await?
        .result()
        .await?;
    assert_eq!(recovered.final_message(), "automatic-generation-2");
    assert_eq!(
        compactions.load(Ordering::SeqCst),
        1,
        "the exact-ID retry must replay compaction A instead of calling the provider for B"
    );
    assert_eq!(
        generations.load(Ordering::SeqCst),
        2,
        "the generation after compaction must also replay after terminal NotCommitted"
    );
    let history = serde_json::to_string(agent.context().await?.history())?;
    assert!(history.contains("compaction-A"));
    assert!(!history.contains("compaction-B"));

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn takeover_during_automatic_compaction_authorization_fences_before_provider_entry()
-> Result<()> {
    let store = MemoryStore::new()?;
    let authorization_started = Arc::new(tokio::sync::Notify::new());
    let authorization_release = Arc::new(tokio::sync::Notify::new());
    let gated = GateCompactionAuthorization {
        inner: store.clone(),
        started: Arc::clone(&authorization_started),
        release: Arc::clone(&authorization_release),
    };
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let compactions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = || {
        let generations = Arc::clone(&generations);
        let compactions = Arc::clone(&compactions);
        OpenAi::builder("test-key")
            .service(move || AutomaticCompactionService {
                generations: Arc::clone(&generations),
                compactions: Arc::clone(&compactions),
            })
            .build()
    };
    let workspace = temporary_workspace("automatic-compaction-takeover")?;
    let older_state = DurableSession::open(gated, "automatic-compaction-takeover").await?;
    let (older, older_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(older_state)
        .await?
        .build()?;
    older
        .prompt(PromptRequest::new("seed high usage").request_id("takeover-seed"))
        .await?
        .result()
        .await?;
    let interrupted = older
        .prompt(
            PromptRequest::new("compact only with fresh authority")
                .request_id("takeover-compaction"),
        )
        .await?;
    authorization_started.notified().await;

    let newer_state = DurableSession::open(store, "automatic-compaction-takeover").await?;
    let (newer, newer_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .durability(newer_state)
        .await?
        .build()?;
    authorization_release.notify_one();
    let fenced = interrupted
        .result()
        .await
        .expect_err("takeover must reject the stale compaction authorization");
    assert!(fenced.to_string().contains("owner was fenced"));
    assert_eq!(
        compactions.load(Ordering::SeqCst),
        0,
        "the stale owner must fail before entering the compaction provider call"
    );

    newer
        .prompt(
            PromptRequest::new("compact only with fresh authority")
                .request_id("takeover-compaction"),
        )
        .await?
        .result()
        .await?;
    assert_eq!(
        compactions.load(Ordering::SeqCst),
        0,
        "a cold recovery may recompute below the proactive threshold, but must never inherit the stale owner's provider admission"
    );
    assert_eq!(generations.load(Ordering::SeqCst), 2);

    let _ = older.shutdown().await;
    newer.shutdown().await?;
    drop((older, older_events, newer, newer_events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

async fn assert_cold_model_replay_forces_full_history(store_responses: bool) -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let failing_store = FailReplaceOnce {
        inner: store.clone(),
        expected_revision: 5,
        failed: Arc::new(AtomicBool::new(false)),
    };
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tool_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = || {
        let generations = Arc::clone(&generations);
        OpenAi::builder("test-key")
            .store(store_responses)
            .service(move || ReplayContinuationService {
                generations: Arc::clone(&generations),
            })
            .build()
    };
    let tools = || {
        Tools::builder()
            .without_defaults()
            .tool(CountingDurableTool {
                calls: Arc::clone(&tool_calls),
            })
            .build()
    };
    let suffix = if store_responses {
        "stored"
    } else {
        "ephemeral"
    };
    let state_id = format!("cold-model-replay-{suffix}");
    let workspace = temporary_workspace(&state_id)?;

    let state = crate::DurableSession::open(failing_store, state_id.clone()).await?;
    let (first, first_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(tools()?)
        .durability(state)
        .await?
        .build()?;
    let error = first
        .prompt(
            PromptRequest::new("replay the response chain safely")
                .request_id("response-chain-turn"),
        )
        .await?
        .result()
        .await
        .expect_err("the injected tool-step replacement must fail the first owner");
    assert!(error.to_string().contains("injected replacement failure"));
    assert_eq!(generations.load(Ordering::SeqCst), 1);
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0);
    first.shutdown().await?;
    drop((first, first_events));

    let state = crate::DurableSession::open(store, state_id).await?;
    let (reopened, reopened_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(tools()?)
        .durability(state)
        .await?
        .build()?;
    let result = reopened
        .prompt(
            PromptRequest::new("replay the response chain safely")
                .request_id("response-chain-turn"),
        )
        .await?
        .result()
        .await?;
    assert_eq!(result.final_message(), "continued from full typed history");
    assert_eq!(generations.load(Ordering::SeqCst), 2);
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1);

    reopened.shutdown().await?;
    drop((reopened, reopened_events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn cold_model_step_replay_never_reuses_the_replaced_transport_chain() -> Result<()> {
    assert_cold_model_replay_forces_full_history(false).await?;
    assert_cold_model_replay_forces_full_history(true).await
}

#[tokio::test]
async fn abandoned_terminal_replay_acceptance_emits_no_terminal_event() -> Result<()> {
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = || {
        let generations = Arc::clone(&generations);
        OpenAi::builder("test-key")
            .service(move || DurableReplayService {
                generations: Arc::clone(&generations),
            })
            .build()
    };
    let workspace = temporary_workspace("abandoned-terminal-replay")?;
    let (seed, seed_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .build()?;
    let snapshot = seed
        .prompt("seed replay snapshot")
        .await?
        .result()
        .await?
        .snapshot()
        .expect("local turns always retain a snapshot");
    seed.shutdown().await?;
    drop((seed, seed_events));

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let policy = Arc::new(GatedCompletedPolicy {
        snapshot,
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    let (agent, mut events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .execution_policy(policy)
        .build()?;
    let abandoned = {
        let agent = agent.clone();
        tokio::spawn(async move {
            agent
                .prompt(PromptRequest::new("retained terminal").request_id("retained-turn"))
                .await
        })
    };
    entered.notified().await;
    abandoned.abort();
    let _ = abandoned.await;
    release.notify_one();

    // This command is ordered behind the abandoned prompt and proves the
    // driver has finished processing its terminal admission.
    agent.set_fast_mode(false).await?;
    while let Some(event) = events.try_recv_timed() {
        assert!(
            !event.event.kind.is_terminal(),
            "a prompt whose caller never accepted it must not publish a terminal event"
        );
    }
    assert_eq!(
        generations.load(Ordering::SeqCst),
        1,
        "terminal replay must not execute the model"
    );

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn abandoned_routed_terminal_replay_emits_no_terminal_event() -> Result<()> {
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = || {
        let generations = Arc::clone(&generations);
        OpenAi::builder("test-key")
            .service(move || DurableReplayService {
                generations: Arc::clone(&generations),
            })
            .build()
    };
    let workspace = temporary_workspace("abandoned-routed-terminal-replay")?;
    let (seed, seed_events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .build()?;
    let snapshot = seed
        .prompt("seed routed replay snapshot")
        .await?
        .result()
        .await?
        .snapshot()
        .expect("local turns always retain a snapshot");
    seed.shutdown().await?;
    drop((seed, seed_events));

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let policy = Arc::new(GatedCompletedPolicy {
        snapshot,
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    let (agent, mut events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .execution_policy(policy)
        .build()?;
    let abandoned = {
        let agent = agent.clone();
        tokio::spawn(async move { agent.route_prompt("retained routed terminal").await })
    };
    entered.notified().await;
    abandoned.abort();
    let _ = abandoned.await;
    release.notify_one();

    agent.set_fast_mode(false).await?;
    while let Some(event) = events.try_recv_timed() {
        assert!(
            !event.event.kind.is_terminal(),
            "a routed prompt whose caller never accepted it must not publish a terminal event"
        );
    }
    assert_eq!(
        generations.load(Ordering::SeqCst),
        1,
        "routed terminal replay must not execute the model"
    );

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn follow_on_turn_replays_after_cold_reopen() -> Result<()> {
    let store = MemoryStore::new()?;
    let failing_store = FailEntryOnce {
        inner: store.clone(),
        entry_tag: "\"operation_completed\"",
        operation_id: "second-turn",
        failed: Arc::new(AtomicBool::new(false)),
    };
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = || {
        let generations = Arc::clone(&generations);
        OpenAi::builder("test-key")
            .service(move || DurableReplayService {
                generations: Arc::clone(&generations),
            })
            .build()
    };
    let workspace = temporary_workspace("durable-follow-on-replay")?;
    let state = DurableSession::open(failing_store, "follow-on-replay").await?;
    let (agent, events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .execution_environment(nanocodex_agent::ExecutionEnvironment::new(
            "2026-09-03",
            "Etc/UTC",
        ))
        .durability(state)
        .await?
        .build()?;
    agent
        .prompt(PromptRequest::new("first").request_id("first-turn"))
        .await?
        .result()
        .await?;
    let error = agent
        .prompt(PromptRequest::new("second").request_id("second-turn"))
        .await?
        .result()
        .await
        .expect_err("second turn settlement must fail");
    assert!(
        error
            .to_string()
            .contains("injected state replacement failure")
    );
    agent.shutdown().await?;
    drop((agent, events));

    let state = DurableSession::open(store, "follow-on-replay").await?;
    let (agent, events) = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .execution_environment(nanocodex_agent::ExecutionEnvironment::new(
            "2026-09-04",
            "Etc/UTC",
        ))
        .durability(state)
        .await?
        .build()?;
    let result = agent
        .prompt(PromptRequest::new("second").request_id("second-turn"))
        .await?
        .result()
        .await?;
    assert_eq!(result.final_message(), "durably replayed");
    assert_eq!(generations.load(Ordering::SeqCst), 2);
    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn portable_state_replays_a_completed_model_step_after_terminal_commit_failure() -> Result<()>
{
    let store = crate::MemoryStore::new()?;
    let failing_store = FailReplaceOnce {
        inner: store.clone(),
        expected_revision: 5,
        failed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = || {
        let generations = Arc::clone(&generations);
        OpenAi::builder("test-key")
            .service(move || DurableReplayService {
                generations: Arc::clone(&generations),
            })
            .build()
    };
    let workspace = temporary_workspace("portable-durability-model-replay")?;
    let state = crate::DurableSession::open(failing_store, "portable-model-replay").await?;
    let builder = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .session_id(test_session_id())
        .durability(state)
        .await?;
    let (agent, mut events) = builder.build()?;
    let first_turn = agent.prompt("replay this exact turn").await?;
    let first_request_id = first_turn
        .request_id()
        .ok_or_else(|| eyre!("first durable request ID is missing"))?
        .to_owned();
    let error = first_turn
        .result()
        .await
        .expect_err("the injected terminal replacement must fail the first attempt");
    assert!(error.to_string().contains("injected replacement failure"));
    let terminals = std::iter::from_fn(|| events.try_recv_timed())
        .filter(|event| event.event.kind.is_terminal())
        .collect::<Vec<_>>();
    assert_eq!(
        terminals.len(),
        1,
        "an accepted turn must publish exactly one terminal event even when settlement fails"
    );
    assert_eq!(terminals[0].event.kind, AgentEventKind::RunFailed);
    agent.shutdown().await?;
    drop((agent, events));

    let state = crate::DurableSession::open(store, "portable-model-replay").await?;
    let builder = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .session_id(test_session_id())
        .durability(state)
        .await?;
    let (resumed, resumed_events) = builder.build()?;
    let recovered_turn = resumed.prompt("replay this exact turn").await?;
    assert_eq!(recovered_turn.request_id(), Some(first_request_id.as_str()));
    let result = recovered_turn.result().await?;
    assert_eq!(result.request_id(), Some(first_request_id.as_str()));
    assert_eq!(result.final_message(), "durably replayed");
    assert_eq!(
        generations.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the recovered operation must use the Rust-durable model output",
    );
    resumed.shutdown().await?;
    drop((resumed, resumed_events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn exact_id_retry_reclaims_a_definitely_uncommitted_terminal_replace() -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let failing_store = FailReplaceOnce {
        inner: store,
        expected_revision: 5,
        failed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = {
        let generations = Arc::clone(&generations);
        OpenAi::builder("test-key")
            .service(move || DurableReplayService {
                generations: Arc::clone(&generations),
            })
            .build()?
    };
    let workspace = temporary_workspace("portable-durability-live-retry")?;
    let state = crate::DurableSession::open(failing_store, "portable-model-live-retry").await?;
    let builder = Nanocodex::builder(openai)
        .workspace(&workspace)
        .session_id(test_session_id())
        .durability(state)
        .await?;
    let (agent, events) = builder.build()?;

    let first = agent
        .prompt(PromptRequest::new("replay this exact turn").request_id("exact-live-retry"))
        .await?
        .result()
        .await
        .expect_err("the injected terminal replacement must fail the first attempt");
    assert!(first.to_string().contains("injected replacement failure"));

    let recovered = agent
        .prompt(PromptRequest::new("replay this exact turn").request_id("exact-live-retry"))
        .await?
        .result()
        .await?;
    assert_eq!(recovered.final_message(), "durably replayed");
    assert_eq!(
        generations.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the live owner must roll back before replaying the durable model output",
    );

    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn portable_state_retries_an_unfinished_tool() -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let failing_store = FailReplaceOnce {
        inner: store.clone(),
        expected_revision: 6,
        failed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tool_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = || {
        let generations = Arc::clone(&generations);
        OpenAi::builder("test-key")
            .service(move || DurableToolService {
                generations: Arc::clone(&generations),
            })
            .build()
    };
    let tools = || {
        Tools::builder()
            .without_defaults()
            .tool(CountingDurableTool {
                calls: Arc::clone(&tool_calls),
            })
            .build()
    };
    let workspace = temporary_workspace("portable-durability-ambiguous-tool")?;
    let state = crate::DurableSession::open(failing_store, "ambiguous-tool").await?;
    let builder = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(tools()?)
        .durability(state)
        .await?;
    let (agent, events) = builder.build()?;
    let first_turn = agent
        .prompt(PromptRequest::new("run the counter").request_id("turn-1"))
        .await?;
    assert_eq!(first_turn.request_id(), Some("turn-1"));
    let first = first_turn
        .result()
        .await
        .expect_err("the injected tool completion replacement must fail");
    assert!(first.to_string().contains("injected replacement failure"));
    agent.shutdown().await?;
    drop((agent, events));

    let state = crate::DurableSession::open(store.clone(), "ambiguous-tool").await?;
    let builder = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(tools()?)
        .durability(state)
        .await?;
    let (resumed, resumed_events) = builder.build()?;
    let recovered_turn = resumed
        .prompt(PromptRequest::new("run the counter").request_id("turn-1"))
        .await?;
    assert_eq!(recovered_turn.request_id(), Some("turn-1"));
    let recovered = recovered_turn.result().await?;
    assert_eq!(
        recovered.final_message(),
        "recovered after retrying the tool"
    );
    assert_eq!(tool_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(generations.load(std::sync::atomic::Ordering::SeqCst), 2);
    resumed.shutdown().await?;
    drop((resumed, resumed_events));

    let state = crate::DurableSession::open(store, "ambiguous-tool").await?;
    let builder = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(tools()?)
        .durability(state)
        .await?;
    let (reopened, reopened_events) = builder.build()?;
    let replayed = reopened
        .prompt(PromptRequest::new("run the counter").request_id("turn-1"))
        .await?
        .result()
        .await?;
    assert_eq!(
        replayed.final_message(),
        "recovered after retrying the tool"
    );
    assert_eq!(generations.load(std::sync::atomic::Ordering::SeqCst), 2);
    let next = reopened
        .prompt(PromptRequest::new("continue").request_id("turn-2"))
        .await?
        .result()
        .await?;
    assert_eq!(next.final_message(), "recovered after retrying the tool");
    assert_eq!(tool_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(generations.load(std::sync::atomic::Ordering::SeqCst), 3);
    reopened.shutdown().await?;
    drop((reopened, reopened_events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn completed_tool_output_replays_after_tool_is_removed() -> Result<()> {
    let store = crate::MemoryStore::new()?;
    let failing_store = FailReplaceOnce {
        inner: store.clone(),
        expected_revision: 7,
        failed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tool_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = || {
        let generations = Arc::clone(&generations);
        OpenAi::builder("test-key")
            .service(move || RemovedToolRecoveryService {
                generations: Arc::clone(&generations),
                requests: Arc::default(),
            })
            .build()
    };
    let workspace = temporary_workspace("durability-removed-tool-recovery")?;
    let first_tools = Tools::builder()
        .without_defaults()
        .tool_with_exposure(
            RecordedHiddenTool {
                calls: Arc::clone(&tool_calls),
            },
            nanocodex_agent::tools::ToolExposure::Hidden,
        )
        .build()?;
    let state = crate::DurableSession::open(failing_store, "removed-tool-recovery").await?;
    let builder = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(first_tools)
        .durability(state)
        .await?;
    let (agent, events) = builder.build()?;

    let first = agent
        .prompt(PromptRequest::new("call the recorded tool once").request_id("turn-1"))
        .await?
        .result()
        .await
        .expect_err("the injected crash boundary must stop before the wait model call");
    assert!(first.to_string().contains("injected replacement failure"));
    assert_eq!(
        tool_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the first runtime must execute the tool exactly once"
    );
    assert_eq!(
        generations.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the crash must happen after tool completion and before the next model call"
    );
    agent.shutdown().await?;
    drop((agent, events));

    let state = crate::DurableSession::open(store, "removed-tool-recovery").await?;
    let recovered_tools = Tools::builder().without_defaults().build()?;
    let builder = Nanocodex::builder(openai()?)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(recovered_tools)
        .durability(state)
        .await?;
    let (recovered, recovered_events) = builder.build()?;
    let replayed = recovered
        .prompt(PromptRequest::new("call the recorded tool once").request_id("turn-1"))
        .await?
        .result()
        .await?;
    assert_eq!(
        replayed.final_message(),
        "recovered with the recorded tool output"
    );
    assert_eq!(
        tool_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "recovery must not rerun the missing tool handler"
    );
    assert_eq!(
        generations.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "recovery must continue from the exact recorded tool output"
    );

    recovered.shutdown().await?;
    drop((recovered, recovered_events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn model_recovery_uses_recorded_requests_across_runtime_changes() -> Result<()> {
    for (warmup, pending) in [(false, false), (false, true), (true, false), (true, true)] {
        let expected_revision = 5 + u64::from(warmup) * 2 + u64::from(pending);
        let store = crate::MemoryStore::new()?;
        let failing_store = FailReplaceOnce {
            inner: store.clone(),
            expected_revision,
            failed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tool_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let openai = || {
            let requests = Arc::clone(&requests);
            let generations = Arc::clone(&generations);
            OpenAi::builder("test-key")
                .websocket_warmup(warmup)
                .service(move || RemovedToolRecoveryService {
                    generations: Arc::clone(&generations),
                    requests: Arc::clone(&requests),
                })
                .build()
        };
        let workspace = temporary_workspace("durability-changed-tool-profile")?;
        let first_tools = Tools::builder()
            .without_defaults()
            .tool(RecordedHiddenTool {
                calls: Arc::clone(&tool_calls),
            })
            .build()?;
        let state = crate::DurableSession::open(failing_store, "changed-tool-profile").await?;
        let builder = Nanocodex::builder(openai()?)
            .workspace(&workspace)
            .session_id(test_session_id())
            .tools(first_tools)
            .instructions("original system instructions")
            .execution_environment(nanocodex_agent::ExecutionEnvironment::new(
                "2026-09-03",
                "Etc/UTC",
            ))
            .durability(state.clone())
            .await?;
        let (agent, events) = builder.build()?;

        let first = agent
            .prompt(PromptRequest::new("call the recorded tool once").request_id("turn-1"))
            .await?
            .result()
            .await
            .expect_err("the injected crash boundary must stop before the next model call");
        assert!(first.to_string().contains("injected replacement failure"));
        let retained = state.state().await?;
        let recorded_input = retained
            .operation("turn-1")
            .unwrap()
            .steps
            .get("model-2")
            .map(|step| step.input.decode::<serde_json::Value>())
            .transpose()?;
        agent.shutdown().await?;
        drop((agent, events));

        let state = crate::DurableSession::open(store, "changed-tool-profile").await?;
        let recovered_tools = Tools::builder().without_defaults().build()?;
        let builder = Nanocodex::builder(openai()?)
            .workspace(&workspace)
            .session_id(test_session_id())
            .tools(recovered_tools)
            .instructions("updated system instructions")
            .execution_environment(nanocodex_agent::ExecutionEnvironment::new(
                "2026-09-04",
                "Europe/Athens",
            ))
            .durability(state)
            .await?;
        let (recovered, recovered_events) = builder.build()?;
        let result = recovered
            .prompt(PromptRequest::new("call the recorded tool once").request_id("turn-1"))
            .await?
            .result()
            .await?;
        assert_eq!(
            result.final_message(),
            "recovered with the recorded tool output"
        );
        assert_eq!(
            tool_calls.load(Ordering::SeqCst),
            1,
            "completed tools must not rerun"
        );
        assert_eq!(
            generations.load(Ordering::SeqCst),
            if pending { 3 } else { 2 }
        );
        if let Some(input) = recorded_input {
            let mut expected = input["request_prefix"].as_array().unwrap().clone();
            expected.extend(input["prompt_history"].as_array().unwrap().iter().cloned());
            assert_eq!(
                requests.lock().unwrap().last().unwrap(),
                &json!(expected),
                "an unfinished model call must resend its original instructions, tools, and history"
            );
        }
        recovered
            .prompt(PromptRequest::new("continue").request_id("turn-2"))
            .await?
            .result()
            .await?;
        let next = requests.lock().unwrap().last().unwrap().to_string();
        let checkpoint = serde_json::to_value(result.snapshot().unwrap())?;
        assert!(
            checkpoint["request_prefix"]
                .to_string()
                .contains("updated system instructions")
        );
        assert!(next.contains("2026-09-04"));
        assert!(next.contains("Europe/Athens"));

        recovered.shutdown().await?;
        drop((recovered, recovered_events));
        std::fs::remove_dir_all(workspace)?;
    }
    Ok(())
}

#[tokio::test]
async fn durability_attached_agent_gives_each_clean_descendant_its_own_durable_state() -> Result<()>
{
    let store = crate::MemoryStore::new()?;
    let acquisitions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let state = crate::DurableSession::open(
        CountingAcquires {
            inner: store.clone(),
            acquisitions: Arc::clone(&acquisitions),
        },
        "spawn-policy",
    )
    .await?;
    let service_factories = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = OpenAi::builder("test-key")
        .service({
            let service_factories = Arc::clone(&service_factories);
            let generations = Arc::clone(&generations);
            move || {
                service_factories.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                DurableReplayService {
                    generations: Arc::clone(&generations),
                }
            }
        })
        .build()?;
    let workspace = temporary_workspace("durability-spawn-policy")?;
    let builder = Nanocodex::builder(openai)
        .workspace(&workspace)
        .session_id(test_session_id())
        .durability(state)
        .await?;
    let (agent, events) = builder.build()?;

    let (child, child_events) = agent.spawn().await?;
    let child_id = child.session_id().parse::<SessionId>()?;
    assert_eq!(
        *acquisitions
            .lock()
            .expect("acquisition recorder lock is not poisoned"),
        ["spawn-policy", "spawn-policy"],
        "root spawn must not acquire the new child's state"
    );
    let (grandchild, grandchild_events) = child.spawn().await?;
    let grandchild_id = grandchild.session_id().parse::<SessionId>()?;
    assert_eq!(
        service_factories.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "each clean descendant must receive its own service and driver"
    );
    assert_eq!(
        *acquisitions
            .lock()
            .expect("acquisition recorder lock is not poisoned"),
        ["spawn-policy".to_owned(), "spawn-policy".to_owned(),],
        "clean spawn must not acquire either the parent or new child state"
    );

    let child_result = child
        .prompt(PromptRequest::new("child work").request_id("child-turn"))
        .await?
        .result()
        .await?;
    let grandchild_result = grandchild
        .prompt(PromptRequest::new("grandchild work").request_id("grandchild-turn"))
        .await?
        .result()
        .await?;
    assert_eq!(child_result.final_message(), "durably replayed");
    assert_eq!(grandchild_result.final_message(), "durably replayed");
    assert_eq!(generations.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        *acquisitions
            .lock()
            .expect("acquisition recorder lock is not poisoned"),
        [
            "spawn-policy".to_owned(),
            "spawn-policy".to_owned(),
            child_id.to_string(),
            child_id.to_string(),
            grandchild_id.to_string(),
            grandchild_id.to_string(),
        ],
        "each descendant must open and attach exactly its own state ID"
    );

    grandchild.shutdown().await?;
    drop((grandchild, grandchild_events));
    child.shutdown().await?;
    drop((child, child_events));
    agent.shutdown().await?;
    drop((agent, events));

    for (session_id, request_id, input) in [
        (child_id, "child-turn", "child work"),
        (grandchild_id, "grandchild-turn", "grandchild work"),
    ] {
        let reopened_state =
            crate::DurableSession::open(store.clone(), session_id.to_string()).await?;
        let reopened_builder = Nanocodex::builder(
            OpenAi::builder("test-key")
                .service({
                    let generations = Arc::clone(&generations);
                    move || DurableReplayService {
                        generations: Arc::clone(&generations),
                    }
                })
                .build()?,
        )
        .workspace(&workspace)
        .session_id(session_id)
        .durability(reopened_state)
        .await?;
        let (reopened, reopened_events) = reopened_builder.build()?;
        let replayed = reopened
            .prompt(PromptRequest::new(input).request_id(request_id))
            .await?
            .result()
            .await?;
        assert_eq!(replayed.final_message(), "durably replayed");
        reopened.shutdown().await?;
        drop((reopened, reopened_events));
    }
    assert_eq!(
        generations.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "reopening either descendant must replay without another model call"
    );

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn blocked_child_state_acquisition_does_not_block_the_parent_driver() -> Result<()> {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let store = GateFirstChildAcquire {
        inner: crate::MemoryStore::new()?,
        root_state_id: "nonblocking-spawn-policy",
        gated: Arc::new(AtomicBool::new(false)),
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    };
    let state = crate::DurableSession::open(store, "nonblocking-spawn-policy").await?;
    let generations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let openai = OpenAi::builder("test-key")
        .service({
            let generations = Arc::clone(&generations);
            move || DurableReplayService {
                generations: Arc::clone(&generations),
            }
        })
        .build()?;
    let workspace = temporary_workspace("durability-nonblocking-spawn")?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(&workspace)
        .durability(state)
        .await?
        .build()?;
    let (child, child_events) = agent.spawn().await?;
    let child_prompt = child.clone();
    let child_result = tokio::spawn(async move {
        child_prompt
            .prompt(PromptRequest::new("blocked child work").request_id("blocked-child-turn"))
            .await?
            .result()
            .await
    });

    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .map_err(|_| eyre!("the child never reached its gated durability acquisition"))?;
    let (sibling, sibling_events) = tokio::time::timeout(Duration::from_secs(1), agent.spawn())
        .await
        .map_err(|_| eyre!("the child store blocked the parent driver"))??;

    release.notify_one();
    assert_eq!(child_result.await??.final_message(), "durably replayed");
    sibling.shutdown().await?;
    drop((sibling, sibling_events));
    child.shutdown().await?;
    drop((child, child_events));
    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}
