#![allow(missing_docs)]

// Test-local dispatchers own process-wide callsite registration, so keep these
// subscriber tests isolated from the unrelated parallel integration harness.

use std::{
    collections::HashMap,
    future::{Pending, Ready, pending, ready},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    task::{Context, Poll},
    time::Instant,
};

use nanocodex_agent::{
    Model, Nanocodex, NanocodexError, OpenAi, ResponseError, Tools,
    transport::{ResponsesAttempt, ResponsesAttemptKind, ResponsesServiceResponse},
};
use nanocodex_oai_api::{
    responses::{Usage, WarmupResponse},
    tower::{CodeCall, CodeCallKind, GenerationOutput, ResponsePipelineStats, ResponsesOutput},
};
use nanocodex_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult, contract::async_trait,
};
use serde_json::json;
use tokio::sync::mpsc;
use tower::Service;
use tracing::{
    Event, Id, Instrument, Subscriber,
    field::{Field, Visit},
    info_span,
    span::{Attributes, Record},
};
use tracing_subscriber::{Layer, layer::Context as LayerContext, prelude::*, registry::LookupSpan};

#[derive(Clone)]
struct PendingService;

impl Service<ResponsesAttempt> for PendingService {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Pending<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _request: ResponsesAttempt) -> Self::Future {
        pending()
    }
}

struct PendingSpanTool {
    started: Arc<tokio::sync::Notify>,
}

struct SteeringBarrierTool {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Tool for PendingSpanTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "trace__pending",
            "Remains active until the turn is cancelled.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        self.started.notify_one();
        std::future::pending().await
    }
}

#[async_trait]
impl Tool for SteeringBarrierTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "trace__pending",
            "Waits at a model boundary until the steering trace test releases it.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        self.started.notify_one();
        self.release.notified().await;
        Ok(ToolOutput::text("released"))
    }
}

#[derive(Clone)]
struct PendingToolService {
    calls: Arc<AtomicU32>,
}

#[derive(Clone)]
struct SteeringTraceService {
    calls: Arc<AtomicU32>,
}

#[derive(Clone)]
struct PricingTraceService {
    calls: Arc<AtomicU32>,
}

impl Service<ResponsesAttempt> for PendingToolService {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        let output = match (call, request.kind()) {
            (0, ResponsesAttemptKind::Warmup) => ResponsesOutput::Warmup(WarmupResponse {
                id: "resp-warmup".to_owned(),
                usage: None,
                usage_metadata: None,
            }),
            (1, ResponsesAttemptKind::Generation) => pending_tool_generation(),
            _ => panic!("unexpected attempt {call}: {:?}", request.kind()),
        };
        ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

impl Service<ResponsesAttempt> for SteeringTraceService {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        let output = match (call, request.kind()) {
            (0, ResponsesAttemptKind::Warmup) => ResponsesOutput::Warmup(WarmupResponse {
                id: "resp-warmup".to_owned(),
                usage: None,
                usage_metadata: None,
            }),
            (1, ResponsesAttemptKind::Generation) => pending_tool_generation(),
            (2, ResponsesAttemptKind::Generation) => final_generation(),
            _ => panic!("unexpected attempt {call}: {:?}", request.kind()),
        };
        ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

impl Service<ResponsesAttempt> for PricingTraceService {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        assert_eq!(request.model(), Model::Luna);
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        let output = match (call, request.kind()) {
            (0, ResponsesAttemptKind::Warmup) => ResponsesOutput::Warmup(WarmupResponse {
                id: "resp-warmup".to_owned(),
                usage: None,
                usage_metadata: None,
            }),
            (1, ResponsesAttemptKind::Generation) => {
                ResponsesOutput::Generation(GenerationOutput {
                    id: "resp-final".to_owned(),
                    status: "completed".to_owned(),
                    end_turn: Some(true),
                    final_message: Some("priced".to_owned()),
                    output_items: Vec::new(),
                    code_calls: Vec::new(),
                    usage: Some(Usage {
                        input_tokens: 1_000_000,
                        output_tokens: 1_000_000,
                        total_tokens: 2_000_000,
                        ..Usage::default()
                    }),
                    usage_metadata: None,
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: Some(0),
                    pipeline_stats: ResponsePipelineStats::default(),
                })
            }
            _ => panic!("unexpected attempt {call}: {:?}", request.kind()),
        };
        ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

fn pending_tool_generation() -> ResponsesOutput {
    let item = serde_json::from_value(json!({
        "type": "function_call",
        "call_id": "call-pending",
        "namespace": "trace__",
        "name": "pending",
        "arguments": "{}"
    }))
    .expect("function call item decodes");
    ResponsesOutput::Generation(GenerationOutput {
        id: "resp-tool".to_owned(),
        status: "completed".to_owned(),
        end_turn: Some(false),
        final_message: None,
        output_items: vec![item],
        code_calls: vec![CodeCall {
            call_id: "call-pending".to_owned(),
            name: "pending".to_owned(),
            namespace: Some("trace__".to_owned()),
            input: "{}".to_owned(),
            kind: CodeCallKind::Function,
        }],
        usage: None,
        usage_metadata: None,
        time_to_first_event_ns: 0,
        time_to_first_output_ns: None,
        pipeline_stats: ResponsePipelineStats::default(),
    })
}

fn final_generation() -> ResponsesOutput {
    ResponsesOutput::Generation(GenerationOutput {
        id: "resp-final".to_owned(),
        status: "completed".to_owned(),
        end_turn: Some(true),
        final_message: Some("steering recorded".to_owned()),
        output_items: Vec::new(),
        code_calls: Vec::new(),
        usage: None,
        usage_metadata: None,
        time_to_first_event_ns: 0,
        time_to_first_output_ns: Some(0),
        pipeline_stats: ResponsePipelineStats::default(),
    })
}

#[derive(Clone, Default)]
struct TraceCapture(Arc<Mutex<CapturedSpans>>);

#[derive(Clone, Default)]
struct TraceEventCapture(Arc<Mutex<Vec<CapturedTraceEvent>>>);

#[derive(Default)]
struct CapturedSpans {
    // The registry pools raw IDs after close. Preserve completed spans while
    // resolving records and parents through only the currently active IDs.
    spans: Vec<CapturedSpan>,
    active: HashMap<u64, usize>,
}

#[derive(Clone)]
struct CapturedSpan {
    name: &'static str,
    parent: Option<usize>,
    opened: Instant,
    closed: Option<Instant>,
    fields: HashMap<String, String>,
}

struct CapturedTraceEvent {
    scope: Vec<&'static str>,
    fields: HashMap<String, String>,
}

struct FieldCapture<'a>(&'a mut HashMap<String, String>);

impl Visit for FieldCapture<'_> {
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().to_owned(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_owned(), value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().to_owned(), value.to_string());
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.to_owned());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_owned(), format!("{value:?}"));
    }
}

impl<S> Layer<S> for TraceCapture
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: LayerContext<'_, S>) {
        let mut fields = HashMap::new();
        attributes.record(&mut FieldCapture(&mut fields));
        let parent = attributes
            .parent()
            .map(|parent| parent.clone().into_u64())
            .or_else(|| {
                attributes
                    .is_contextual()
                    .then(|| context.current_span().id().map(Id::into_u64))
                    .flatten()
            });
        let mut captured = self.0.lock().unwrap();
        let parent = parent.and_then(|parent| captured.active.get(&parent).copied());
        let span_index = captured.spans.len();
        captured.spans.push(CapturedSpan {
            name: attributes.metadata().name(),
            parent,
            opened: Instant::now(),
            closed: None,
            fields,
        });
        assert!(
            captured
                .active
                .insert(id.clone().into_u64(), span_index)
                .is_none(),
            "tracing registry reused an active span ID"
        );
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, _context: LayerContext<'_, S>) {
        let mut captured = self.0.lock().unwrap();
        let Some(span_index) = captured.active.get(&id.clone().into_u64()).copied() else {
            return;
        };
        values.record(&mut FieldCapture(&mut captured.spans[span_index].fields));
    }

    fn on_close(&self, id: Id, _context: LayerContext<'_, S>) {
        let mut captured = self.0.lock().unwrap();
        let Some(span_index) = captured.active.remove(&id.into_u64()) else {
            return;
        };
        captured.spans[span_index].closed = Some(Instant::now());
    }
}

impl<S> Layer<S> for TraceEventCapture
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event<'_>, context: LayerContext<'_, S>) {
        let mut fields = HashMap::new();
        event.record(&mut FieldCapture(&mut fields));
        let scope = context.event_scope(event).map_or_else(Vec::new, |scope| {
            scope
                .from_root()
                .map(|span| span.metadata().name())
                .collect()
        });
        self.0
            .lock()
            .unwrap()
            .push(CapturedTraceEvent { scope, fields });
    }
}

#[test]
fn luna_model_call_span_uses_luna_rates() {
    let capture = TraceCapture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let dispatch = tracing::Dispatch::new(subscriber);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async {
            let calls = Arc::new(AtomicU32::new(0));
            let service_calls = Arc::clone(&calls);
            let openai = OpenAi::builder("test")
                .service(move || PricingTraceService {
                    calls: Arc::clone(&service_calls),
                })
                .build()
                .unwrap();
            let (agent, events) = Nanocodex::builder(openai)
                .model(Model::Luna)
                .tools(Tools::builder().without_defaults().build().unwrap())
                .build()
                .unwrap();

            agent
                .prompt("price this turn")
                .await
                .unwrap()
                .await
                .unwrap();
            agent.shutdown().await.unwrap();
            drop((agent, events));
            assert_eq!(calls.load(Ordering::Relaxed), 2);
        });
    });

    let capture = capture.0.lock().unwrap();
    let call = capture
        .spans
        .iter()
        .find(|span| span.name == "model.call")
        .expect("model call span was captured");
    assert_eq!(
        call.fields.get("model").map(String::as_str),
        Some("gpt-5.6-luna")
    );
    assert_eq!(call.fields.get("cost.usd").map(String::as_str), Some("1.4"));
}

#[test]
fn steering_content_is_traced_in_prompt_order() {
    const PROMPT: &str = "trace the initial prompt exactly";
    const STEER: &str = "NANOCODEX_TRACE_STEER_SENTINEL";

    let events_capture = TraceEventCapture::default();
    let subscriber = tracing_subscriber::registry().with(events_capture.clone());
    let dispatch = tracing::Dispatch::new(subscriber);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async {
            let calls = Arc::new(AtomicU32::new(0));
            let started = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let service_calls = Arc::clone(&calls);
            let openai = OpenAi::builder("test")
                .service(move || SteeringTraceService {
                    calls: Arc::clone(&service_calls),
                })
                .build()
                .unwrap();
            let tools = Tools::builder()
                .without_defaults()
                .tool(SteeringBarrierTool {
                    started: Arc::clone(&started),
                    release: Arc::clone(&release),
                })
                .build()
                .unwrap();
            let (agent, events) = Nanocodex::builder(openai).tools(tools).build().unwrap();
            let turn = agent.prompt(PROMPT).await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
                .await
                .expect("steering barrier tool did not start");
            turn.steer(STEER).await.unwrap();
            release.notify_one();
            assert_eq!(
                turn.result().await.unwrap().into_final_message(),
                "steering recorded"
            );
            agent.shutdown().await.unwrap();
            drop((agent, events));
            assert_eq!(calls.load(Ordering::Relaxed), 3);
        });
    });

    let events = events_capture.0.lock().unwrap();
    let prompt_index = events
        .iter()
        .position(|event| {
            event.fields.get("content_kind").map(String::as_str) == Some("prompt")
                && event
                    .fields
                    .get("content")
                    .is_some_and(|content| content.contains(PROMPT))
        })
        .expect("prompt content event was not captured");
    let steer_index = events
        .iter()
        .position(|event| {
            event.fields.get("content_kind").map(String::as_str) == Some("steer")
                && event
                    .fields
                    .get("content")
                    .is_some_and(|content| content.contains(STEER))
                && event.scope.contains(&"agent.turn")
        })
        .expect("steering content event was not captured under the turn");
    assert!(
        prompt_index < steer_index,
        "steering content preceded the original prompt in the trace"
    );
}

#[test]
fn contextual_child_turns_preserve_parallel_orchestration_parentage() {
    let capture = TraceCapture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let dispatch = tracing::Dispatch::new(subscriber);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async {
            let (handles, mut received_handles) = mpsc::unbounded_channel();
            let openai = OpenAi::builder("test")
                .service(|| PendingService)
                .build()
                .unwrap();
            let (root, root_events) = Nanocodex::builder(openai)
                .tools_factory(move |handle| {
                    drop(handles.send(handle));
                    Tools::builder().without_defaults().build()
                })
                .build()
                .unwrap();
            let root_handle = received_handles.recv().await.unwrap();
            let (child_a, first_events) = root_handle.spawn().await.unwrap();
            let (child_b, second_events) = root_handle.spawn().await.unwrap();
            let (controls, mut received_controls) = mpsc::unbounded_channel();

            let (task_a, task_b) = async {
                let controls_a = controls.clone();
                let task_a = tokio::spawn(
                    async move {
                        let turn = child_a.prompt("child a").await.unwrap();
                        controls_a.send(turn.control()).unwrap();
                        assert!(matches!(
                            turn.result().await,
                            Err(NanocodexError::TurnCancelled)
                        ));
                    }
                    .instrument(info_span!("test.spawn_agent", child = "a")),
                );
                let task_b = tokio::spawn(
                    async move {
                        let turn = child_b.prompt("child b").await.unwrap();
                        controls.send(turn.control()).unwrap();
                        assert!(matches!(
                            turn.result().await,
                            Err(NanocodexError::TurnCancelled)
                        ));
                    }
                    .instrument(info_span!("test.spawn_agent", child = "b")),
                );
                (task_a, task_b)
            }
            .instrument(info_span!("test.code_mode.cell"))
            .await;

            let control_a = received_controls.recv().await.unwrap();
            let control_b = received_controls.recv().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let (cancel_a, cancel_b) = tokio::join!(control_a.cancel(), control_b.cancel());
            cancel_a.unwrap();
            cancel_b.unwrap();
            task_a.await.unwrap();
            task_b.await.unwrap();

            let openai = OpenAi::builder("test")
                .service(|| PendingService)
                .build()
                .unwrap();
            let (plain, plain_events) = Nanocodex::builder(openai).build().unwrap();
            let plain_turn = plain.prompt("plain root turn").await.unwrap();
            plain_turn.cancel().await.unwrap();
            assert!(matches!(
                plain_turn.result().await,
                Err(NanocodexError::TurnCancelled)
            ));

            drop((plain, plain_events, root, root_events));
            drop((first_events, second_events));
        });
    });

    let capture = capture.0.lock().unwrap();
    let spans = &capture.spans;
    let turns = spans
        .iter()
        .filter(|span| span.name == "agent.turn")
        .collect::<Vec<_>>();
    assert_eq!(turns.len(), 3);

    let child_turns = turns
        .iter()
        .filter(|span| {
            span.parent
                .and_then(|parent| spans.get(parent))
                .is_some_and(|parent| parent.name == "test.spawn_agent")
        })
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(child_turns.len(), 2);
    assert!(turns.iter().any(|span| span.parent.is_none()));

    let first = child_turns[0];
    let second = child_turns[1];
    assert!(
        first.opened < second.closed.unwrap() && second.opened < first.closed.unwrap(),
        "child turn intervals should overlap"
    );
}

#[test]
fn cancelled_tool_span_records_its_terminal_state_before_closing() {
    let capture = TraceCapture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let dispatch = tracing::Dispatch::new(subscriber);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async {
            let calls = Arc::new(AtomicU32::new(0));
            let started = Arc::new(tokio::sync::Notify::new());
            let service_calls = Arc::clone(&calls);
            let openai = OpenAi::builder("test")
                .service(move || PendingToolService {
                    calls: Arc::clone(&service_calls),
                })
                .build()
                .unwrap();
            let tools = Tools::builder()
                .without_defaults()
                .tool(PendingSpanTool {
                    started: Arc::clone(&started),
                })
                .build()
                .unwrap();
            let (agent, events) = Nanocodex::builder(openai).tools(tools).build().unwrap();
            let turn = agent.prompt("run pending tool").await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
                .await
                .expect("pending tool did not start");
            turn.cancel().await.unwrap();
            assert!(matches!(
                turn.result().await,
                Err(NanocodexError::TurnCancelled)
            ));
            agent.shutdown().await.unwrap();
            drop((agent, events));
            assert_eq!(calls.load(Ordering::Relaxed), 2);
        });
    });

    let capture = capture.0.lock().unwrap();
    let spans = &capture.spans;
    let tool = spans
        .iter()
        .find(|span| span.name == "tool.call")
        .expect("tool span was not captured");
    let turn = spans
        .iter()
        .find(|span| span.name == "agent.turn")
        .unwrap_or_else(|| {
            panic!(
                "turn span was not captured; observed: {:?}",
                spans.iter().map(|span| span.name).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        turn.fields.get("status").map(String::as_str),
        Some("cancelled")
    );
    assert_eq!(
        turn.fields.get("otel.status_code").map(String::as_str),
        Some("ERROR")
    );
    assert!(turn.closed.is_some(), "cancelled turn span did not close");
    assert_eq!(
        tool.fields.get("status").map(String::as_str),
        Some("cancelled")
    );
    assert_eq!(
        tool.fields.get("otel.status_code").map(String::as_str),
        Some("ERROR")
    );
    assert!(
        tool.fields
            .get("duration_ns")
            .and_then(|duration| duration.parse::<u64>().ok())
            .is_some_and(|duration| duration > 0)
    );
    assert!(tool.closed.is_some(), "tool span did not close");
}
