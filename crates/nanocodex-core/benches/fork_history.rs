use std::{hint::black_box, sync::Arc};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use nanocodex_core::{ContentItem, MessageRole, ResponseItem, responses::ResponseHistory};

fn history_item(index: usize) -> ResponseItem {
    ResponseItem::message(
        if index.is_multiple_of(2) {
            MessageRole::User
        } else {
            MessageRole::Assistant
        },
        [ContentItem::InputText {
            text: format!("item-{index:05}-{}", "x".repeat(512)).into_boxed_str(),
        }],
    )
}

fn benchmark_fork_append(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("fork_then_append");
    for item_count in [100_usize, 1_000, 10_000] {
        group.throughput(Throughput::Elements(
            u64::try_from(item_count).expect("benchmark sizes fit in u64"),
        ));
        let items: Vec<_> = (0..item_count).map(history_item).collect();

        let mut segmented = ResponseHistory::new(items.clone());
        segmented.commit_tail();
        group.bench_with_input(
            BenchmarkId::new("immutable_segments", item_count),
            &segmented,
            |bencher, history| {
                bencher.iter(|| {
                    let mut branch = history.clone();
                    branch.push(history_item(usize::MAX));
                    black_box(branch);
                });
            },
        );

        let copy_on_write = Arc::new(items);
        group.bench_with_input(
            BenchmarkId::new("arc_vec_copy_on_write", item_count),
            &copy_on_write,
            |bencher, history| {
                bencher.iter(|| {
                    let mut branch = Arc::clone(history);
                    Arc::make_mut(&mut branch).push(history_item(usize::MAX));
                    black_box(branch);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_fork_append);
criterion_main!(benches);
