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
    responses::{ContentItem, MessageRole, ResponseItem, WarmupResponse},
    tower::{
        CodeCall, CodeCallKind, GenerationOutput, ResponsePipelineStats, ResponsesAttempt,
        ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse,
    },
};
use nanocodex_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult, contract::async_trait,
};
use tower::Service;

use super::*;

#[derive(Clone)]
struct RevisionProvider {
    generations: Arc<AtomicU32>,
}

impl Service<ResponsesAttempt> for RevisionProvider {
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
                id: "resp-revision-warmup".to_owned(),
                usage: None,
            }),
            ResponsesAttemptKind::Generation
                if self.generations.fetch_add(1, Ordering::Relaxed) < 2 =>
            {
                revision_tool_generation(self.generations.load(Ordering::Relaxed))
            }
            ResponsesAttemptKind::Generation => {
                assert!(request.input_items().any(|item| {
                    serde_json::to_value(item).is_ok_and(|item| {
                        item["type"] == "function_call_output"
                            && item["call_id"] == "call-revision-2"
                    })
                }));
                ResponsesOutput::Generation(GenerationOutput {
                    id: "resp-revision-complete".to_owned(),
                    status: "completed".to_owned(),
                    end_turn: Some(true),
                    final_message: Some("revision observed".to_owned()),
                    output_items: vec![ResponseItem::message(
                        MessageRole::Assistant,
                        [ContentItem::output_text("revision observed")],
                    )],
                    code_calls: Vec::new(),
                    usage: None,
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: None,
                    pipeline_stats: ResponsePipelineStats::default(),
                })
            }
            _ => panic!("revision probe received an unsupported attempt kind"),
        };
        ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

fn revision_tool_generation(index: u32) -> ResponsesOutput {
    let item = serde_json::from_value(json!({
        "type": "function_call",
        "call_id": format!("call-revision-{index}"),
        "name": "revision_probe",
        "arguments": "{}"
    }))
    .expect("function call item decodes");
    ResponsesOutput::Generation(GenerationOutput {
        id: "resp-revision-tool".to_owned(),
        status: "completed".to_owned(),
        end_turn: Some(false),
        final_message: None,
        output_items: vec![item],
        code_calls: vec![CodeCall {
            call_id: format!("call-revision-{index}"),
            name: "revision_probe".to_owned(),
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

struct RevisionProbe {
    seen: tokio::sync::mpsc::UnboundedSender<Option<u64>>,
    release: Arc<tokio::sync::Semaphore>,
}

#[async_trait]
impl Tool for RevisionProbe {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "revision_probe",
            "Records the originating instruction revision.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, _input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        self.seen.send(context.instruction_revision()).unwrap();
        self.release.acquire().await.unwrap().forget();
        Ok(ToolOutput::text("observed"))
    }
}

struct ProviderSteps {
    saved: Mutex<Option<nanocodex_agent::execution::ExecutionContinuation>>,
    interrupt: AtomicBool,
}

impl ProviderSteps {
    const fn new() -> Self {
        Self {
            saved: Mutex::new(None),
            interrupt: AtomicBool::new(true),
        }
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
        Box::pin(async { Ok(self.saved.lock().unwrap().take()) })
    }
    fn advance<'a>(
        &'a self,
        _operation_id: String,
        state: nanocodex_agent::execution::ExecutionContinuation,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async move {
            *self.saved.lock().unwrap() = Some(state);
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
            let revision = self.saved.lock().unwrap().as_ref().and_then(|saved| {
                serde_json::from_str::<Value>(&saved.state_json).unwrap()["instruction_revision"]
                    .as_u64()
            });
            if kind == "model_call"
                && revision == Some(2)
                && self.interrupt.swap(false, Ordering::SeqCst)
            {
                return Err(NanocodexError::ExecutionPolicyOwnerStopped);
            }
            Ok(ExecutionStepAdmission::Execute)
        })
    }

    fn complete_step<'a>(
        &'a self,
        _operation_id: String,
        _step_id: String,
        _output_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async move { Ok(()) })
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
    fn accept_steer<'a>(
        &'a self,
        _operation_id: String,
        _accepted_after_model_call_index: u32,
        _input_json: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<u32>> {
        Box::pin(async { Ok(1) })
    }
    fn bind_steer<'a>(
        &'a self,
        _operation_id: String,
        _steer_index: u32,
        _model_call_index: u32,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn consumed_instruction_revision_survives_execution_recovery() -> Result<()> {
    timeout(std::time::Duration::from_secs(15), async {
        let policy = Arc::new(ProviderSteps::new());
        let generations = Arc::new(AtomicU32::new(0));
        let (seen, mut observed) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let workspace = tempfile::tempdir()?;
        let build = || -> Result<_> {
            let generations = generations.clone();
            let openai = OpenAi::builder("test-key")
                .transport(ResponsesTransport::Https)
                .service(move || RevisionProvider {
                    generations: generations.clone(),
                })
                .build()?;
            Nanocodex::builder(openai)
                .workspace(workspace.path())
                .execution_policy(policy.clone())
                .tools(
                    Tools::builder()
                        .without_defaults()
                        .tool(RevisionProbe {
                            seen: seen.clone(),
                            release: release.clone(),
                        })
                        .build()?,
                )
                .build()
                .map_err(Into::into)
        };
        let original = || {
            PromptRequest::new(Prompt::new("original revision").with_instruction_revision(1))
                .request_id("revision-recovery")
        };
        let (agent, events) = build()?;
        drop(events);
        let turn = agent.prompt(original()).await?;
        assert_eq!(observed.recv().await, Some(Some(1)));
        turn.steer(Prompt::new("consumed steering").with_instruction_revision(2))
            .await?;
        release.add_permits(1);
        assert!(matches!(
            turn.await,
            Err(NanocodexError::ExecutionPolicyOwnerStopped)
        ));
        {
            let saved = policy.saved.lock().unwrap();
            let saved = saved.as_ref().unwrap();
            let state: Value = serde_json::from_str(&saved.state_json)?;
            assert_eq!(state["instruction_revision"], 2);
            assert!(serde_json::to_string(&saved.history)?.contains("consumed steering"));
        }
        assert_eq!(generations.load(Ordering::SeqCst), 1);
        drop(agent);
        let (recovered, events) = build()?;
        drop(events);
        let resumed = recovered.prompt(original()).await?;
        assert_eq!(
            observed.recv().await,
            Some(Some(2)),
            "saved consumed revision must override replayed prompt revision 1"
        );
        release.add_permits(1);
        resumed.await?;
        recovered.shutdown().await?;
        Ok::<_, eyre::Report>(())
    })
    .await?
}
