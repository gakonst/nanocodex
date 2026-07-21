use criterion::{criterion_group, criterion_main};

mod tui {
    use std::{hint::black_box, sync::Arc, time::Instant};

    use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
    use nanocodex::{AgentEvent, AgentEventKind, AgentEventTiming, TimedAgentEvent};
    use ratatui::{Terminal, backend::TestBackend};

    #[allow(dead_code, unused_imports)]
    mod transcript {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/tui/transcript.rs"
        ));
    }

    #[allow(dead_code, unused_imports)]
    mod composer {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tui/composer.rs"));
    }

    #[allow(dead_code, unused_imports)]
    mod app {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tui/app.rs"));
    }

    #[allow(dead_code, unused_imports)]
    mod view {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tui/view.rs"));
    }

    #[allow(dead_code, unused_imports)]
    mod terminal {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tui/terminal.rs"));
    }

    #[allow(dead_code, unused_imports)]
    mod telemetry {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tui/telemetry.rs"));
    }

    use app::App;
    use telemetry::StreamTelemetry;
    use terminal::DrawMetrics;
    use transcript::{ToolStatus, Transcript, TranscriptItem};

    #[derive(Clone, Copy)]
    struct TraceShape {
        name: &'static str,
        user_messages: usize,
        user_chars: usize,
        assistant_messages: usize,
        assistant_chars: usize,
        tool_calls: usize,
        tool_argument_chars: usize,
    }

    // Sanitized structural summaries derived from a long local Codex rollout and
    // the longest Amp thread returned by `amp threads list --include-archived` on
    // 2026-07-20. No prompt, tool argument, result, or user content is retained.
    const TRACE_SHAPES: [TraceShape; 2] = [
        TraceShape {
            name: "codex_long",
            user_messages: 78,
            user_chars: 30_486,
            assistant_messages: 964,
            assistant_chars: 308_701,
            tool_calls: 3_471,
            tool_argument_chars: 1_438_038,
        },
        TraceShape {
            name: "amp_long",
            user_messages: 38,
            user_chars: 4_716,
            assistant_messages: 199,
            assistant_chars: 69_676,
            tool_calls: 241,
            tool_argument_chars: 162_209,
        },
    ];

    const TERMINAL_SIZES: [(u16, u16); 3] = [(80, 24), (120, 40), (200, 60)];

    fn sized_text(len: usize, salt: usize) -> String {
        const WORDS: [&str; 8] = [
            "workspace",
            "response",
            "compile",
            "tool",
            "stream",
            "unicode",
            "λ",
            "🦀",
        ];
        let mut text = String::with_capacity(len);
        let mut index = salt;
        while text.len() < len {
            if !text.is_empty() {
                text.push(if index.is_multiple_of(19) { '\n' } else { ' ' });
            }
            text.push_str(WORDS[index % WORDS.len()]);
            index += 1;
        }
        while text.len() > len {
            text.pop();
        }
        text
    }

    fn distribute(total: usize, count: usize, index: usize) -> usize {
        if count == 0 {
            return 0;
        }
        let base = total / count;
        let remainder = total % count;
        base + usize::from(index < remainder)
    }

    fn trace_app(shape: TraceShape) -> App {
        let mut app = App::new("/workspace/nanocodex".into());
        let turns = shape
            .user_messages
            .max(shape.assistant_messages)
            .max(shape.tool_calls);

        for index in 0..turns {
            if index < shape.user_messages {
                app.main.transcript.push_editable_user(
                    sized_text(
                        distribute(shape.user_chars, shape.user_messages, index),
                        index,
                    ),
                    index as u64 + 1,
                );
            }
            if index < shape.assistant_messages {
                app.main
                    .transcript
                    .push(TranscriptItem::Assistant(sized_text(
                        distribute(shape.assistant_chars, shape.assistant_messages, index),
                        index + 1,
                    )));
            }
            if index < shape.tool_calls {
                app.main.transcript.push(TranscriptItem::Tool {
                    call_id: format!("call-{index}"),
                    name: "exec_command".to_owned(),
                    arguments: sized_text(
                        distribute(shape.tool_argument_chars, shape.tool_calls, index).min(180),
                        index + 2,
                    ),
                    status: ToolStatus::Completed,
                });
            }
        }
        // The benchmark models a partially streamed tail after retained history.
        app.main
            .transcript
            .push(TranscriptItem::Assistant(sized_text(2_048, turns + 1)));
        app
    }

    pub(super) fn render_benchmarks(criterion: &mut Criterion) {
        let mut group = criterion.benchmark_group("tui_trace_render");
        for shape in TRACE_SHAPES {
            let item_count = shape.user_messages + shape.assistant_messages + shape.tool_calls;
            group.throughput(Throughput::Elements(item_count as u64));
            for (scroll_name, scroll_from_bottom) in [("tail", 0), ("scrolled_4k", 4_000)] {
                for (width, height) in TERMINAL_SIZES {
                    let mut app = trace_app(shape);
                    app.main.scroll_from_bottom = scroll_from_bottom;
                    let mut terminal = Terminal::new(TestBackend::new(width, height))
                        .expect("trace benchmark terminal should initialize");
                    terminal
                        .draw(|frame| view::render(frame, &mut app))
                        .expect("initial trace benchmark frame should render");

                    group.bench_with_input(
                        BenchmarkId::new(shape.name, format!("{scroll_name}/{width}x{height}")),
                        &(width, height),
                        |bencher, _| {
                            bencher.iter(|| {
                                // Invalidate the streaming tail's wrapped-height cache without
                                // growing the fixture across Criterion iterations.
                                assert!(app.main.transcript.append_assistant_delta(""));
                                terminal
                                    .draw(|frame| view::render(frame, &mut app))
                                    .expect("trace benchmark frame should render");
                            });
                        },
                    );
                }
            }
        }
        group.finish();
    }

    pub(super) fn resize_benchmarks(criterion: &mut Criterion) {
        let mut group = criterion.benchmark_group("tui_trace_resize");
        for shape in TRACE_SHAPES {
            let item_count = shape.user_messages + shape.assistant_messages + shape.tool_calls;
            group.throughput(Throughput::Elements(item_count as u64));
            group.bench_function(BenchmarkId::new(shape.name, "80x24_to_200x60"), |bencher| {
                let mut app = trace_app(shape);
                let mut terminal = Terminal::new(TestBackend::new(80, 24))
                    .expect("resize benchmark terminal should initialize");
                let mut large = true;
                bencher.iter(|| {
                    let (width, height) = if large { (200, 60) } else { (80, 24) };
                    large = !large;
                    terminal.backend_mut().resize(width, height);
                    terminal
                        .autoresize()
                        .expect("resize benchmark terminal should autoresize");
                    terminal
                        .draw(|frame| view::render(frame, &mut app))
                        .expect("resized trace benchmark frame should render");
                });
            });
        }
        group.finish();
    }

    pub(super) fn transcript_update_benchmark(criterion: &mut Criterion) {
        criterion.bench_function("tui_transcript_delta/assistant_2k", |bencher| {
            bencher.iter_batched(
                || {
                    let mut transcript = Transcript::default();
                    transcript.push(TranscriptItem::Assistant(sized_text(2_048, 1)));
                    transcript
                },
                |mut transcript| {
                    assert!(transcript.append_assistant_delta(black_box("delta")));
                    black_box(transcript);
                },
                BatchSize::SmallInput,
            );
        });
    }

    pub(super) fn scroll_anchor_benchmark(criterion: &mut Criterion) {
        const DELTAS: usize = 128;
        fn scrolled_app() -> App {
            let mut app = App::new("/workspace/nanocodex".into());
            for index in 0..50 {
                app.main
                    .transcript
                    .push(TranscriptItem::User(sized_text(160, index)));
            }
            app.main.push_assistant_delta(&sized_text(2_048, 51));
            let mut terminal = Terminal::new(TestBackend::new(120, 40))
                .expect("scroll benchmark terminal should initialize");
            terminal
                .draw(|frame| view::render(frame, &mut app))
                .expect("initial scroll benchmark frame should render");
            app.main.scroll_from_bottom = 100;
            app
        }

        let mut group = criterion.benchmark_group("tui_scroll_anchor");
        group.throughput(Throughput::Elements(DELTAS as u64));
        group.bench_function("apply_128_deltas_scrolled", |bencher| {
            bencher.iter_batched(
                scrolled_app,
                |mut app| {
                    for _ in 0..DELTAS {
                        app.main.push_assistant_delta(black_box(" streamed delta"));
                    }
                    black_box(app);
                },
                BatchSize::SmallInput,
            );
        });
        group.bench_function("settle_128_deltas_scrolled/118_columns", |bencher| {
            bencher.iter_batched(
                || {
                    let mut app = scrolled_app();
                    for _ in 0..DELTAS {
                        app.main.push_assistant_delta(" streamed delta");
                    }
                    app
                },
                |mut app| {
                    app.main.settle_viewport(118, 33);
                    black_box(app);
                },
                BatchSize::SmallInput,
            );
        });
        group.bench_function("coalesced_128_deltas_scrolled/120x40", |bencher| {
            bencher.iter_batched(
                || {
                    let mut app = scrolled_app();
                    let mut terminal = Terminal::new(TestBackend::new(120, 40))
                        .expect("scroll benchmark terminal should initialize");
                    terminal
                        .draw(|frame| view::render(frame, &mut app))
                        .expect("initial scroll benchmark frame should render");
                    (app, terminal)
                },
                |(mut app, mut terminal)| {
                    for _ in 0..DELTAS {
                        app.main.push_assistant_delta(black_box(" streamed delta"));
                    }
                    terminal
                        .draw(|frame| view::render(frame, &mut app))
                        .expect("anchored scroll benchmark frame should render");
                    black_box(app.main.scroll_from_bottom);
                },
                BatchSize::SmallInput,
            );
        });
        group.finish();
    }

    pub(super) fn smooth_follow_benchmark(criterion: &mut Criterion) {
        const DELTAS: usize = 128;

        fn following_app() -> App {
            let mut app = App::new("/workspace/nanocodex".into());
            for index in 0..50 {
                app.main
                    .transcript
                    .push(TranscriptItem::User(sized_text(160, index)));
            }
            app.main.push_assistant_delta(&sized_text(2_048, 51));
            app.main.settle_viewport(118, 38);
            app
        }

        fn queued_animation() -> (App, Terminal<TestBackend>) {
            let mut app = following_app();
            let mut terminal = Terminal::new(TestBackend::new(120, 40))
                .expect("smooth-follow benchmark terminal should initialize");
            terminal
                .draw(|frame| view::render(frame, &mut app))
                .expect("initial smooth-follow frame should render");
            for _ in 0..DELTAS {
                app.main.push_assistant_delta("\nstreamed viewport row");
            }
            terminal
                .draw(|frame| view::render(frame, &mut app))
                .expect("burst smooth-follow frame should render");
            assert!(app.smooth_scroll_pending());
            (app, terminal)
        }

        let mut group = criterion.benchmark_group("tui_smooth_follow");
        group.sample_size(30);
        group.throughput(Throughput::Elements(DELTAS as u64));
        group.bench_function("settle_128_new_rows/118_columns", |bencher| {
            bencher.iter_batched(
                || {
                    let mut app = following_app();
                    for _ in 0..DELTAS {
                        app.main.push_assistant_delta("\nstreamed viewport row");
                    }
                    app
                },
                |mut app| {
                    app.main.settle_viewport(118, 38);
                    black_box(app.main.display_scroll_from_bottom());
                },
                BatchSize::SmallInput,
            );
        });
        group.bench_function("animate_one_row/120x40", |bencher| {
            bencher.iter_batched(
                queued_animation,
                |(mut app, mut terminal)| {
                    app.advance_smooth_scroll();
                    terminal
                        .draw(|frame| view::render(frame, &mut app))
                        .expect("smooth-follow animation frame should render");
                    black_box(app.main.display_scroll_from_bottom());
                },
                BatchSize::SmallInput,
            );
        });
        group.bench_function("drain_bounded_backlog/120x40", |bencher| {
            bencher.iter_batched(
                queued_animation,
                |(mut app, mut terminal)| {
                    let mut frames = 0_u64;
                    while app.smooth_scroll_pending() {
                        app.advance_smooth_scroll();
                        terminal
                            .draw(|frame| view::render(frame, &mut app))
                            .expect("smooth-follow animation frame should render");
                        frames += 1;
                    }
                    black_box(frames);
                },
                BatchSize::SmallInput,
            );
        });
        group.finish();
    }

    pub(super) fn stream_telemetry_benchmark(criterion: &mut Criterion) {
        const DELTAS: u64 = 1_024;
        let events = std::iter::once(AgentEventKind::RunStarted)
            .chain(std::iter::repeat_n(
                AgentEventKind::AssistantDelta,
                usize::try_from(DELTAS).unwrap(),
            ))
            .enumerate()
            .map(|(index, kind)| TimedAgentEvent {
                event: AgentEvent {
                    protocol_version: 1,
                    request_id: Arc::from("benchmark-session"),
                    seq: index as u64 + 1,
                    kind,
                    payload: serde_json::value::to_raw_value(&serde_json::json!({
                        "text": "delta"
                    }))
                    .unwrap(),
                },
                timing: AgentEventTiming {
                    emitted_ns: 0,
                    source_received_ns: (kind == AgentEventKind::AssistantDelta).then_some(0),
                },
            })
            .collect::<Vec<_>>();
        let app = App::new("/workspace/nanocodex".into());
        let mut group = criterion.benchmark_group("tui_stream_telemetry");
        group.throughput(Throughput::Elements(DELTAS));
        group.bench_function("apply_1024_and_present", |bencher| {
            bencher.iter(|| {
                let mut telemetry = StreamTelemetry::default();
                for event in &events {
                    let received = telemetry.event_received(app::PaneId::Main, event);
                    telemetry.event_applied(received, true);
                }
                let now = Instant::now();
                telemetry.frame_presented(now, now, DrawMetrics::default(), &app);
                black_box(telemetry);
            });
        });
        group.finish();
    }

    pub(super) fn first_frame_benchmarks(criterion: &mut Criterion) {
        let mut group = criterion.benchmark_group("tui_trace_first_frame");
        for shape in TRACE_SHAPES {
            let item_count = shape.user_messages + shape.assistant_messages + shape.tool_calls;
            group.throughput(Throughput::Elements(item_count as u64));
            group.bench_function(BenchmarkId::new(shape.name, "120x40"), |bencher| {
                bencher.iter_batched(
                    || {
                        let app = trace_app(shape);
                        let terminal = Terminal::new(TestBackend::new(120, 40))
                            .expect("trace benchmark terminal should initialize");
                        (app, terminal)
                    },
                    |(mut app, mut terminal)| {
                        terminal
                            .draw(|frame| view::render(frame, &mut app))
                            .expect("first trace benchmark frame should render");
                    },
                    BatchSize::LargeInput,
                );
            });
        }
        group.finish();
    }

    pub(super) fn composer_benchmarks(criterion: &mut Criterion) {
        criterion.bench_function("tui_composer_render/multiline_100k/120x40", |bencher| {
            let mut app = App::new("/workspace/nanocodex".into());
            app.input = sized_text(100 * 1_024, 7);
            app.cursor = app.input.len();
            let mut terminal = Terminal::new(TestBackend::new(120, 40))
                .expect("composer benchmark terminal should initialize");
            terminal
                .draw(|frame| view::render(frame, &mut app))
                .expect("initial composer benchmark frame should render");
            bencher.iter(|| {
                terminal
                    .draw(|frame| view::render(frame, &mut app))
                    .expect("composer benchmark frame should render");
            });
        });
    }

    pub(super) fn history_navigation_benchmarks(criterion: &mut Criterion) {
        let mut group = criterion.benchmark_group("tui_history_navigation");
        for shape in TRACE_SHAPES {
            group.throughput(Throughput::Elements(shape.user_messages as u64));
            group.bench_function(
                BenchmarkId::new(shape.name, "select_all_prompts"),
                |bencher| {
                    bencher.iter_batched(
                        || trace_app(shape),
                        |mut app| {
                            for _ in 0..shape.user_messages {
                                app.move_up();
                            }
                            black_box(app)
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
            group.bench_function(
                BenchmarkId::new(shape.name, "select_latest_and_render/120x40"),
                |bencher| {
                    bencher.iter_batched(
                        || {
                            let app = trace_app(shape);
                            let terminal = Terminal::new(TestBackend::new(120, 40))
                                .expect("history benchmark terminal should initialize");
                            (app, terminal)
                        },
                        |(mut app, mut terminal)| {
                            app.move_up();
                            terminal
                                .draw(|frame| view::render(frame, &mut app))
                                .expect("selected history frame should render");
                            black_box((app, terminal))
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
            group.bench_function(
                BenchmarkId::new(shape.name, "edit_latest_and_render/120x40"),
                |bencher| {
                    bencher.iter_batched(
                        || {
                            let mut app = trace_app(shape);
                            app.move_up();
                            assert!(app.start_historical_edit());
                            let terminal = Terminal::new(TestBackend::new(120, 40))
                                .expect("inline edit benchmark terminal should initialize");
                            (app, terminal)
                        },
                        |(mut app, mut terminal)| {
                            terminal
                                .draw(|frame| view::render(frame, &mut app))
                                .expect("inline history editor frame should render");
                            black_box((app, terminal))
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
        }
        group.finish();
    }

    pub(super) fn branch_state_benchmarks(criterion: &mut Criterion) {
        let mut group = criterion.benchmark_group("tui_branch_state");
        for shape in TRACE_SHAPES {
            group.bench_function(
                BenchmarkId::new(shape.name, "fork_visible_prefix"),
                |bencher| {
                    bencher.iter_batched(
                        || {
                            let mut app = trace_app(shape);
                            app.move_up();
                            assert!(app.start_historical_edit());
                            app
                        },
                        |mut app| {
                            let request = app
                                .commit_historical_edit()
                                .expect("trace prompt should be editable");
                            let prompt = app.main_branch_opened(
                                request.new_branch,
                                request.source_branch,
                                request.prompt,
                                Arc::from("benchmark-branch"),
                            );
                            black_box((app, prompt))
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
            group.bench_function(BenchmarkId::new(shape.name, "switch_branch"), |bencher| {
                bencher.iter_batched(
                    || {
                        let mut app = trace_app(shape);
                        app.move_up();
                        assert!(app.start_historical_edit());
                        let request = app
                            .commit_historical_edit()
                            .expect("trace prompt should be editable");
                        let _ = app.main_branch_opened(
                            request.new_branch,
                            request.source_branch,
                            request.prompt,
                            Arc::from("benchmark-branch"),
                        );
                        app
                    },
                    |mut app| {
                        let id = app
                            .cycle_main_branch(-1)
                            .expect("parent branch should be retained");
                        app.main_branch_switched(id, Arc::from("benchmark-parent"));
                        black_box(app)
                    },
                    BatchSize::SmallInput,
                );
            });
            group.bench_function(
                BenchmarkId::new(shape.name, "render_navigator/120x40"),
                |bencher| {
                    bencher.iter_batched(
                        || {
                            let mut app = trace_app(shape);
                            app.move_up();
                            assert!(app.start_historical_edit());
                            let request = app
                                .commit_historical_edit()
                                .expect("trace prompt should be editable");
                            let _ = app.main_branch_opened(
                                request.new_branch,
                                request.source_branch,
                                request.prompt,
                                Arc::from("benchmark-branch"),
                            );
                            assert!(app.toggle_branch_navigator());
                            app.move_branch_navigator(-1);
                            let terminal = Terminal::new(TestBackend::new(120, 40))
                                .expect("branch navigator terminal should initialize");
                            (app, terminal)
                        },
                        |(mut app, mut terminal)| {
                            terminal
                                .draw(|frame| view::render(frame, &mut app))
                                .expect("branch navigator frame should render");
                            black_box((app, terminal))
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
        }
        group.finish();
    }
}

criterion_group!(
    benches,
    tui::render_benchmarks,
    tui::resize_benchmarks,
    tui::transcript_update_benchmark,
    tui::scroll_anchor_benchmark,
    tui::smooth_follow_benchmark,
    tui::stream_telemetry_benchmark,
    tui::first_frame_benchmarks,
    tui::composer_benchmarks,
    tui::history_navigation_benchmarks,
    tui::branch_state_benchmarks
);
criterion_main!(benches);
