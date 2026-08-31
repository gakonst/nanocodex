use std::{
    future::{Ready, ready},
    hint::black_box,
    task::{Context, Poll},
    time::Duration,
};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use nanocodex_agent::{Nanocodex, OpenAi, ResponseError, Tools};
use nanocodex_oai_api::{
    responses::{ContentItem, MessageRole, ResponseItem, Usage, WarmupResponse},
    tower::{
        GenerationOutput, ResponsePipelineStats, ResponsesAttempt, ResponsesAttemptKind,
        ResponsesOutput, ResponsesServiceResponse,
    },
};
use tower::Service;

#[derive(Clone)]
struct ImmediateResponses;

impl Service<ResponsesAttempt> for ImmediateResponses {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        let output = match request.kind() {
            ResponsesAttemptKind::Warmup => ResponsesOutput::Warmup(WarmupResponse {
                id: "resp_warmup".to_owned(),
                usage: None,
                usage_metadata: None,
            }),
            ResponsesAttemptKind::Generation => {
                let message = ResponseItem::message(
                    MessageRole::Assistant,
                    [ContentItem::output_text("done")],
                );
                ResponsesOutput::Generation(GenerationOutput {
                    id: "resp_generation".to_owned(),
                    status: "completed".to_owned(),
                    end_turn: Some(true),
                    final_message: Some("done".to_owned()),
                    output_items: vec![message],
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
                panic!("the one-turn benchmark must not compact")
            }
        };
        ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

fn benchmark_agent_lifecycle(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("benchmark runtime");
    let workspace = tempfile::tempdir().expect("benchmark workspace");
    let openai = OpenAi::builder("benchmark-only")
        .service(|| ImmediateResponses)
        .build()
        .expect("the deterministic client recipe is valid");
    let tools = Tools::builder()
        .without_defaults()
        .build()
        .expect("the empty benchmark registry is valid");
    let builder = Nanocodex::builder(openai)
        .instructions("Reply with exactly `done`.")
        .workspace(workspace.path())
        .tools(tools);

    let (agent, events) = {
        let _runtime = runtime.enter();
        builder.clone().build().expect("benchmark agent")
    };
    drop(events);
    criterion.bench_function("agent_handle_clone", |bencher| {
        bencher.iter(|| black_box(agent.clone()));
    });
    drop(agent);

    let mut group = criterion.benchmark_group("agent_lifecycle");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(2));
    let runtime_handle = runtime.handle().clone();
    group.bench_function("accepted_first_turn_to_typed_result", |bencher| {
        bencher.to_async(&runtime).iter_batched(
            || {
                let _runtime = runtime_handle.enter();
                builder.clone().build().expect("benchmark agent")
            },
            |(agent, events)| async move {
                drop(events);
                let result = agent
                    .prompt("Return the required answer.")
                    .await
                    .expect("prompt accepted")
                    .await
                    .expect("turn completed");
                black_box((
                    result.final_message(),
                    result
                        .usage()
                        .map_or(0, nanocodex_agent::TurnUsage::total_tokens),
                ));
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, benchmark_agent_lifecycle);
criterion_main!(benches);
