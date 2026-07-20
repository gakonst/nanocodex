use criterion::{criterion_group, criterion_main};

mod tui {
    use std::hint::black_box;

    use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
    use ratatui::{Terminal, backend::TestBackend};

    #[allow(dead_code, unused_imports)]
    mod transcript {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/tui/transcript.rs"
        ));
    }

    #[allow(dead_code, unused_imports)]
    mod app {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tui/app.rs"));
    }

    #[allow(dead_code, unused_imports)]
    mod view {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tui/view.rs"));
    }

    use app::App;
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
                app.main.transcript.push(TranscriptItem::User(sized_text(
                    distribute(shape.user_chars, shape.user_messages, index),
                    index,
                )));
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
            for (width, height) in TERMINAL_SIZES {
                let mut app = trace_app(shape);
                let mut terminal = Terminal::new(TestBackend::new(width, height))
                    .expect("trace benchmark terminal should initialize");
                terminal
                    .draw(|frame| view::render(frame, &app))
                    .expect("initial trace benchmark frame should render");

                group.bench_with_input(
                    BenchmarkId::new(shape.name, format!("{width}x{height}")),
                    &(width, height),
                    |bencher, _| {
                        bencher.iter(|| {
                            // Invalidate the streaming tail's wrapped-height cache without
                            // growing the fixture across Criterion iterations.
                            assert!(app.main.transcript.append_assistant_delta(""));
                            terminal
                                .draw(|frame| view::render(frame, &app))
                                .expect("trace benchmark frame should render");
                        });
                    },
                );
            }
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
                    |(app, mut terminal)| {
                        terminal
                            .draw(|frame| view::render(frame, &app))
                            .expect("first trace benchmark frame should render");
                    },
                    BatchSize::LargeInput,
                );
            });
        }
        group.finish();
    }
}

criterion_group!(
    benches,
    tui::render_benchmarks,
    tui::transcript_update_benchmark,
    tui::first_frame_benchmarks
);
criterion_main!(benches);
