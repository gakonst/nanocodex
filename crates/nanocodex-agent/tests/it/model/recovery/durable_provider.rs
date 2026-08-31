use std::{
    future::{Ready, ready},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    task::{Context, Poll},
};

use nanocodex_agent::{
    NanocodexError,
    execution::{
        ExecutionAdmission, ExecutionFuture, ExecutionOutput, ExecutionPolicy, ExecutionRetry,
        ExecutionStepAdmission,
    },
    session::SessionSnapshot,
};
use nanocodex_oai_api::{
    responses::{ContentItem, MessageRole, ResponseItem, ResponseItemId, WarmupResponse},
    tower::{
        CompactionOutput, GenerationOutput, ResponsePipelineStats, ResponsesAttempt,
        ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse,
    },
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
                usage_metadata: None,
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
                usage_metadata: None,
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
                usage_metadata: None,
                time_to_first_event_ns: 0,
                time_to_first_output_ns: None,
                pipeline_stats: ResponsePipelineStats::default(),
            }),
            _ => panic!("provider recovery probe received an unsupported attempt kind"),
        };
        ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

struct PendingProviderStep {
    pending_kind: &'static str,
    admissions: Mutex<Vec<(String, ExecutionRetry)>>,
}

impl PendingProviderStep {
    const fn new(pending_kind: &'static str) -> Self {
        Self {
            pending_kind,
            admissions: Mutex::new(Vec::new()),
        }
    }

    fn assert_recovered_at_most_once(&self) {
        assert_eq!(
            self.admissions.lock().unwrap().as_slice(),
            [(self.pending_kind.to_owned(), ExecutionRetry::Never)]
        );
    }

    fn clear_admissions(&self) {
        self.admissions.lock().unwrap().clear();
    }
}

impl ExecutionPolicy for PendingProviderStep {
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
        retry: ExecutionRetry,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<ExecutionStepAdmission>> {
        Box::pin(async move {
            self.admissions.lock().unwrap().push((kind.clone(), retry));
            if kind == self.pending_kind && retry == ExecutionRetry::Never {
                Ok(ExecutionStepAdmission::Unknown)
            } else {
                Ok(ExecutionStepAdmission::Execute)
            }
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

#[tokio::test]
async fn execution_policy_cancellation_reconciliation_defaults_fail_closed() {
    let policy = PendingProviderStep::new("tool_call");
    let outcome = policy
        .reconcile_cancelled_step(
            "turn-1".to_owned(),
            "tool-1".to_owned(),
            r#"{"status":"unknown"}"#.to_owned(),
        )
        .await;

    assert!(matches!(
        outcome,
        Err(NanocodexError::ExecutionPolicyCapabilityUnsupported {
            capability: "reconcile_cancelled_step"
        })
    ));
}

async fn assert_pending_provider_step_is_not_repeated(
    kind: &'static str,
    transport: ResponsesTransport,
    expected_operation: &'static str,
) -> Result<()> {
    let provider_calls = Arc::new(AtomicU32::new(0));
    let service_calls = Arc::clone(&provider_calls);
    let policy = Arc::new(PendingProviderStep::new(kind));
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

    let error = agent
        .prompt("recover the interrupted provider step")
        .await?
        .await
        .expect_err("an EffectPending provider step must fail closed");
    assert!(matches!(
        error,
        NanocodexError::ProviderOutcomeUnknown { operation }
            if operation == expected_operation
    ));
    assert_eq!(
        provider_calls.load(Ordering::Relaxed),
        0,
        "durable recovery must not submit the provider request again"
    );
    policy.assert_recovered_at_most_once();
    agent.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn pending_model_call_recovery_does_not_call_the_provider_again() -> Result<()> {
    assert_pending_provider_step_is_not_repeated(
        "model_call",
        ResponsesTransport::Https,
        "model call",
    )
    .await
}

#[tokio::test]
async fn pending_warmup_recovery_does_not_call_the_provider_again() -> Result<()> {
    assert_pending_provider_step_is_not_repeated("warmup", ResponsesTransport::WebSocket, "warmup")
        .await
}

#[tokio::test]
async fn pending_compaction_recovery_does_not_call_the_provider_again() -> Result<()> {
    let provider_calls = Arc::new(AtomicU32::new(0));
    let service_calls = Arc::clone(&provider_calls);
    let policy = Arc::new(PendingProviderStep::new("compaction"));
    let openai = OpenAi::builder("test-key")
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

    let error = agent
        .prompt("recover the pending compaction")
        .await?
        .await
        .expect_err("an EffectPending compaction must fail closed");
    assert!(matches!(
        error,
        NanocodexError::ProviderOutcomeUnknown {
            operation: "compaction"
        }
    ));
    assert_eq!(
        provider_calls.load(Ordering::Relaxed),
        0,
        "compaction recovery must not submit the provider request again"
    );
    policy.assert_recovered_at_most_once();
    agent.shutdown().await?;
    Ok(())
}
