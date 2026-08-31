use std::{
    convert::Infallible,
    future::{Ready, ready},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use futures_util::TryStreamExt;
use nanocodex_oai_api::{
    OpenAi, OpenAiError, ResponseEvent,
    responses::{ContentItem, FunctionOutputBody, JsonSchema, MessageRole, ResponseItem, Usage},
    session::ResponseInput,
    tools::ToolDefinition,
    tower::{
        GenerationOutput, LayeredServiceFactory, ResponsePipelineStats, ResponsesAttempt,
        ResponsesAttemptKind, ResponsesOutput, ResponsesServiceFactory, ResponsesServiceResponse,
        StandardServiceFactory,
    },
};
use serde_json::json;
use tower::Service;
use tower::timeout::TimeoutLayer;
use tracing::{Subscriber, span::Attributes};
use tracing_subscriber::{Layer, layer::Context as LayerContext, prelude::*, registry::LookupSpan};

#[derive(Clone)]
struct ScriptedResponses {
    calls: Arc<Mutex<Vec<ObservedAttempt>>>,
    tool_call_on_first_response: bool,
}

struct ObservedAttempt {
    previous_response_id: Option<String>,
    input_item_count: usize,
    input_items: Vec<ResponseItem>,
}

impl Service<ResponsesAttempt> for ScriptedResponses {
    type Response = ResponsesServiceResponse;
    type Error = Infallible;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        assert!(matches!(request.kind(), ResponsesAttemptKind::Generation));
        let mut calls = self.calls.lock().unwrap();
        calls.push(ObservedAttempt {
            previous_response_id: request.previous_response_id().map(str::to_owned),
            input_item_count: request.input_item_count(),
            input_items: request.input_items().cloned().collect(),
        });
        let index = calls.len();
        drop(calls);

        let output_items = if self.tool_call_on_first_response && index == 1 {
            vec![ResponseItem::FunctionCall {
                id: None,
                name: "lookup_region".into(),
                namespace: None,
                arguments: r#"{"region":"iad"}"#.into(),
                encrypted_function_args: None,
                call_id: "call_region_01".into(),
                caller: None,
                status: None,
                created_by: None,
                internal_chat_message_metadata_passthrough: None,
            }]
        } else {
            vec![ResponseItem::message(
                MessageRole::Assistant,
                [ContentItem::output_text(format!("answer-{index}"))],
            )]
        };
        ready(Ok(ResponsesServiceResponse::new(
            ResponsesOutput::Generation(GenerationOutput {
                id: format!("resp_{index}"),
                status: "completed".to_owned(),
                end_turn: Some(true),
                final_message: Some(format!("answer-{index}")),
                output_items,
                code_calls: Vec::new(),
                usage: Some(Usage {
                    input_tokens: 3,
                    output_tokens: 2,
                    total_tokens: 5,
                    ..Usage::default()
                }),
                usage_metadata: None,
                time_to_first_event_ns: 0,
                time_to_first_output_ns: Some(0),
                pipeline_stats: ResponsePipelineStats::default(),
            }),
        )))
    }
}

#[derive(Clone)]
struct ResponseCallCount(Arc<AtomicUsize>);

type TimedStandardFactory = LayeredServiceFactory<StandardServiceFactory, TimeoutLayer>;

fn timed_openai() -> Result<OpenAi<TimedStandardFactory>, OpenAiError> {
    OpenAi::builder("test-key")
        .layer(TimeoutLayer::new(std::time::Duration::from_secs(30)))
        .build()
}

const fn assert_nameable_factory<F>(_: &OpenAi<F>)
where
    F: ResponsesServiceFactory,
{
}

#[test]
fn layered_tower_factory_is_nameable_in_public_signatures() {
    let openai = timed_openai().unwrap();
    assert_nameable_factory(&openai);
}

impl<S> Layer<S> for ResponseCallCount
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(
        &self,
        attributes: &Attributes<'_>,
        _id: &tracing::Id,
        _context: LayerContext<'_, S>,
    ) {
        let metadata = attributes.metadata();
        if metadata.name() == "responses.call" && metadata.target() == "nanocodex_oai_api" {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[test]
fn public_session_streams_results_and_reuses_continuation_state() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let response_call_count = Arc::new(AtomicUsize::new(0));
    let subscriber =
        tracing_subscriber::registry().with(ResponseCallCount(Arc::clone(&response_call_count)));
    let dispatch = tracing::Dispatch::new(subscriber);
    // Keep more than one scoped dispatcher registered while parallel tests
    // first encounter this callsite. Otherwise tracing's single-dispatcher
    // fast path can cache the no-subscriber interest from another test thread.
    let _parallel_test_registration =
        tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async {
            let service_calls = Arc::clone(&calls);
            let openai = OpenAi::builder("test-key")
                .service(move || ScriptedResponses {
                    calls: Arc::clone(&service_calls),
                    tool_call_on_first_response: false,
                })
                .build()
                .unwrap();
            let mut session = openai
                .instructions("Preserve exact identifiers and answer concisely.")
                .build()
                .unwrap();

            {
                let mut first_turn = session.turn();
                let mut response = first_turn.create("Remember deployment req_7f3.");
                let mut completed_event_seen = false;
                while let Some(event) = response.try_next().await.unwrap() {
                    completed_event_seen |= matches!(event, ResponseEvent::Completed { .. });
                }
                let first = response.await.unwrap();
                assert!(completed_event_seen);
                assert_eq!(first.output_text(), "answer-1");
            }

            let second = session
                .turn()
                .create("Which deployment identifier did I provide?")
                .await
                .unwrap();
            assert_eq!(second.output_text(), "answer-2");
            assert!(session.history_len() >= 4);
        });
    });

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].previous_response_id, None);
    assert_eq!(calls[1].previous_response_id.as_deref(), Some("resp_1"));
    assert!(calls[0].input_item_count > calls[1].input_item_count);
    assert_eq!(response_call_count.load(Ordering::Relaxed), 2);
}

#[test]
fn public_session_accepts_tool_definitions_and_paired_tool_outputs() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let service_calls = Arc::clone(&calls);
        let openai = OpenAi::builder("test-key")
            .service(move || ScriptedResponses {
                calls: Arc::clone(&service_calls),
                tool_call_on_first_response: true,
            })
            .build()
            .unwrap();
        let mut session = openai
            .instructions("Use lookup_region for deployment questions. Preserve exact identifiers.")
            .tool_definitions([ToolDefinition::function(
                "lookup_region",
                "Return deployment metadata for one exact region identifier.",
                JsonSchema::from(json!({
                    "type": "object",
                    "properties": {
                        "region": { "type": "string" }
                    },
                    "required": ["region"],
                    "additionalProperties": false
                })),
            )])
            .build()
            .unwrap();

        session.turn().create("Check region iad.").await.unwrap();
        session
            .turn()
            .create(ResponseInput::items([ResponseItem::function_call_output(
                "call_region_01".to_owned(),
                FunctionOutputBody::Text(r#"{"region":"iad","status":"healthy"}"#.into()),
            )]))
            .await
            .unwrap();
    });

    let calls = calls.lock().unwrap();
    let first = &calls[0].input_items;
    assert!(first.iter().any(|item| {
        matches!(
            item,
            ResponseItem::AdditionalTools { tools, .. }
                if tools.iter().any(|tool| tool.name() == "lookup_region")
        )
    }));
    let second = &calls[1].input_items;
    assert!(second.iter().any(|item| {
        matches!(
            item,
            ResponseItem::FunctionCallOutput { call_id, .. }
                if call_id.as_ref() == "call_region_01"
        )
    }));
}
