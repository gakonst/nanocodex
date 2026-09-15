use std::{hint::black_box, time::Duration};

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use nanocodex_browser::{BrowserAction, BrowserTool};
use nanocodex_tools::{Tool, ToolContext, ToolInput};
use serde_json::value::RawValue;

const OPEN_INPUT: &str = r#"{"action":"open","url":"data:text/html,<main>benchmark</main>"}"#;
const ACTIONS_PER_BATCH: u64 = 100;

fn benchmark_protocol(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("benchmark runtime");
    let action = BrowserAction::Open {
        url: "data:text/html,<main>benchmark</main>".to_owned(),
    };
    let mut group = criterion.benchmark_group("browser_protocol");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(2));
    group.bench_function("typed_action_serde", |bencher| {
        bencher.iter(|| {
            let encoded = serde_json::to_vec(black_box(&action)).expect("serialize action");
            let decoded =
                serde_json::from_slice::<BrowserAction>(&encoded).expect("deserialize action");
            black_box(decoded);
        });
    });
    group.bench_function("recording_tool_call", |bencher| {
        bencher.to_async(&runtime).iter_batched(
            BrowserTool::recording,
            |(tool, recording)| async move {
                let input = ToolInput::Function(
                    RawValue::from_string(OPEN_INPUT.to_owned()).expect("valid action input"),
                );
                let context = ToolContext::new("benchmark", "session", "call", &[], 1_000);
                let execution = tool.execute(input, context).await.expect("record action");
                black_box(execution);
                black_box(recording.actions().expect("recorded actions"));
            },
            BatchSize::SmallInput,
        );
    });
    group.throughput(Throughput::Elements(ACTIONS_PER_BATCH));
    group.bench_function("retained_recording_100_actions", |bencher| {
        bencher.to_async(&runtime).iter_batched(
            BrowserTool::recording,
            |(tool, recording)| async move {
                for call in 0..ACTIONS_PER_BATCH {
                    let input = ToolInput::Function(
                        RawValue::from_string(OPEN_INPUT.to_owned()).expect("valid action input"),
                    );
                    let call_id = call.to_string();
                    let context = ToolContext::new("benchmark", "session", &call_id, &[], 1_000);
                    black_box(tool.execute(input, context).await.expect("record action"));
                }
                black_box(recording.actions().expect("recorded actions"));
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, benchmark_protocol);
criterion_main!(benches);
