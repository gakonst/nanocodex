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

fn benchmark_active_boundary_snapshot(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("active_boundary_snapshot_then_append");
    for item_count in [100_usize, 1_000, 10_000] {
        group.throughput(Throughput::Elements(
            u64::try_from(item_count).expect("benchmark sizes fit in u64"),
        ));
        let items: Vec<_> = (0..item_count).map(history_item).collect();
        let active = ResponseHistory::new(items.clone());
        group.bench_with_input(
            BenchmarkId::new("immutable_boundary", item_count),
            &active,
            |bencher, history| {
                bencher.iter(|| {
                    let mut parent = history.clone();
                    parent.commit_tail();
                    let snapshot = parent.clone();
                    parent.push(history_item(usize::MAX));
                    black_box((parent, snapshot));
                });
            },
        );

        let copy_on_write = Arc::new(items);
        group.bench_with_input(
            BenchmarkId::new("arc_vec_boundary", item_count),
            &copy_on_write,
            |bencher, history| {
                bencher.iter(|| {
                    let snapshot = Arc::clone(history);
                    let mut parent = Arc::clone(history);
                    Arc::make_mut(&mut parent).push(history_item(usize::MAX));
                    black_box((parent, snapshot));
                });
            },
        );
    }
    group.finish();
}

fn benchmark_incremental_suffix(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("incremental_suffix_iteration");
    for item_count in [100_usize, 1_000, 10_000] {
        let mut history = ResponseHistory::default();
        for index in 0..item_count {
            history.push(history_item(index));
            history.commit_tail();
        }
        group.bench_with_input(
            BenchmarkId::new("last_item", item_count),
            &history,
            |bencher, history| {
                bencher
                    .iter(|| black_box(history.iter_from(history.len().saturating_sub(1)).count()));
            },
        );
    }
    group.finish();
}

fn benchmark_code_mode_history_snapshot(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("code_mode_history_snapshot");
    for item_count in [100_usize, 1_000, 10_000] {
        group.throughput(Throughput::Elements(
            u64::try_from(item_count).expect("benchmark sizes fit in u64"),
        ));
        let mut history = ResponseHistory::new((0..item_count).map(history_item).collect());
        history.commit_tail();

        group.bench_with_input(
            BenchmarkId::new("flatten_then_deep_clone", item_count),
            &history,
            |bencher, history| {
                bencher.iter(|| {
                    let flattened = history.iter().cloned().collect::<Vec<_>>();
                    let owned_cell = flattened.clone();
                    black_box((flattened, owned_cell));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("flatten_into_shared_owner", item_count),
            &history,
            |bencher, history| {
                bencher.iter(|| {
                    let owned_cell = Arc::new(history.iter().cloned().collect::<Vec<_>>());
                    black_box(owned_cell);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    benchmark_fork_append,
    benchmark_active_boundary_snapshot,
    benchmark_incremental_suffix,
    benchmark_code_mode_history_snapshot
);
criterion_main!(benches);
