// SPDX-License-Identifier: Apache-2.0

//! Synthetic Criterion coverage for retained-history TUI hot paths.

#![allow(dead_code, unused_imports)]

#[path = "components/mod.rs"]
mod components;
#[path = "../config.rs"]
mod config;
mod context;
mod format;
mod history;
#[path = "../installation.rs"]
mod installation;
mod pane;
mod prompt;
mod session;
#[path = "../skill.rs"]
mod skill;
mod spinner;
mod theme;
#[path = "transcript/mod.rs"]
mod transcript;

// Keep production components on their normal module paths in this private target.
mod tui {
    pub(crate) use crate::{context, format, pane, prompt, session, spinner, theme, transcript};
}

use components::{AppEffect, AppEvent, AppNode, RenderRequest, RootEffect, RootNode};
use config::ReasoningEffort;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use history::{
    HistoryPrefetch, HistoryWindow, history_projection, history_projection_with_sequences,
    older_history_projection_with_sequences,
};
use nanocodex_agent::events::{AgentEvent, AgentEventKind};
use nanocodex_managed::{EventHistoryPage, ManagedEvent, ManagedEventData, PromptInput};
use pane::PaneId;
use ratatui::{Terminal, backend::TestBackend};
use serde_json::{json, value::to_raw_value};
use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    hint::black_box,
    path::Path,
    sync::Arc,
};
use theme::Theme;
use transcript::TranscriptRecord;

const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;
const HISTORY_SIZES: [usize; 3] = [256, 4_096, 16_384];
const OLDER_PAGE_SIZE: usize = 256;
const EVENTS_PER_TURN: usize = 8;
const AGENT_ID: &str = "benchmark-agent";
const WORKSPACE: &str = "/benchmark-workspace";

struct Harness {
    app: AppNode,
    terminal: Terminal<TestBackend>,
}

impl Harness {
    fn from_records(records: Vec<Arc<TranscriptRecord>>, width: u16, height: u16) -> Self {
        let workspace = Path::new(WORKSPACE);
        let root = RootNode::new(workspace, ReasoningEffort::Medium);
        let mut app = AppNode::new(Theme::default(), workspace.to_path_buf(), root);
        let projection = RootNode::project_open_session(ReasoningEffort::Medium, records);
        let update = app.update(AppEvent::HistoryReplayed {
            pane: PaneId::Main,
            projection: Box::new(projection),
        });
        assert_eq!(update.render, RenderRequest::Immediate);
        assert!(update.effects.is_empty());
        Self {
            app,
            terminal: Terminal::new(TestBackend::new(width, height))
                .expect("benchmark terminal should initialize"),
        }
    }

    fn render(&mut self) {
        self.terminal
            .draw(|frame| self.app.render(frame))
            .expect("benchmark frame should render");
    }

    fn render_fingerprint(&mut self) -> u64 {
        self.render();
        self.fingerprint()
    }

    fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        for cell in self.terminal.backend().buffer().content() {
            cell.symbol().hash(&mut hasher);
        }
        hasher.finish()
    }

    fn screen_text(&self) -> String {
        self.terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn key_update(&mut self, code: KeyCode, modifiers: KeyModifiers) -> (usize, RenderRequest) {
        let update = self.app.update(AppEvent::Terminal(Event::Key(KeyEvent::new(
            code, modifiers,
        ))));
        let loads = update
            .effects
            .iter()
            .filter(|effect| {
                matches!(
                    effect,
                    AppEffect::Pane {
                        pane: PaneId::Main,
                        effect: RootEffect::LoadOlderHistory
                    }
                )
            })
            .count();
        (loads, update.render)
    }

    fn page_up(&mut self) -> usize {
        self.key_update(KeyCode::PageUp, KeyModifiers::NONE).0
    }

    fn toggle_all_tools(&mut self) -> bool {
        let (loads, render) = self.key_update(KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert_eq!(loads, 0);
        render == RenderRequest::Immediate
    }
}

#[derive(Clone)]
struct PrependCase {
    loaded: Vec<ManagedEvent>,
    older: Vec<ManagedEvent>,
}

struct PrependRun {
    window: HistoryWindow,
    older_page: EventHistoryPage,
    sequences: HashMap<String, u64>,
    next_sequence: u64,
    records: Vec<Arc<TranscriptRecord>>,
    stable_cursor: String,
    stable_sequence: u64,
    harness: Harness,
}

struct ReplayOutcome {
    event_count: usize,
    record_count: usize,
    before: Option<String>,
    stable_sequence_preserved: bool,
    render_requested: bool,
}

impl PrependRun {
    fn setup(case: &PrependCase) -> Self {
        let mut sequences = HashMap::new();
        let mut next_sequence = 1;
        let (records, _) = history_projection_with_sequences(
            &case.loaded,
            AGENT_ID,
            Path::new(WORKSPACE),
            &mut sequences,
            &mut next_sequence,
        )
        .expect("loaded history should project");
        assert_eq!(records.len(), case.loaded.len());

        let stable_cursor = case
            .loaded
            .first()
            .expect("loaded history should be nonempty")
            .cursor
            .clone();
        let stable_sequence = sequences[&stable_cursor];
        let mut harness = Harness::from_records(records.clone(), WIDTH, HEIGHT);
        harness.render();
        assert_eq!(harness.page_up(), 0);
        harness.render();

        Self {
            window: HistoryWindow {
                events: case.loaded.clone(),
                before: Some(stable_cursor.clone()),
                has_more: true,
            },
            older_page: EventHistoryPage {
                data: case.older.clone(),
                has_more: false,
                latest_cursor: case
                    .loaded
                    .last()
                    .expect("loaded history should be nonempty")
                    .cursor
                    .clone(),
            },
            sequences,
            next_sequence,
            records,
            stable_cursor,
            stable_sequence,
            harness,
        }
    }

    fn complete(mut self) -> (ReplayOutcome, Harness) {
        let coherent_tail = self
            .window
            .events
            .iter()
            .any(|event| matches!(event.data, ManagedEventData::TurnAccepted { .. }));
        let older_len = self.older_page.data.len();
        self.window
            .prepend(self.older_page)
            .expect("older page should prepend");
        let (mut older_records, _) = older_history_projection_with_sequences(
            &self.window.events[..older_len],
            coherent_tail,
            AGENT_ID,
            Path::new(WORKSPACE),
            &mut self.sequences,
            &mut self.next_sequence,
        )
        .expect("prepended history should reproject");
        older_records.append(&mut self.records);
        let records = older_records;
        let record_count = records.len();
        let projection = RootNode::project_open_session(ReasoningEffort::Medium, records);
        let update = self.harness.app.update(AppEvent::HistoryReplayed {
            pane: PaneId::Main,
            projection: Box::new(projection),
        });
        self.harness.render();
        (
            ReplayOutcome {
                event_count: self.window.events.len(),
                record_count,
                before: self.window.before,
                stable_sequence_preserved: self.sequences[&self.stable_cursor]
                    == self.stable_sequence,
                render_requested: update.render == RenderRequest::Immediate,
            },
            self.harness,
        )
    }
}

fn synthetic_history(event_count: usize, tool_output_bytes: usize) -> Vec<ManagedEvent> {
    synthetic_history_range(1, event_count, tool_output_bytes)
}

fn synthetic_user_prompt_history(prompt_bytes: usize) -> Vec<ManagedEvent> {
    let mut history = synthetic_history(4_096, 128);
    let Some(ManagedEvent {
        data: ManagedEventData::TurnAccepted { input, .. },
        ..
    }) = history.first_mut()
    else {
        panic!("synthetic turn should start with an accepted user prompt");
    };
    *input = PromptInput::Text(deterministic_user_prompt(prompt_bytes));
    history
}

fn synthetic_history_range(
    first_cursor: usize,
    event_count: usize,
    tool_output_bytes: usize,
) -> Vec<ManagedEvent> {
    assert_eq!(event_count % EVENTS_PER_TURN, 0);
    (first_cursor..first_cursor.saturating_add(event_count))
        .map(|cursor| synthetic_event(cursor, tool_output_bytes))
        .collect()
}

fn synthetic_event(cursor: usize, tool_output_bytes: usize) -> ManagedEvent {
    let turn = (cursor - 1) / EVENTS_PER_TURN;
    let phase = (cursor - 1) % EVENTS_PER_TURN;
    let turn_id = format!("turn-{turn:05}");
    let data = match phase {
        0 => ManagedEventData::TurnAccepted {
            id: turn_id.clone(),
            input: PromptInput::Text(format!(
                "inspect deterministic subsystem {turn:05} and report its behavior"
            )),
            replayed: false,
        },
        1 => agent_data(cursor, turn, AgentEventKind::RunStarted, json!({})),
        2 | 3 => agent_data(
            cursor,
            turn,
            AgentEventKind::AssistantDelta,
            json!({
                "model_call_index": turn,
                "item_id": format!("message-{turn:05}"),
                "phase": "final_answer",
                "text": format!("deterministic response chunk {phase} for turn {turn:05}. "),
            }),
        ),
        4 => agent_data(
            cursor,
            turn,
            AgentEventKind::ToolCall,
            json!({
                "call_id": format!("tool-{turn:05}"),
                "tool": "exec_command",
                "arguments": {"cmd": format!("cargo check -p fixture-{turn:05}")},
            }),
        ),
        5 => {
            let output = deterministic_output(turn, tool_output_bytes);
            agent_data(
                cursor,
                turn,
                AgentEventKind::ToolResult,
                json!({
                    "call_id": format!("tool-{turn:05}"),
                    "tool": "exec_command",
                    "status": "completed",
                    "duration_ns": 1_000_000_u64,
                    "result": format!(
                        "Wall time: 0.001 seconds\nProcess exited with code 0\nFinal output:\n{output}"
                    ),
                    "structured_result": {
                        "output": output,
                        "exit_code": 0,
                        "wall_time_seconds": 0.001,
                    },
                    "metadata": null,
                }),
            )
        }
        6 => agent_data(
            cursor,
            turn,
            AgentEventKind::AssistantMessage,
            json!({
                "model_call_index": turn,
                "item_id": format!("message-{turn:05}"),
                "phase": "final_answer",
                "text": format!("completed deterministic response for turn {turn:05}"),
            }),
        ),
        7 => agent_data(
            cursor,
            turn,
            AgentEventKind::RunCompleted,
            json!({"duration_ns": 8_000_000_u64}),
        ),
        _ => unreachable!(),
    };
    ManagedEvent {
        cursor: cursor.to_string(),
        created_at: Some(1_750_000_000.0 + cursor as f64 / 1_000.0),
        turn_id: Some(turn_id),
        data,
    }
}

fn agent_data(
    cursor: usize,
    turn: usize,
    kind: AgentEventKind,
    payload: serde_json::Value,
) -> ManagedEventData {
    let event = AgentEvent {
        protocol_version: 1,
        request_id: Arc::from(format!("benchmark-request-{turn:05}")),
        seq: u64::try_from(cursor).expect("benchmark cursor should fit u64"),
        kind,
        payload: to_raw_value(&payload)
            .expect("benchmark payload should serialize")
            .into(),
    };
    ManagedEventData::Event {
        event: to_raw_value(&event).expect("benchmark event should serialize"),
        agent_id: None,
    }
}

fn deterministic_output(turn: usize, bytes: usize) -> String {
    let line = format!("payload-line-{turn:05}: synthetic benchmark output\n");
    deterministic_payload(bytes, &line)
}

fn deterministic_user_prompt(bytes: usize) -> String {
    const LINE: &str = "user-prompt-payload: inspect deterministic benchmark behavior\n";
    deterministic_payload(bytes, LINE)
}

fn deterministic_payload(bytes: usize, line: &str) -> String {
    let mut payload = String::with_capacity(bytes);
    while payload.len() < bytes {
        payload.push_str(line);
    }
    payload.truncate(bytes);
    payload
}

fn validated_projection(history: &[ManagedEvent]) -> Vec<Arc<TranscriptRecord>> {
    let (records, _, recent) = history_projection(history.to_vec(), AGENT_ID, Path::new(WORKSPACE))
        .expect("synthetic history should project");
    assert_eq!(records.len(), history.len());
    assert_eq!(recent.len(), history.len() / EVENTS_PER_TURN);
    assert_eq!(
        records.first().map(|record| record.kind()),
        Some("user.submitted")
    );
    assert_eq!(
        records.last().map(|record| record.kind()),
        Some("run.completed")
    );
    records
}

fn projection_benchmarks(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("nanocodex2_history/projection");
    group.sample_size(10);
    for event_count in HISTORY_SIZES {
        let history = synthetic_history(event_count, 128);
        let records = validated_projection(&history);
        assert_eq!(records.len(), event_count);
        group.throughput(Throughput::Elements(event_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(event_count),
            &history,
            |bencher, history| {
                bencher.iter_batched(
                    || history.clone(),
                    |history| {
                        let (records, _, recent) =
                            history_projection(history, AGENT_ID, Path::new(WORKSPACE))
                                .expect("synthetic history should project during benchmark");
                        black_box((records.len(), recent.len()))
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

fn replay_and_frame_benchmarks(criterion: &mut Criterion) {
    let mut replay = criterion.benchmark_group("nanocodex2_history/model_replay");
    replay.sample_size(10);
    for event_count in HISTORY_SIZES {
        let history = synthetic_history(event_count, 128);
        let records = validated_projection(&history);
        black_box(RootNode::project_open_session(
            ReasoningEffort::Medium,
            records.clone(),
        ));
        replay.throughput(Throughput::Elements(event_count as u64));
        replay.bench_with_input(
            BenchmarkId::from_parameter(event_count),
            &records,
            |bencher, records| {
                bencher.iter_batched(
                    || records.clone(),
                    |records| {
                        black_box(RootNode::project_open_session(
                            ReasoningEffort::Medium,
                            records,
                        ))
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    replay.finish();

    let mut frames = criterion.benchmark_group("nanocodex2_history/frame_120x40");
    frames.sample_size(10);
    for event_count in HISTORY_SIZES {
        let history = synthetic_history(event_count, 128);
        let records = validated_projection(&history);
        let mut first = Harness::from_records(records.clone(), WIDTH, HEIGHT);
        assert_ne!(first.render_fingerprint(), 0);

        frames.throughput(Throughput::Elements(event_count as u64));
        frames.bench_with_input(
            BenchmarkId::new("first", event_count),
            &records,
            |bencher, records| {
                bencher.iter_batched(
                    || Harness::from_records(records.clone(), WIDTH, HEIGHT),
                    |mut harness| {
                        harness.render();
                        black_box(&harness);
                    },
                    BatchSize::LargeInput,
                );
            },
        );

        let mut cached = Harness::from_records(records, WIDTH, HEIGHT);
        let cached_fingerprint = cached.render_fingerprint();
        assert_eq!(cached.render_fingerprint(), cached_fingerprint);
        frames.bench_function(BenchmarkId::new("cached", event_count), |bencher| {
            bencher.iter(|| {
                cached.render();
                black_box(&cached);
            });
        });
    }
    frames.finish();
}

fn prepend_replay_benchmarks(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("nanocodex2_history/prepend_reproject_replay");
    group.sample_size(10);
    for loaded_count in HISTORY_SIZES {
        let case = PrependCase {
            loaded: synthetic_history_range(OLDER_PAGE_SIZE + 1, loaded_count, 128),
            older: synthetic_history(OLDER_PAGE_SIZE, 128),
        };
        let expected_count = loaded_count + OLDER_PAGE_SIZE;
        let run = PrependRun::setup(&case);
        let before_fingerprint = run.harness.fingerprint();
        let (outcome, harness) = run.complete();
        assert_eq!(outcome.event_count, expected_count);
        assert_eq!(outcome.record_count, expected_count);
        assert_eq!(outcome.before.as_deref(), Some("1"));
        assert!(outcome.stable_sequence_preserved);
        assert_eq!(harness.fingerprint(), before_fingerprint);
        assert!(outcome.render_requested);

        group.throughput(Throughput::Elements(expected_count as u64));
        group.bench_with_input(
            BenchmarkId::new("page_256", loaded_count),
            &case,
            |bencher, case| {
                bencher.iter_batched(
                    || PrependRun::setup(case),
                    // Return the complete replay so Criterion drops the large
                    // model after the timed routine rather than charging its
                    // teardown to page completion.
                    |run| black_box(run.complete()),
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

fn proactive_prefetch_orchestration_benchmarks(criterion: &mut Criterion) {
    const PAGE_COUNT: usize = 4_096;
    let mut group = criterion.benchmark_group("nanocodex2_history/proactive_prefetch");
    group.throughput(Throughput::Elements(PAGE_COUNT as u64));
    group.bench_function("sequential_cursor_chain_4096_pages", |bencher| {
        bencher.iter(|| {
            let mut history = HistoryWindow::retry_from(PAGE_COUNT.to_string());
            let mut prefetch = HistoryPrefetch::default();
            for next_before in (0..PAGE_COUNT).rev() {
                let requested = prefetch
                    .claim(&history)
                    .expect("synthetic prefetch should remain available");
                black_box(prefetch.claim(&history));
                prefetch
                    .store(&requested, prefetch_page(next_before, next_before != 0))
                    .expect("synthetic page should buffer");
                prefetch.request_replay();
                black_box(
                    prefetch
                        .take_requested(history.before.as_deref())
                        .expect("synthetic page should replay from the buffer"),
                );
                history.before = Some(next_before.to_string());
                history.has_more = next_before != 0;
            }
            black_box(prefetch.claim(&history))
        });
    });
    group.finish();
}

fn prefetch_page(cursor: usize, has_more: bool) -> EventHistoryPage {
    EventHistoryPage {
        data: vec![ManagedEvent {
            cursor: cursor.to_string(),
            created_at: None,
            turn_id: None,
            data: ManagedEventData::AgentCreated {
                agent_id: "synthetic-agent".to_owned(),
                capabilities: json!({}),
            },
        }],
        has_more,
        latest_cursor: "synthetic-latest".to_owned(),
    }
}

fn near_top_harness(records: &[Arc<TranscriptRecord>]) -> Harness {
    let mut harness = Harness::from_records(records.to_vec(), WIDTH, HEIGHT);
    harness.render();
    for _ in 0..1_024 {
        let loads = harness.page_up();
        harness.render();
        if loads == 1 {
            return harness;
        }
        assert_eq!(loads, 0);
    }
    panic!("synthetic transcript should reach the near-top lazy-load window");
}

fn prewarmed_harness(records: &[Arc<TranscriptRecord>]) -> Harness {
    let mut harness = Harness::from_records(records.to_vec(), WIDTH, HEIGHT);
    harness.render();
    harness
}

fn scroll_benchmarks(criterion: &mut Criterion) {
    let history = synthetic_history(4_096, 128);
    let records = validated_projection(&history);

    let mut far = Harness::from_records(records.clone(), WIDTH, HEIGHT);
    far.render();
    assert_eq!(far.page_up(), 0);
    let mut near = near_top_harness(&records);
    assert_eq!(near.page_up(), 1);
    let mut home = Harness::from_records(records.clone(), WIDTH, HEIGHT);
    home.render();
    assert_eq!(home.key_update(KeyCode::Home, KeyModifiers::CONTROL).0, 1);
    let mut down = Harness::from_records(records.clone(), WIDTH, HEIGHT);
    down.render();
    assert_eq!(down.key_update(KeyCode::PageDown, KeyModifiers::NONE).0, 0);
    let mut page_and_frame = prewarmed_harness(&records);
    let before_page = page_and_frame.fingerprint();
    page_and_frame.page_up();
    let after_page = page_and_frame.render_fingerprint();
    assert_ne!(after_page, before_page);

    let mut group = criterion.benchmark_group("nanocodex2_history/scroll_lazy_load_decision");
    group.sample_size(10);
    // These decisions only read cached viewport state. Reuse each prepared harness so dropping its
    // large transcript and layout cache is not accidentally included in the timed routine.
    group.bench_function("far_from_top_page_up", |bencher| {
        let mut harness = prewarmed_harness(&records);
        bencher.iter(|| black_box(harness.page_up()));
    });
    group.bench_function("near_top_page_up", |bencher| {
        let mut harness = near_top_harness(&records);
        bencher.iter(|| black_box(harness.page_up()));
    });
    group.bench_function("home", |bencher| {
        let mut harness = prewarmed_harness(&records);
        bencher.iter(|| black_box(harness.key_update(KeyCode::Home, KeyModifiers::CONTROL).0));
    });
    group.bench_function("page_down", |bencher| {
        let mut harness = prewarmed_harness(&records);
        bencher.iter(|| black_box(harness.key_update(KeyCode::PageDown, KeyModifiers::NONE).0));
    });
    group.bench_function("prewarmed_page_up_and_frame", |bencher| {
        bencher.iter_batched(
            || prewarmed_harness(&records),
            |mut harness| {
                black_box(harness.page_up());
                harness.render();
                black_box(harness)
            },
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

fn user_prompt_payload_benchmarks(criterion: &mut Criterion) {
    let mut group =
        criterion.benchmark_group("nanocodex2_history/user_prompt_cold_scroll_frame_120x40");
    group.sample_size(10);
    for (size_name, prompt_bytes) in [("4KiB", 4 * 1_024), ("256KiB", 256 * 1_024)] {
        let history = synthetic_user_prompt_history(prompt_bytes);
        let Some(ManagedEventData::TurnAccepted {
            input: PromptInput::Text(prompt),
            ..
        }) = history.first().map(|event| &event.data)
        else {
            panic!("prompt benchmark should contain a text user prompt");
        };
        assert_eq!(prompt.len(), prompt_bytes);
        let records = validated_projection(&history);

        let mut probe = prewarmed_harness(&records);
        assert!(!probe.screen_text().contains("user-prompt-payload"));
        let before_page = probe.fingerprint();
        probe.key_update(KeyCode::Home, KeyModifiers::CONTROL);
        let after_page = probe.render_fingerprint();
        assert_ne!(after_page, before_page);
        assert!(probe.screen_text().contains("user-prompt-payload"));

        group.throughput(Throughput::Bytes(prompt_bytes as u64));
        group.bench_with_input(
            BenchmarkId::new("ctrl_home_first_frame", size_name),
            &records,
            |bencher, records| {
                bencher.iter_batched(
                    || prewarmed_harness(records),
                    |mut harness| {
                        black_box(harness.key_update(KeyCode::Home, KeyModifiers::CONTROL));
                        harness.render();
                        black_box(harness)
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

fn tool_payload_benchmarks(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("nanocodex2_history/tool_payload_120x40");
    group.sample_size(10);
    for (size_name, output_bytes) in [("4KiB", 4 * 1_024), ("256KiB", 256 * 1_024)] {
        let records = validated_projection(&synthetic_history(EVENTS_PER_TURN, output_bytes));

        let mut collapsed = Harness::from_records(records.clone(), WIDTH, HEIGHT);
        let collapsed_fingerprint = collapsed.render_fingerprint();
        assert!(collapsed.screen_text().contains("cargo check"));
        let mut expanded = Harness::from_records(records.clone(), WIDTH, HEIGHT);
        assert!(expanded.toggle_all_tools());
        let expanded_fingerprint = expanded.render_fingerprint();
        assert_ne!(expanded_fingerprint, collapsed_fingerprint);
        assert!(expanded.screen_text().contains("payload-line"));

        group.throughput(Throughput::Bytes(output_bytes as u64));
        group.bench_with_input(
            BenchmarkId::new("collapsed_first", size_name),
            &records,
            |bencher, records| {
                bencher.iter_batched(
                    || Harness::from_records(records.clone(), WIDTH, HEIGHT),
                    |mut harness| {
                        harness.render();
                        black_box(&harness);
                    },
                    BatchSize::LargeInput,
                );
            },
        );

        let mut collapsed_cached = Harness::from_records(records.clone(), WIDTH, HEIGHT);
        let collapsed_cached_fingerprint = collapsed_cached.render_fingerprint();
        assert_eq!(
            collapsed_cached.render_fingerprint(),
            collapsed_cached_fingerprint
        );
        group.bench_function(BenchmarkId::new("collapsed_cached", size_name), |bencher| {
            bencher.iter(|| {
                collapsed_cached.render();
                black_box(&collapsed_cached);
            });
        });

        group.bench_with_input(
            BenchmarkId::new("expanded_first", size_name),
            &records,
            |bencher, records| {
                bencher.iter_batched(
                    || {
                        let mut harness = Harness::from_records(records.clone(), WIDTH, HEIGHT);
                        assert!(harness.toggle_all_tools());
                        harness
                    },
                    |mut harness| {
                        harness.render();
                        black_box(&harness);
                    },
                    BatchSize::LargeInput,
                );
            },
        );

        let mut expanded_cached = Harness::from_records(records.clone(), WIDTH, HEIGHT);
        assert!(expanded_cached.toggle_all_tools());
        let expanded_cached_fingerprint = expanded_cached.render_fingerprint();
        assert_eq!(
            expanded_cached.render_fingerprint(),
            expanded_cached_fingerprint
        );
        group.bench_function(BenchmarkId::new("expanded_cached", size_name), |bencher| {
            bencher.iter(|| {
                expanded_cached.render();
                black_box(&expanded_cached);
            });
        });
    }
    group.finish();
}

criterion_group!(
    tui_benches,
    projection_benchmarks,
    replay_and_frame_benchmarks,
    prepend_replay_benchmarks,
    proactive_prefetch_orchestration_benchmarks,
    scroll_benchmarks,
    user_prompt_payload_benchmarks,
    tool_payload_benchmarks,
);
criterion_main!(tui_benches);
