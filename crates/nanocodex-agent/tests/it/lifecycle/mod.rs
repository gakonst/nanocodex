use std::{
    future::{Future, Pending, Ready, pending, ready},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use async_trait::async_trait;
use nanocodex_agent::{
    Nanocodex, NanocodexError, OpenAi, ResponseError, Tools,
    rollout::RolloutConfig,
    session::SessionId,
    transport::{ResponsesAttempt, ResponsesServiceResponse},
};
use nanocodex_oai_api::{
    responses::{ContentItem, MessageRole, ResponseItem, Usage, WarmupResponse},
    tower::{GenerationOutput, ResponsePipelineStats, ResponsesAttemptKind, ResponsesOutput},
};
use nanocodex_tools::{ToolContext, ToolDefinition, ToolOutput, runtime::DynamicToolProvider};
use serde_json::Value;
use tempfile::tempdir;
use tokio::sync::mpsc;
use tower::{Service, ServiceBuilder, limit::ConcurrencyLimitLayer, timeout::TimeoutLayer};

#[derive(Clone)]
struct NeverCalled;

impl Service<ResponsesAttempt> for NeverCalled {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Ready<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _request: ResponsesAttempt) -> Self::Future {
        panic!("the service is not called by this test")
    }
}

fn test_openai() -> OpenAi {
    OpenAi::new("test").unwrap()
}

#[derive(Clone)]
struct PendingService;

#[derive(Clone)]
struct DropPendingService {
    started: Arc<AtomicBool>,
    dropped: Arc<AtomicBool>,
}

struct DropPendingFuture {
    dropped: Arc<AtomicBool>,
}

impl Future for DropPendingFuture {
    type Output = std::result::Result<ResponsesServiceResponse, ResponseError>;

    fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Pending
    }
}

impl Drop for DropPendingFuture {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}

impl Service<ResponsesAttempt> for DropPendingService {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = DropPendingFuture;

    fn poll_ready(
        &mut self,
        _context: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _request: ResponsesAttempt) -> Self::Future {
        self.started.store(true, Ordering::Release);
        DropPendingFuture {
            dropped: Arc::clone(&self.dropped),
        }
    }
}

struct StartProbe(Arc<AtomicBool>);

#[async_trait]
impl DynamicToolProvider for StartProbe {
    fn start(&self) {
        self.0.store(true, Ordering::Release);
    }

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

impl Service<ResponsesAttempt> for PendingService {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Pending<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _request: ResponsesAttempt) -> Self::Future {
        pending()
    }
}

#[derive(Clone)]
struct RetainingCompletedService {
    retained: mpsc::UnboundedSender<ResponsesAttempt>,
}

impl Service<ResponsesAttempt> for RetainingCompletedService {
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
                id: "resp-warmup".to_owned(),
                usage: None,
                usage_metadata: None,
            }),
            ResponsesAttemptKind::Generation => {
                self.retained
                    .send(request.clone())
                    .expect("the test retains the generation attempt");
                let call_index = request
                    .model_call_index()
                    .expect("generation attempts have a model call index");
                ResponsesOutput::Generation(GenerationOutput {
                    id: format!("resp-generation-{call_index}"),
                    status: "completed".to_owned(),
                    end_turn: Some(true),
                    final_message: Some("done".to_owned()),
                    output_items: vec![ResponseItem::message(
                        MessageRole::Assistant,
                        [ContentItem::output_text("done")],
                    )],
                    code_calls: Vec::new(),
                    usage: Some(Usage {
                        input_tokens: 1,
                        output_tokens: 1,
                        total_tokens: 2,
                        ..Usage::default()
                    }),
                    usage_metadata: None,
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: Some(0),
                    pipeline_stats: ResponsePipelineStats::default(),
                })
            }
            _ => {
                panic!("the one-turn lifecycle test must not compact")
            }
        };
        ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

mod builder;
mod control;
mod shutdown;
