use std::{
    future::{Ready, ready},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    task::{Context, Poll},
};

use nanocodex_agent::{
    PromptRequest,
    execution::{
        ExecutionAdmission, ExecutionFuture, ExecutionOutput, ExecutionPolicy,
        ExecutionStepAdmission,
    },
    session::SessionSnapshot,
};
use nanocodex_oai_api::{
    responses::{ContentItem, MessageRole, ResponseItem, ResponseItemId, WarmupResponse},
    tower::{
        CodeCall, CodeCallKind, CompactionOutput, GenerationOutput, ResponsePipelineStats,
        ResponsesAttempt, ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse,
    },
};
use nanocodex_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult, contract::async_trait,
};
use tower::Service;

use super::*;

#[derive(Clone)]
struct ProviderProbe {
    compaction_response_id: &'static str,
    calls: Arc<AtomicU32>,
}

impl Service<ResponsesAttempt> for ProviderProbe {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Ready<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let output = match request.kind() {
            ResponsesAttemptKind::Warmup => ResponsesOutput::Warmup(WarmupResponse {
                id: "resp-warmup".to_owned(),
                usage: None,
            }),
            ResponsesAttemptKind::Generation => ResponsesOutput::Generation(GenerationOutput {
                id: "resp-generation".to_owned(),
                status: "completed".to_owned(),
                end_turn: Some(true),
                final_message: Some("provider was called".to_owned()),
                output_items: vec![ResponseItem::message(
                    MessageRole::Assistant,
                    [ContentItem::output_text("provider was called")],
                )],
                code_calls: Vec::new(),
                usage: None,
                time_to_first_event_ns: 0,
                time_to_first_output_ns: None,
                pipeline_stats: ResponsePipelineStats::default(),
            }),
            ResponsesAttemptKind::Compaction => ResponsesOutput::Compaction(CompactionOutput {
                id: self.compaction_response_id.to_owned(),
                status: "completed".to_owned(),
                item: ResponseItem::Compaction {
                    id: Some(ResponseItemId::from("cmp-provider")),
                    encrypted_content: "opaque-summary".into(),
                    created_by: None,
                    internal_chat_message_metadata_passthrough: None,
                },
                usage: None,
                time_to_first_event_ns: 0,
                time_to_first_output_ns: None,
                pipeline_stats: ResponsePipelineStats::default(),
            }),
            _ => panic!("provider recovery probe received an unsupported attempt kind"),
        };
        ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

#[derive(Clone)]
struct HostContextProvider {
    generations: Arc<AtomicU32>,
}

impl Service<ResponsesAttempt> for HostContextProvider {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Ready<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        let output = match request.kind() {
            ResponsesAttemptKind::Warmup => ResponsesOutput::Warmup(WarmupResponse {
                id: "resp-host-context-warmup".to_owned(),
                usage: None,
            }),
            ResponsesAttemptKind::Generation
                if self.generations.fetch_add(1, Ordering::Relaxed) == 0 =>
            {
                host_context_tool_generation()
            }
            ResponsesAttemptKind::Generation => {
                assert!(request.input_items().any(|item| {
                    serde_json::to_value(item).is_ok_and(|item| {
                        item["type"] == "function_call_output"
                            && item["call_id"] == "call-host-context"
                    })
                }));
                ResponsesOutput::Generation(GenerationOutput {
                    id: "resp-host-context-complete".to_owned(),
                    status: "completed".to_owned(),
                    end_turn: Some(true),
                    final_message: Some("private context observed".to_owned()),
                    output_items: vec![ResponseItem::message(
                        MessageRole::Assistant,
                        [ContentItem::output_text("private context observed")],
                    )],
                    code_calls: Vec::new(),
                    usage: None,
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: None,
                    pipeline_stats: ResponsePipelineStats::default(),
                })
            }
            _ => panic!("host-context probe received an unsupported attempt kind"),
        };
        ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

fn host_context_tool_generation() -> ResponsesOutput {
    let item = serde_json::from_value(json!({
        "type": "function_call",
        "call_id": "call-host-context",
        "name": "host_context_probe",
        "arguments": "{}"
    }))
    .expect("function call item decodes");
    ResponsesOutput::Generation(GenerationOutput {
        id: "resp-host-context-tool".to_owned(),
        status: "completed".to_owned(),
        end_turn: Some(false),
        final_message: None,
        output_items: vec![item],
        code_calls: vec![CodeCall {
            call_id: "call-host-context".to_owned(),
            name: "host_context_probe".to_owned(),
            namespace: None,
            input: "{}".to_owned(),
            kind: CodeCallKind::Function,
        }],
        usage: None,
        time_to_first_event_ns: 0,
        time_to_first_output_ns: None,
        pipeline_stats: ResponsePipelineStats::default(),
    })
}

struct HostContextProbe {
    seen: Arc<Mutex<Vec<Option<String>>>>,
}

#[async_trait]
impl Tool for HostContextProbe {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "host_context_probe",
            "Records embedding-owned context without exposing it to the model.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, _input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        self.seen
            .lock()
            .unwrap()
            .push(context.host_context().map(str::to_owned));
        Ok(ToolOutput::text("observed"))
    }
}

struct ProviderSteps {
    compaction_fault: Option<&'static str>,
    admissions: Mutex<Vec<String>>,
    retain_continuation: bool,
    continuation: Mutex<Option<nanocodex_agent::execution::ExecutionContinuation>>,
    fail_next_model: AtomicBool,
}

impl ProviderSteps {
    const fn new() -> Self {
        Self {
            compaction_fault: None,
            admissions: Mutex::new(Vec::new()),
            retain_continuation: false,
            continuation: Mutex::new(None),
            fail_next_model: AtomicBool::new(false),
        }
    }

    fn assert_admitted(&self, kind: &str) {
        assert!(
            self.admissions
                .lock()
                .unwrap()
                .iter()
                .any(|seen| seen == kind)
        );
    }

    fn clear_admissions(&self) {
        self.admissions.lock().unwrap().clear();
    }
}

impl ExecutionPolicy for ProviderSteps {
    fn continuation<'a>(
        &'a self,
        _operation_id: String,
    ) -> ExecutionFuture<
        'a,
        nanocodex_agent::Result<Option<nanocodex_agent::execution::ExecutionContinuation>>,
    > {
        Box::pin(async { Ok(self.continuation.lock().unwrap().take()) })
    }
    fn advance<'a>(
        &'a self,
        _operation_id: String,
        state: nanocodex_agent::execution::ExecutionContinuation,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async move {
            if self.retain_continuation {
                *self.continuation.lock().unwrap() = Some(state);
            }
            Ok(())
        })
    }

    fn admit<'a>(
        &'a self,
        _operation_id: String,
        _input_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<ExecutionAdmission>> {
        Box::pin(async { Ok(ExecutionAdmission::Execute) })
    }

    fn admit_automatic<'a>(
        &'a self,
        candidate_operation_id: String,
        _input_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<(String, ExecutionAdmission)>> {
        Box::pin(async move { Ok((candidate_operation_id, ExecutionAdmission::Execute)) })
    }

    fn release<'a>(&'a self, _operation_id: String) -> ExecutionFuture<'a, ()> {
        Box::pin(async {})
    }

    fn cancel<'a>(
        &'a self,
        _operation_id: String,
        _snapshot: Option<SessionSnapshot>,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn begin_attempt<'a>(
        &'a self,
        _operation_id: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn begin_step<'a>(
        &'a self,
        _operation_id: String,
        _step_id: String,
        kind: String,
        _input_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<ExecutionStepAdmission>> {
        Box::pin(async move {
            self.admissions.lock().unwrap().push(kind.clone());
            if kind == "model_call" && self.fail_next_model.swap(false, Ordering::Relaxed) {
                return Err(NanocodexError::InvalidExecutionPolicy(
                    "interrupted before model call".into(),
                ));
            }
            if kind == "compaction" && self.compaction_fault == Some("cancellation") {
                return std::future::pending().await;
            }
            if kind == "compaction" && self.compaction_fault == Some("admission") {
                return Err(NanocodexError::InvalidExecutionPolicy(
                    "compaction admission unavailable".into(),
                ));
            }
            Ok(ExecutionStepAdmission::Execute)
        })
    }

    fn complete_step<'a>(
        &'a self,
        _operation_id: String,
        step_id: String,
        _output_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async move {
            if step_id.starts_with("compaction-") && self.compaction_fault == Some("completion") {
                return Err(NanocodexError::InvalidExecutionPolicy(
                    "compaction completion unavailable".into(),
                ));
            }
            Ok(())
        })
    }

    fn complete<'a>(
        &'a self,
        _operation_id: String,
        _snapshot: SessionSnapshot,
        _output: ExecutionOutput,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn fail_attempt<'a>(
        &'a self,
        _operation_id: String,
        _error: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn fail<'a>(
        &'a self,
        _operation_id: String,
        _snapshot: SessionSnapshot,
        _error: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

async fn assert_provider_step_executes(
    kind: &'static str,
    transport: ResponsesTransport,
) -> Result<()> {
    let provider_calls = Arc::new(AtomicU32::new(0));
    let service_calls = Arc::clone(&provider_calls);
    let policy = Arc::new(ProviderSteps::new());
    let openai = OpenAi::builder("test-key")
        .transport(transport)
        .service(move || ProviderProbe {
            compaction_response_id: "resp-compaction",
            calls: Arc::clone(&service_calls),
        })
        .build()?;
    let workspace = tempfile::tempdir()?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(workspace.path())
        .execution_policy(policy.clone())
        .build()?;
    drop(events);

    let result = agent
        .prompt("recover the interrupted provider step")
        .await?
        .await?;
    assert_eq!(result.final_message(), "provider was called");
    assert!(provider_calls.load(Ordering::Relaxed) >= 1);
    policy.assert_admitted(kind);
    agent.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn model_call_admission_executes_the_provider() -> Result<()> {
    assert_provider_step_executes("model_call", ResponsesTransport::Https).await
}

#[tokio::test]
async fn admitted_operation_id_reaches_tools_only_as_private_context() -> Result<()> {
    const HOST_CONTEXT: &str = "opaque-managed-turn";

    let seen = Arc::new(Mutex::new(Vec::new()));
    let tools = Tools::builder()
        .without_defaults()
        .tool(HostContextProbe {
            seen: Arc::clone(&seen),
        })
        .build()?;
    let openai = OpenAi::builder("test-key")
        .transport(ResponsesTransport::Https)
        .service(|| HostContextProvider {
            generations: Arc::new(AtomicU32::new(0)),
        })
        .build()?;
    let workspace = tempfile::tempdir()?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(workspace.path())
        .execution_policy(Arc::new(ProviderSteps::new()))
        .tools(tools)
        .build()?;
    drop(events);

    let result = agent
        .prompt(PromptRequest::new("inspect private context").request_id(HOST_CONTEXT))
        .await?
        .await?;
    assert_eq!(result.final_message(), "private context observed");
    assert_eq!(&*seen.lock().unwrap(), &[Some(HOST_CONTEXT.to_owned())]);
    agent.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn warmup_admission_executes_the_provider() -> Result<()> {
    assert_provider_step_executes("warmup", ResponsesTransport::WebSocket).await
}

#[tokio::test]
async fn compaction_admission_executes_the_provider() -> Result<()> {
    let provider_calls = Arc::new(AtomicU32::new(0));
    let service_calls = Arc::clone(&provider_calls);
    let policy = Arc::new(ProviderSteps::new());
    let openai = OpenAi::builder("test-key")
        .transport(ResponsesTransport::Https)
        .service(move || ProviderProbe {
            compaction_response_id: "resp-compaction",
            calls: Arc::clone(&service_calls),
        })
        .build()?;
    let workspace = tempfile::tempdir()?;
    let (agent, events) = Nanocodex::builder(openai)
        .workspace(workspace.path())
        .context_window_tokens(1)
        .execution_policy(policy.clone())
        .build()?;
    drop(events);

    assert_eq!(
        agent
            .prompt("establish context that requires compaction")
            .await?
            .await?
            .final_message(),
        "provider was called"
    );
    assert_eq!(provider_calls.swap(0, Ordering::Relaxed), 1);
    policy.clear_admissions();

    let result = agent
        .prompt("recover the pending compaction")
        .await?
        .await?;
    assert_eq!(result.final_message(), "provider was called");
    assert!(provider_calls.load(Ordering::Relaxed) >= 1);
    policy.assert_admitted("compaction");
    agent.shutdown().await?;
    Ok(())
}

async fn assert_compaction_failure_terminal(fault: &'static str) -> Result<()> {
    let provider_calls = Arc::new(AtomicU32::new(0));
    let service_calls = Arc::clone(&provider_calls);
    let policy = Arc::new(ProviderSteps {
        compaction_fault: Some(fault),
        ..ProviderSteps::new()
    });
    let openai = OpenAi::builder("test-key")
        .transport(ResponsesTransport::Https)
        .service(move || ProviderProbe {
            compaction_response_id: if fault == "response_id" {
                ""
            } else {
                "resp-compaction"
            },
            calls: Arc::clone(&service_calls),
        })
        .build()?;
    let workspace = tempfile::tempdir()?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .workspace(workspace.path())
        .context_window_tokens(1)
        .execution_policy(policy.clone())
        .build()?;
    agent.prompt("establish context").await?.await?;
    while events.try_recv_timed().is_some() {}
    provider_calls.store(0, Ordering::Relaxed);
    let turn = agent.prompt("compact context").await?;
    if fault == "cancellation" {
        timeout(std::time::Duration::from_secs(5), async {
            loop {
                if policy
                    .admissions
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|kind| kind == "compaction")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await?;
        turn.control().cancel().await?;
    }
    let error = turn.await.expect_err("compaction must fail");
    match fault {
        "cancellation" => assert!(matches!(error, NanocodexError::TurnCancelled)),
        "response_id" => assert!(matches!(error, NanocodexError::MalformedResponse { .. })),
        _ => assert!(
            error
                .to_string()
                .contains(&format!("compaction {fault} unavailable"))
        ),
    }
    policy.assert_admitted("compaction");
    assert_eq!(
        provider_calls.load(Ordering::Relaxed),
        u32::from(matches!(fault, "completion" | "response_id"))
    );
    let kinds: Vec<_> = std::iter::from_fn(|| events.try_recv_timed())
        .map(|event| event.event.kind)
        .collect();
    let compaction: Vec<_> = kinds
        .iter()
        .copied()
        .filter(|kind| {
            matches!(
                kind,
                AgentEventKind::ModelCompactionStarted
                    | AgentEventKind::ModelCompactionCompleted
                    | AgentEventKind::ModelCompactionFailed
            )
        })
        .collect();
    assert_eq!(
        compaction,
        vec![
            AgentEventKind::ModelCompactionStarted,
            AgentEventKind::ModelCompactionFailed
        ]
    );
    agent.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn compaction_admission_failure_emits_terminal_event() -> Result<()> {
    assert_compaction_failure_terminal("admission").await
}

#[tokio::test]
async fn compaction_completion_failure_emits_terminal_event() -> Result<()> {
    assert_compaction_failure_terminal("completion").await
}

#[tokio::test]
async fn compaction_cancellation_emits_terminal_event() -> Result<()> {
    assert_compaction_failure_terminal("cancellation").await
}

#[tokio::test]
async fn compaction_invalid_response_id_emits_terminal_event() -> Result<()> {
    assert_compaction_failure_terminal("response_id").await
}

#[derive(Clone)]
struct RevisionRecoveryProvider {
    generations: Arc<AtomicU32>,
    compactions: Arc<AtomicU32>,
}

impl Service<ResponsesAttempt> for RevisionRecoveryProvider {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Ready<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        if matches!(request.kind(), ResponsesAttemptKind::Compaction) {
            self.compactions.fetch_add(1, Ordering::Relaxed);
        }
        if matches!(request.kind(), ResponsesAttemptKind::Generation) {
            let index = self.generations.fetch_add(1, Ordering::Relaxed);
            if index < 2 {
                let ResponsesOutput::Generation(mut generation) = host_context_tool_generation()
                else {
                    unreachable!()
                };
                generation.id = format!("revision-response-{index}");
                generation.code_calls[0].call_id = format!("revision-call-{index}");
                generation.output_items = vec![
                    serde_json::from_value(json!({
                        "type": "function_call", "call_id": format!("revision-call-{index}"),
                        "name": "host_context_probe", "arguments": "{}"
                    }))
                    .unwrap(),
                ];
                return ready(Ok(ResponsesServiceResponse::new(
                    ResponsesOutput::Generation(generation),
                )));
            }
        }
        ProviderProbe {
            compaction_response_id: "revision-compaction",
            calls: Arc::new(AtomicU32::new(0)),
        }
        .call(request)
    }
}

struct RecoveredRevisionProbe {
    observations: Arc<Mutex<Vec<(Option<u64>, u32)>>>,
    compactions: Arc<AtomicU32>,
}

#[async_trait]
impl Tool for RecoveredRevisionProbe {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "host_context_probe",
            "Record restored instruction revision.",
            json!({
                "type": "object", "properties": {}, "additionalProperties": false
            }),
        )
    }
    async fn execute(&self, _input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        self.observations.lock().unwrap().push((
            context.instruction_revision(),
            self.compactions.load(Ordering::Relaxed),
        ));
        Ok(ToolOutput::text("observed"))
    }
}

#[tokio::test]
async fn instruction_revision_survives_durable_recovery_and_mid_turn_compaction() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let policy = Arc::new(ProviderSteps {
        retain_continuation: true,
        fail_next_model: AtomicBool::new(true),
        ..ProviderSteps::new()
    });
    let revisions = Arc::new(Mutex::new(Vec::new()));
    let compactions = Arc::new(AtomicU32::new(0));
    for revision in [7, 99] {
        let service_compactions = compactions.clone();
        let openai = OpenAi::builder("test-key")
            .transport(ResponsesTransport::Https)
            .service(move || RevisionRecoveryProvider {
                generations: Arc::new(AtomicU32::new(0)),
                compactions: service_compactions.clone(),
            })
            .build()?;
        let tools = Tools::builder()
            .without_defaults()
            .tool(RecoveredRevisionProbe {
                observations: revisions.clone(),
                compactions: compactions.clone(),
            })
            .build()?;
        let (agent, events) = Nanocodex::builder(openai)
            .workspace(workspace.path())
            .context_window_tokens(1)
            .execution_policy(policy.clone())
            .tools(tools)
            .build()?;
        drop(events);
        let result = agent
            .prompt(
                PromptRequest::new(
                    Prompt::new("recover the original operation")
                        .with_instruction_revision(revision),
                )
                .request_id("revision-recovery-operation"),
            )
            .await?
            .await;
        if revision == 7 {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("interrupted before model call")
            );
            let retained = policy.continuation.lock().unwrap();
            let saved: Value = serde_json::from_str(&retained.as_ref().unwrap().state_json)?;
            assert_eq!(saved["instruction_revision"], 7);
            assert_eq!(saved["phase"], "Generate");
        } else {
            result?;
        }
        agent.shutdown().await?;
    }
    let observations = revisions.lock().unwrap();
    assert_eq!(
        observations.iter().map(|entry| entry.0).collect::<Vec<_>>(),
        vec![Some(7), Some(7)]
    );
    assert!(
        observations[1].1 > observations[0].1,
        "second tool must execute after compaction"
    );
    assert!(compactions.load(Ordering::Relaxed) > 0);
    Ok(())
}
