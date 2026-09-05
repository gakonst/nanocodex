use std::{
    convert::Infallible,
    future::{Ready, ready},
    hint::black_box,
    task::{Context, Poll},
    time::Duration,
};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use nanocodex_oai_api::{
    OpenAi,
    responses::{ContentItem, MessageRole, ResponseItem, Usage},
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
    type Error = Infallible;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        assert!(matches!(request.kind(), ResponsesAttemptKind::Generation));
        let message =
            ResponseItem::message(MessageRole::Assistant, [ContentItem::output_text("done")]);
        ready(Ok(ResponsesServiceResponse::new(
            ResponsesOutput::Generation(GenerationOutput {
                id: "resp_benchmark".to_owned(),
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
                time_to_first_event_ns: 0,
                time_to_first_output_ns: Some(0),
                pipeline_stats: ResponsePipelineStats::default(),
            }),
        )))
    }
}

fn benchmark_session_lifecycle(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("benchmark runtime");
    let openai = OpenAi::builder("benchmark-only")
        .service(|| ImmediateResponses)
        .build()
        .expect("the deterministic client recipe is valid");
    let builder = openai.instructions("Reply with exactly `done`.");

    criterion.bench_function("oai_session_build", |bencher| {
        bencher.iter(|| black_box(builder.clone().build().expect("benchmark session")));
    });

    let mut group = criterion.benchmark_group("oai_session_lifecycle");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(2));
    group.bench_function("first_create_to_completed_response", |bencher| {
        bencher.to_async(&runtime).iter_batched(
            || builder.clone().build().expect("benchmark session"),
            |mut session| async move {
                let completed = session
                    .turn()
                    .create("Return the required answer.")
                    .await
                    .expect("response completed");
                black_box((
                    completed.output_text(),
                    completed.usage().map(|usage| usage.total_tokens),
                ));
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, benchmark_session_lifecycle);
criterion_main!(benches);
