use criterion::{criterion_group, criterion_main};

mod tui {
    use std::{cell::Cell, fmt::Write as _, hint::black_box, rc::Rc, sync::Arc, time::Instant};

    use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
    use nanocodex::{AgentEvent, AgentEventKind, AgentEventTiming, TimedAgentEvent};
    use ratatui::{
        Terminal, TerminalOptions, Viewport,
        backend::{CrosstermBackend, TestBackend},
        layout::Rect,
    };

    #[allow(dead_code, unused_imports)]
    mod markdown {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tui/markdown.rs"));
    }

    #[allow(dead_code, unused_imports)]
    mod diff {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tui/diff.rs"));
    }

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
    mod selection {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tui/selection.rs"));
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
    use terminal::{ByteCountingWriter, DrawMetrics, MeasuredBackend};
    use transcript::{ToolStatus, Transcript, TranscriptItem};

    #[derive(Clone, Copy)]
    struct TraceShape {
        name: &'static str,
        user_messages: usize,
        user_chars: usize,
        assistant_messages: usize,
        assistant_chars: usize,
        reasoning_messages: usize,
        reasoning_chars: usize,
        tool_calls: usize,
        tool_argument_chars: usize,
        tool_results: usize,
        tool_result_chars: usize,
    }

    // Sanitized structural summaries derived from a long local Codex rollout and
    // the longest Amp thread returned by `amp threads list --include-archived` on
    // 2026-07-22. Amp reasoning and tool-result totals are retained structurally;
    // the observed 269 KiB maximum result is isolated below. No prompt, tool
    // argument, result, or user content is retained.
    const TRACE_SHAPES: [TraceShape; 2] = [
        TraceShape {
            name: "codex_long",
            user_messages: 78,
            user_chars: 30_486,
            assistant_messages: 964,
            assistant_chars: 308_701,
            reasoning_messages: 0,
            reasoning_chars: 0,
            tool_calls: 3_471,
            tool_argument_chars: 1_438_038,
            tool_results: 0,
            tool_result_chars: 0,
        },
        TraceShape {
            name: "amp_long",
            user_messages: 22,
            user_chars: 3_763,
            assistant_messages: 72,
            assistant_chars: 56_681,
            reasoning_messages: 201,
            reasoning_chars: 610_004,
            tool_calls: 255,
            tool_argument_chars: 261_830,
            tool_results: 255,
            tool_result_chars: 1_718_915,
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

    fn trace_app_with_tail(shape: TraceShape, tail_chars: usize) -> App {
        let mut app = App::new("/workspace/nanocodex".into());
        let turns = shape
            .user_messages
            .max(shape.assistant_messages)
            .max(shape.reasoning_messages)
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
            if index < shape.reasoning_messages {
                app.main
                    .transcript
                    .push(TranscriptItem::Reasoning(sized_text(
                        distribute(shape.reasoning_chars, shape.reasoning_messages, index),
                        index + 2,
                    )));
            }
            if index < shape.tool_calls {
                let call_id = format!("call-{index}");
                app.main.transcript.push(TranscriptItem::Tool {
                    call_id: call_id.clone(),
                    name: "exec_command".to_owned(),
                    arguments: sized_text(
                        distribute(shape.tool_argument_chars, shape.tool_calls, index),
                        index + 3,
                    ),
                    status: ToolStatus::Completed,
                });
                if index < shape.tool_results {
                    assert!(app.main.transcript.set_tool_result(
                        &call_id,
                        ToolStatus::Completed,
                        Some(1_000_000),
                        Some(sized_text(
                            distribute(shape.tool_result_chars, shape.tool_results, index),
                            index + 4,
                        )),
                    ));
                }
            }
        }
        // The benchmark models a partially streamed tail after retained history.
        app.main
            .transcript
            .push(TranscriptItem::Assistant(sized_text(tail_chars, turns + 1)));
        app
    }

    fn trace_app(shape: TraceShape) -> App {
        trace_app_with_tail(shape, 2_048)
    }

    fn trace_app_with_single_line_tail(shape: TraceShape, tail_chars: usize) -> App {
        let mut app = trace_app_with_tail(shape, 0);
        assert!(
            app.main
                .transcript
                .append_assistant_delta(&"x".repeat(tail_chars))
        );
        app
    }

    type OutputTerminal = Terminal<MeasuredBackend<CrosstermBackend<ByteCountingWriter<Vec<u8>>>>>;

    fn output_terminal() -> (OutputTerminal, Rc<Cell<u64>>) {
        let output_bytes = Rc::new(Cell::new(0));
        let writer = ByteCountingWriter {
            inner: Vec::new(),
            bytes: Rc::clone(&output_bytes),
        };
        let backend = MeasuredBackend {
            inner: CrosstermBackend::new(writer),
            changed_cells: 0,
        };
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 120, 40)),
            },
        )
        .expect("output benchmark terminal should initialize");
        (terminal, output_bytes)
    }

    fn output_backlog_setup() -> (App, OutputTerminal, Rc<Cell<u64>>) {
        let mut app = App::new("/workspace/nanocodex".into());
        for index in 0..50 {
            app.main
                .transcript
                .push(TranscriptItem::User(sized_text(160, index)));
        }
        app.main.push_assistant_delta(&sized_text(2_048, 51));
        app.main.settle_viewport(118, 38);
        let (mut terminal, output_bytes) = output_terminal();
        terminal
            .draw(|frame| view::render(frame, &mut app))
            .expect("initial output benchmark frame should render");
        for _ in 0..128 {
            app.main.push_assistant_delta("\nstreamed viewport row");
        }
        terminal
            .draw(|frame| view::render(frame, &mut app))
            .expect("burst output benchmark frame should render");
        assert!(app.smooth_scroll_pending());
        (app, terminal, output_bytes)
    }

    fn fast_mode_toggle_setup() -> (App, OutputTerminal, Rc<Cell<u64>>) {
        let mut app = App::new("/workspace/nanocodex".into());
        let (mut terminal, output_bytes) = output_terminal();
        terminal
            .draw(|frame| view::render(frame, &mut app))
            .expect("initial footer frame should render");
        app.fast_mode_changed(true);
        (app, terminal, output_bytes)
    }

    fn draw_fast_mode_toggle(
        app: &mut App,
        terminal: &mut OutputTerminal,
        output_bytes: &Cell<u64>,
    ) -> DrawMetrics {
        let bytes_before = output_bytes.get();
        terminal.backend_mut().changed_cells = 0;
        terminal
            .draw(|frame| view::render(frame, app))
            .expect("fast-mode footer frame should render");
        DrawMetrics {
            changed_cells: terminal.backend().changed_cells,
            output_bytes: output_bytes.get().saturating_sub(bytes_before),
        }
    }

    pub(super) fn render_benchmarks(criterion: &mut Criterion) {
        let mut group = criterion.benchmark_group("tui_trace_render");
        for shape in TRACE_SHAPES {
            let item_count = shape.user_messages
                + shape.assistant_messages
                + shape.reasoning_messages
                + shape.tool_calls;
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
            let item_count = shape.user_messages
                + shape.assistant_messages
                + shape.reasoning_messages
                + shape.tool_calls;
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
        let mut group = criterion.benchmark_group("tui_transcript_delta");
        group.sample_size(20);
        for (name, tail_chars) in [
            ("assistant_2k", 2_048),
            ("assistant_100k", 100 * 1_024),
            ("assistant_1m", 1_024 * 1_024),
        ] {
            group.bench_function(name, |bencher| {
                bencher.iter_batched(
                    || {
                        let mut transcript = Transcript::default();
                        transcript.push(TranscriptItem::Assistant(sized_text(tail_chars, 1)));
                        black_box(transcript.tail_height(118));
                        transcript
                    },
                    |mut transcript| {
                        assert!(
                            transcript.append_assistant_delta(black_box("\nstreamed code line"))
                        );
                        black_box(transcript.tail_height(118));
                        transcript
                    },
                    BatchSize::LargeInput,
                );
            });
        }
        group.bench_function("reasoning_100k", |bencher| {
            bencher.iter_batched(
                || {
                    let mut transcript = Transcript::default();
                    transcript.push(TranscriptItem::Reasoning(sized_text(100 * 1_024, 3)));
                    black_box(transcript.tail_height(118));
                    transcript
                },
                |mut transcript| {
                    assert!(transcript.append_reasoning_delta(black_box("\nnext summary line")));
                    black_box(transcript.tail_height(118));
                    transcript
                },
                BatchSize::LargeInput,
            );
        });
        group.finish();
    }

    pub(super) fn live_tail_render_benchmark(criterion: &mut Criterion) {
        let mut group = criterion.benchmark_group("tui_live_tail_render");
        group.sample_size(20);
        for (name, tail_chars, single_line) in [
            ("assistant_2k", 2_048, false),
            ("assistant_100k", 100 * 1_024, false),
            ("assistant_1m", 1_024 * 1_024, false),
            ("assistant_1m_single_line", 1_024 * 1_024, true),
        ] {
            let mut app = if single_line {
                trace_app_with_single_line_tail(TRACE_SHAPES[0], tail_chars)
            } else {
                trace_app_with_tail(TRACE_SHAPES[0], tail_chars)
            };
            let mut terminal = Terminal::new(TestBackend::new(120, 40))
                .expect("live-tail benchmark terminal should initialize");
            terminal
                .draw(|frame| view::render(frame, &mut app))
                .expect("initial live-tail benchmark frame should render");
            group.bench_function(BenchmarkId::new(name, "120x40"), |bencher| {
                bencher.iter(|| {
                    terminal
                        .draw(|frame| view::render(frame, &mut app))
                        .expect("live-tail benchmark frame should render");
                });
            });
        }
        group.finish();
    }

    pub(super) fn live_tail_first_frame_benchmark(criterion: &mut Criterion) {
        let mut group = criterion.benchmark_group("tui_live_tail_first_frame");
        group.sample_size(10);
        for (name, tail_chars, single_line) in [
            ("assistant_100k", 100 * 1_024, false),
            ("assistant_1m", 1_024 * 1_024, false),
            ("assistant_1m_single_line", 1_024 * 1_024, true),
        ] {
            group.bench_function(BenchmarkId::new(name, "120x40"), |bencher| {
                bencher.iter_batched(
                    || {
                        let app = if single_line {
                            trace_app_with_single_line_tail(TRACE_SHAPES[0], tail_chars)
                        } else {
                            trace_app_with_tail(TRACE_SHAPES[0], tail_chars)
                        };
                        let terminal = Terminal::new(TestBackend::new(120, 40))
                            .expect("live-tail first-frame terminal should initialize");
                        (app, terminal)
                    },
                    |(mut app, mut terminal)| {
                        terminal
                            .draw(|frame| view::render(frame, &mut app))
                            .expect("live-tail first frame should render");
                        (app, terminal)
                    },
                    BatchSize::LargeInput,
                );
            });
        }
        group.finish();
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
        group.bench_function("drain_128_row_backlog/120x40", |bencher| {
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

    pub(super) fn terminal_output_benchmark(criterion: &mut Criterion) {
        fn draw_catch_up_frame(
            app: &mut App,
            terminal: &mut OutputTerminal,
            output_bytes: &Cell<u64>,
        ) -> DrawMetrics {
            let bytes_before = output_bytes.get();
            terminal.backend_mut().changed_cells = 0;
            app.advance_smooth_scroll();
            terminal
                .draw(|frame| view::render(frame, app))
                .expect("catch-up output benchmark frame should render");
            DrawMetrics {
                changed_cells: terminal.backend().changed_cells,
                output_bytes: output_bytes.get().saturating_sub(bytes_before),
            }
        }

        let (mut sample_app, mut sample_terminal, sample_bytes) = output_backlog_setup();
        let sample =
            draw_catch_up_frame(&mut sample_app, &mut sample_terminal, sample_bytes.as_ref());
        let mut group = criterion.benchmark_group("tui_terminal_output");
        group.sample_size(20);
        group.throughput(Throughput::Bytes(sample.output_bytes));
        group.bench_function(
            format!(
                "catch_up_frame_{}cells_{}bytes/120x40",
                sample.changed_cells, sample.output_bytes
            ),
            |bencher| {
                bencher.iter_batched(
                    output_backlog_setup,
                    |(mut app, mut terminal, output_bytes)| {
                        let metrics =
                            draw_catch_up_frame(&mut app, &mut terminal, output_bytes.as_ref());
                        black_box(metrics);
                        (app, terminal, output_bytes)
                    },
                    BatchSize::LargeInput,
                );
            },
        );

        let (mut app, mut terminal, output_bytes) = fast_mode_toggle_setup();
        let sample = draw_fast_mode_toggle(&mut app, &mut terminal, output_bytes.as_ref());
        assert!(
            sample.changed_cells <= 48,
            "footer changed {} cells",
            sample.changed_cells
        );
        assert!(
            sample.output_bytes <= 256,
            "footer wrote {} bytes",
            sample.output_bytes
        );
        group.bench_function(
            format!(
                "fast_mode_toggle_{}cells_{}bytes/120x40",
                sample.changed_cells, sample.output_bytes
            ),
            |bencher| {
                bencher.iter_batched(
                    fast_mode_toggle_setup,
                    |(mut app, mut terminal, output_bytes)| {
                        black_box(draw_fast_mode_toggle(
                            &mut app,
                            &mut terminal,
                            output_bytes.as_ref(),
                        ));
                    },
                    BatchSize::LargeInput,
                );
            },
        );
        group.finish();
    }

    pub(super) fn mouse_selection_benchmark(criterion: &mut Criterion) {
        let mut app = trace_app_with_tail(TRACE_SHAPES[0], 100 * 1_024);
        let mut terminal = Terminal::new(TestBackend::new(120, 40))
            .expect("mouse-selection benchmark terminal should initialize");
        terminal
            .draw(|frame| view::render(frame, &mut app))
            .expect("initial mouse-selection frame should render");
        assert!(app.begin_mouse_selection((1, 2).into()));
        let alternate = Cell::new(false);

        criterion.bench_function("tui_mouse_selection/drag_visible_range/120x40", |bencher| {
            bencher.iter(|| {
                let next = !alternate.get();
                alternate.set(next);
                assert!(app.drag_mouse_selection((118 - u16::from(next), 30).into()));
                terminal
                    .draw(|frame| view::render(frame, &mut app))
                    .expect("mouse-selection benchmark frame should render");
            });
        });

        criterion.bench_function(
            "tui_mouse_selection/edge_auto_scroll_tick/120x40",
            |bencher| {
                bencher.iter_batched(
                    || {
                        let mut app = App::new("/workspace/nanocodex".into());
                        for index in 0..80 {
                            app.main
                                .transcript
                                .push(TranscriptItem::User(sized_text(160, index)));
                        }
                        let mut terminal = Terminal::new(TestBackend::new(120, 40))
                            .expect("edge-selection benchmark terminal should initialize");
                        terminal
                            .draw(|frame| view::render(frame, &mut app))
                            .expect("initial edge-selection frame should render");
                        app.main.scroll_from_bottom = 80;
                        terminal
                            .draw(|frame| view::render(frame, &mut app))
                            .expect("scrolled edge-selection frame should render");
                        assert!(app.begin_mouse_selection((2, 10).into()));
                        assert!(app.drag_mouse_selection((118, 34).into()));
                        terminal
                            .draw(|frame| view::render(frame, &mut app))
                            .expect("edge drag frame should render");
                        (app, terminal)
                    },
                    |(mut app, mut terminal)| {
                        app.on_tick();
                        terminal
                            .draw(|frame| view::render(frame, &mut app))
                            .expect("auto-scroll selection frame should render");
                        black_box((app, terminal));
                    },
                    BatchSize::SmallInput,
                );
            },
        );
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
            let item_count = shape.user_messages
                + shape.assistant_messages
                + shape.reasoning_messages
                + shape.tool_calls;
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

    pub(super) fn large_paste_benchmarks(criterion: &mut Criterion) {
        let pasted = sized_text(100 * 1_024, 5);
        let mut group = criterion.benchmark_group("tui_large_paste");
        group.bench_function("ingest_100k", |bencher| {
            bencher.iter_batched(
                || App::new("/workspace/nanocodex".into()),
                |mut app| {
                    app.handle_paste(black_box(&pasted));
                    black_box(app);
                },
                BatchSize::LargeInput,
            );
        });
        group.bench_function("placeholder_frame_100k/120x40", |bencher| {
            let mut app = App::new("/workspace/nanocodex".into());
            app.handle_paste(&pasted);
            let mut terminal = Terminal::new(TestBackend::new(120, 40))
                .expect("large-paste benchmark terminal should initialize");
            bencher.iter(|| {
                terminal
                    .draw(|frame| view::render(frame, &mut app))
                    .expect("large-paste benchmark frame should render");
            });
        });
        group.bench_function("expand_100k", |bencher| {
            bencher.iter_batched(
                || {
                    let mut app = App::new("/workspace/nanocodex".into());
                    app.handle_paste(&pasted);
                    app
                },
                |mut app| black_box(app.take_submission()),
                BatchSize::LargeInput,
            );
        });
        group.finish();
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

    pub(super) fn markdown_benchmarks(criterion: &mut Criterion) {
        let mut markdown = String::new();
        for index in 0_usize..40 {
            write!(
                markdown,
                "## Result {index}\n\nA **formatted** result with `inline code`.\n\n| Name | Status | Detail |\n| --- | --- | --- |\n| build-{index} | passed | deterministic output |\n\n"
            )
            .expect("writing benchmark Markdown to a string cannot fail");
            if index.is_multiple_of(4) {
                write!(
                    markdown,
                    "```rust\nfn result_{index}() -> usize {{ {index} }}\n```\n\n"
                )
                .expect("writing benchmark code to a string cannot fail");
            }
        }
        criterion.bench_function("tui_markdown/finalize_and_first_frame/120x40", |bencher| {
            bencher.iter_batched(
                || {
                    let mut transcript = Transcript::default();
                    transcript.push(TranscriptItem::Assistant(markdown.clone()));
                    let terminal = Terminal::new(TestBackend::new(120, 40))
                        .expect("markdown benchmark terminal should initialize");
                    (transcript, terminal)
                },
                |(mut transcript, mut terminal)| {
                    assert!(transcript.finalize_assistant(black_box(&markdown)));
                    terminal
                        .draw(|frame| {
                            frame.render_widget(
                                transcript.widget(0, None, None, "empty"),
                                frame.area(),
                            );
                        })
                        .expect("markdown benchmark frame should render");
                    black_box((transcript, terminal));
                },
                BatchSize::SmallInput,
            );
        });

        criterion.bench_function("tui_markdown/healed_streaming_frame/120x40", |bencher| {
            bencher.iter_batched(
                || {
                    let mut transcript = Transcript::default();
                    transcript.push(TranscriptItem::Assistant(format!(
                        "{markdown}\nStreaming **formatted tail"
                    )));
                    let terminal = Terminal::new(TestBackend::new(120, 40))
                        .expect("streaming Markdown benchmark terminal should initialize");
                    (transcript, terminal)
                },
                |(mut transcript, mut terminal)| {
                    assert!(transcript.append_assistant_delta(" with `code"));
                    terminal
                        .draw(|frame| {
                            frame.render_widget(
                                transcript.widget(0, None, None, "empty"),
                                frame.area(),
                            );
                        })
                        .expect("streaming Markdown benchmark frame should render");
                    black_box((transcript, terminal));
                },
                BatchSize::SmallInput,
            );
        });

        let mut linked = Transcript::default();
        let linked_source = (0..64)
            .map(|index| format!("Read [reference {index}](https://example.com/{index})."))
            .collect::<Vec<_>>()
            .join("\n");
        let linked_copy = (0..64)
            .map(|index| format!("Read reference {index}."))
            .collect::<Vec<_>>()
            .join("\n");
        linked.push(TranscriptItem::Assistant(linked_source.clone()));
        criterion.bench_function("tui_markdown/semantic_link_copy_64", |bencher| {
            bencher.iter(|| linked.semanticize_copy(black_box(linked_copy.clone())));
        });

        let image = format!(
            "before\n\n![deployment chart](data:image/png;base64,{})\n\nafter",
            "A".repeat(100_000)
        );
        criterion.bench_function("tui_markdown/image_placeholder_100k", |bencher| {
            bencher.iter(|| markdown::render_agent_markdown(black_box(&image), 120));
        });
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn tool_tree_benchmark(criterion: &mut Criterion) {
        criterion.bench_function("tui_tool_tree/update_and_frame/120x40", |bencher| {
            bencher.iter_batched(
                || {
                    let mut transcript = Transcript::default();
                    transcript.push(TranscriptItem::Tool {
                        call_id: "call-1".to_owned(),
                        name: "exec".to_owned(),
                        arguments: "const tasks = inputs.map(run);\nconst output = await Promise.all(tasks);\ntext(output);".to_owned(),
                        status: ToolStatus::Running,
                    });
                    for index in 0..16 {
                        assert!(transcript.push_tool_child(
                            format!("call-1/code-{index}"),
                            "exec_command".to_owned(),
                            format!("worker {index}"),
                            ToolStatus::Running,
                        ));
                    }
                    let terminal = Terminal::new(TestBackend::new(120, 40))
                        .expect("tool benchmark terminal should initialize");
                    (transcript, terminal)
                },
                |(mut transcript, mut terminal)| {
                    assert!(transcript.set_tool_result(
                        "call-1/code-15",
                        ToolStatus::Completed,
                        Some(80_000_000),
                        Some("exit 0".to_owned()),
                    ));
                    terminal
                        .draw(|frame| {
                            frame.render_widget(
                                transcript.widget(0, None, None, "empty"),
                                frame.area(),
                            );
                        })
                        .expect("tool benchmark frame should render");
                    black_box((transcript, terminal));
                },
                BatchSize::SmallInput,
            );
        });

        let mut patch = String::from("*** Begin Patch\n");
        for index in 0..16 {
            use std::fmt::Write as _;
            writeln!(patch, "*** Update File: src/module_{index}.rs").unwrap();
            patch.push_str("@@\n-old_value();\n+new_value();\n context();\n");
        }
        patch.push_str("*** End Patch");
        criterion.bench_function(
            "tui_tool_tree/patch_16_files_first_frame/120x40",
            |bencher| {
                bencher.iter_batched(
                    || {
                        let mut transcript = Transcript::default();
                        transcript.push(TranscriptItem::Tool {
                            call_id: "patch-1".to_owned(),
                            name: "apply_patch".to_owned(),
                            arguments: patch.clone(),
                            status: ToolStatus::Completed,
                        });
                        let terminal = Terminal::new(TestBackend::new(120, 40))
                            .expect("patch benchmark terminal should initialize");
                        (transcript, terminal)
                    },
                    |(transcript, mut terminal)| {
                        terminal
                            .draw(|frame| {
                                frame.render_widget(
                                    transcript.widget(0, None, None, "empty"),
                                    frame.area(),
                                );
                            })
                            .expect("patch benchmark frame should render");
                        black_box((transcript, terminal));
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        let large_result = sized_text(269 * 1_024, 17);
        criterion.bench_function("tui_tool_tree/result_269k_first_frame/120x40", |bencher| {
            bencher.iter_batched(
                || {
                    let mut transcript = Transcript::default();
                    transcript.push(TranscriptItem::Tool {
                        call_id: "result-1".to_owned(),
                        name: "exec_command".to_owned(),
                        arguments: "render report".to_owned(),
                        status: ToolStatus::Completed,
                    });
                    assert!(transcript.set_tool_result(
                        "result-1",
                        ToolStatus::Completed,
                        Some(1_000_000),
                        Some(large_result.clone()),
                    ));
                    let terminal = Terminal::new(TestBackend::new(120, 40))
                        .expect("large-result benchmark terminal should initialize");
                    (transcript, terminal)
                },
                |(transcript, mut terminal)| {
                    terminal
                        .draw(|frame| {
                            frame.render_widget(
                                transcript.widget(0, None, None, "empty"),
                                frame.area(),
                            );
                        })
                        .expect("large-result benchmark frame should render");
                    black_box((transcript, terminal));
                },
                BatchSize::SmallInput,
            );
        });

        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Tool {
            call_id: "result-1".to_owned(),
            name: "exec_command".to_owned(),
            arguments: "render report".to_owned(),
            status: ToolStatus::Completed,
        });
        assert!(transcript.set_tool_result(
            "result-1",
            ToolStatus::Completed,
            Some(1_000_000),
            Some(large_result),
        ));
        let mut terminal = Terminal::new(TestBackend::new(120, 40))
            .expect("cached large-result benchmark terminal should initialize");
        terminal
            .draw(|frame| {
                frame.render_widget(transcript.widget(0, None, None, "empty"), frame.area());
            })
            .expect("initial large-result benchmark frame should render");
        criterion.bench_function("tui_tool_tree/result_269k_cached_frame/120x40", |bencher| {
            bencher.iter(|| {
                terminal
                    .draw(|frame| {
                        frame
                            .render_widget(transcript.widget(0, None, None, "empty"), frame.area());
                    })
                    .expect("cached large-result benchmark frame should render");
            });
        });

        let nested_result = sized_text(269 * 1_024, 19);
        let nested_transcript = || {
            let mut transcript = Transcript::default();
            transcript.push(TranscriptItem::Tool {
                call_id: "code-mode-1".to_owned(),
                name: "exec".to_owned(),
                arguments: "text(await tools.exec_command({ cmd: 'render report' }));".to_owned(),
                status: ToolStatus::Completed,
            });
            assert!(transcript.push_tool_child(
                "code-mode-1/code-1".to_owned(),
                "exec_command".to_owned(),
                "render report".to_owned(),
                ToolStatus::Completed,
            ));
            assert!(transcript.set_tool_result(
                "code-mode-1/code-1",
                ToolStatus::Completed,
                Some(1_000_000),
                Some(nested_result.clone()),
            ));
            transcript
        };
        criterion.bench_function(
            "tui_tool_tree/nested_result_269k_first_frame/120x40",
            |bencher| {
                bencher.iter_batched(
                    || {
                        let transcript = nested_transcript();
                        let terminal = Terminal::new(TestBackend::new(120, 40))
                            .expect("nested large-result benchmark terminal should initialize");
                        (transcript, terminal)
                    },
                    |(transcript, mut terminal)| {
                        terminal
                            .draw(|frame| {
                                frame.render_widget(
                                    transcript.widget(0, None, None, "empty"),
                                    frame.area(),
                                );
                            })
                            .expect("nested large-result first frame should render");
                        black_box((transcript, terminal));
                    },
                    BatchSize::LargeInput,
                );
            },
        );

        let nested_transcript = nested_transcript();
        let mut nested_terminal = Terminal::new(TestBackend::new(120, 40))
            .expect("cached nested-result benchmark terminal should initialize");
        nested_terminal
            .draw(|frame| {
                frame.render_widget(
                    nested_transcript.widget(0, None, None, "empty"),
                    frame.area(),
                );
            })
            .expect("initial nested large-result benchmark frame should render");
        criterion.bench_function(
            "tui_tool_tree/nested_result_269k_cached_frame/120x40",
            |bencher| {
                bencher.iter(|| {
                    nested_terminal
                        .draw(|frame| {
                            frame.render_widget(
                                nested_transcript.widget(0, None, None, "empty"),
                                frame.area(),
                            );
                        })
                        .expect("cached nested large-result frame should render");
                });
            },
        );
    }

    fn live_code_mode_tree() -> Transcript {
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Tool {
            call_id: "code-mode-live".to_owned(),
            name: "exec".to_owned(),
            arguments: "const results = await Promise.all(tasks);".to_owned(),
            status: ToolStatus::Running,
        });
        for index in 0..16 {
            assert!(transcript.push_tool_child(
                format!("code-mode-live/code-{index}"),
                "exec_command".to_owned(),
                format!("cargo test -p package-{index}"),
                ToolStatus::Running,
            ));
        }
        transcript
    }

    const PROMISE_RESOLUTION_ORDER: [usize; 16] =
        [7, 2, 14, 0, 11, 5, 9, 1, 15, 4, 12, 6, 10, 3, 13, 8];

    fn resolve_promises(transcript: &mut Transcript) {
        for (resolved, index) in PROMISE_RESOLUTION_ORDER.into_iter().enumerate() {
            assert!(transcript.set_tool_result_timing(
                &format!("code-mode-live/code-{index}"),
                ToolStatus::Completed,
                Some(u64::try_from(index).unwrap_or(u64::MAX) * 100_000),
                Some(u64::try_from(resolved + 1).unwrap_or(u64::MAX) * 1_000_000),
                Some("exit 0".to_owned()),
            ));
        }
    }

    fn live_code_mode_output_setup() -> (Transcript, OutputTerminal, Rc<Cell<u64>>) {
        let transcript = live_code_mode_tree();
        let (mut terminal, output_bytes) = output_terminal();
        terminal
            .draw(|frame| {
                frame.render_widget(transcript.widget(0, None, None, "empty"), frame.area());
            })
            .expect("initial live Code Mode frame should render");
        (transcript, terminal, output_bytes)
    }

    fn resolve_promises_with_frames(
        transcript: &mut Transcript,
        terminal: &mut OutputTerminal,
        output_bytes: &Cell<u64>,
    ) -> DrawMetrics {
        let mut totals = DrawMetrics::default();
        for (resolved, index) in PROMISE_RESOLUTION_ORDER.into_iter().enumerate() {
            assert!(transcript.set_tool_result_timing(
                &format!("code-mode-live/code-{index}"),
                ToolStatus::Completed,
                Some(u64::try_from(index).unwrap_or(u64::MAX) * 100_000),
                Some(u64::try_from(resolved + 1).unwrap_or(u64::MAX) * 1_000_000),
                Some("exit 0".to_owned()),
            ));
            let bytes_before = output_bytes.get();
            terminal.backend_mut().changed_cells = 0;
            terminal
                .draw(|frame| {
                    frame.render_widget(transcript.widget(0, None, None, "empty"), frame.area());
                })
                .expect("live Code Mode completion frame should render");
            totals.changed_cells = totals
                .changed_cells
                .saturating_add(terminal.backend().changed_cells);
            totals.output_bytes = totals
                .output_bytes
                .saturating_add(output_bytes.get().saturating_sub(bytes_before));
        }
        totals
    }

    pub(super) fn code_mode_streaming_benchmark(criterion: &mut Criterion) {
        const COMPLETIONS: u64 = 16;
        let mut group = criterion.benchmark_group("tui_code_mode_stream");
        group.throughput(Throughput::Elements(COMPLETIONS));
        group.bench_function("apply_16_out_of_order_completions", |bencher| {
            bencher.iter_batched(
                live_code_mode_tree,
                |mut transcript| {
                    resolve_promises(&mut transcript);
                    black_box(transcript);
                },
                BatchSize::SmallInput,
            );
        });

        let (mut sample_transcript, mut sample_terminal, sample_bytes) =
            live_code_mode_output_setup();
        let sample = resolve_promises_with_frames(
            &mut sample_transcript,
            &mut sample_terminal,
            sample_bytes.as_ref(),
        );
        assert!(
            sample.changed_cells > 0,
            "completion frames must be visible"
        );
        assert!(
            sample.changed_cells <= 1_600,
            "16 completions changed too many terminal cells: {}",
            sample.changed_cells
        );
        assert!(
            sample.output_bytes <= 3_500,
            "16 completions emitted too many terminal bytes: {}",
            sample.output_bytes
        );
        group.throughput(Throughput::Bytes(sample.output_bytes));
        group.bench_function(
            format!(
                "resolve_16_frames_{}cells_{}bytes/120x40",
                sample.changed_cells, sample.output_bytes
            ),
            |bencher| {
                bencher.iter_batched(
                    live_code_mode_output_setup,
                    |(mut transcript, mut terminal, output_bytes)| {
                        let metrics = resolve_promises_with_frames(
                            &mut transcript,
                            &mut terminal,
                            output_bytes.as_ref(),
                        );
                        black_box(metrics);
                    },
                    BatchSize::LargeInput,
                );
            },
        );
        group.finish();
    }

    pub(super) fn folded_tool_benchmarks(criterion: &mut Criterion) {
        criterion.bench_function("tui_tool_tree/fold_3471_activities", |bencher| {
            bencher.iter_batched_ref(
                || {
                    let mut transcript = Transcript::default();
                    for index in 0..3_471 {
                        transcript.push(TranscriptItem::Tool {
                            call_id: format!("call-{index}"),
                            name: "exec_command".to_owned(),
                            arguments: format!("cargo test package-{index}"),
                            status: ToolStatus::Completed,
                        });
                    }
                    let shared = transcript.clone();
                    (transcript, shared)
                },
                |(transcript, shared)| {
                    transcript.set_tool_details_expanded(false);
                    black_box((transcript.len(), shared.len()));
                },
                BatchSize::LargeInput,
            );
        });

        criterion.bench_function("tui_tool_tree/folded_tail_frame/120x40", |bencher| {
            let mut transcript = Transcript::default();
            for index in 0..3_471 {
                transcript.push(TranscriptItem::Tool {
                    call_id: format!("call-{index}"),
                    name: "exec_command".to_owned(),
                    arguments: format!("cargo test package-{index}"),
                    status: ToolStatus::Completed,
                });
            }
            transcript.set_tool_details_expanded(false);
            let mut terminal = Terminal::new(TestBackend::new(120, 40))
                .expect("folded tool terminal should initialize");
            bencher.iter(|| {
                terminal
                    .draw(|frame| {
                        frame
                            .render_widget(transcript.widget(0, None, None, "empty"), frame.area());
                    })
                    .expect("folded tool frame should render");
            });
        });
    }
}

criterion_group!(
    benches,
    tui::render_benchmarks,
    tui::resize_benchmarks,
    tui::transcript_update_benchmark,
    tui::live_tail_render_benchmark,
    tui::live_tail_first_frame_benchmark,
    tui::scroll_anchor_benchmark,
    tui::smooth_follow_benchmark,
    tui::terminal_output_benchmark,
    tui::mouse_selection_benchmark,
    tui::stream_telemetry_benchmark,
    tui::first_frame_benchmarks,
    tui::composer_benchmarks,
    tui::large_paste_benchmarks,
    tui::history_navigation_benchmarks,
    tui::branch_state_benchmarks,
    tui::markdown_benchmarks,
    tui::tool_tree_benchmark,
    tui::code_mode_streaming_benchmark,
    tui::folded_tool_benchmarks
);
criterion_main!(benches);
