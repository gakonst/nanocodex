use std::{
    future::{Ready, ready},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
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
                id: "resp-compaction".to_owned(),
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
    admissions: Mutex<Vec<String>>,
}

impl ProviderSteps {
    const fn new() -> Self {
        Self {
            admissions: Mutex::new(Vec::new()),
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
            self.admissions.lock().unwrap().push(kind);
            Ok(ExecutionStepAdmission::Execute)
        })
    }

    fn complete_step<'a>(
        &'a self,
        _operation_id: String,
        _step_id: String,
        _output_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
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
        .experimental_context(false)
        .transport(ResponsesTransport::Https)
        .service(move || ProviderProbe {
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
