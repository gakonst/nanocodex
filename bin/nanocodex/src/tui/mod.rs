mod app;
mod clipboard;
mod composer;
mod diff;
mod external_editor;
mod markdown;
mod notification;
mod scheduler;
mod selection;
mod telemetry;
mod terminal;
mod transcript;
mod view;

use std::{
    collections::VecDeque,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use eyre::{Result, WrapErr};
use futures_util::StreamExt;
use nanocodex::{
    AgentEvent, AgentEvents, Nanocodex, NanocodexError, TimedAgentEvent, TurnControl, TurnResult,
};
use tokio::{
    sync::mpsc,
    time::{MissedTickBehavior, interval, sleep_until},
};
use tracing::{Instrument, info_span};

use self::{
    app::{App, PaneId, SubmittedPrompt},
    notification::Notifier,
    scheduler::{RenderScheduler, STREAM_FRAME_INTERVAL},
    telemetry::{StreamTelemetry, ViewTelemetry},
    terminal::TerminalSession,
    transcript::TranscriptItem,
};
use crate::config::AgentArgs;

const BTW_BOUNDARY: &str = r"You are answering an ephemeral BTW side question.
Treat inherited conversation history only as reference context. Do not resume or complete an
earlier task. Answer only the question after this boundary. Do not modify the workspace unless
that side question explicitly requests a mutation.

BTW question:
";
const DEFAULT_JAEGER_UI_URL: &str = "http://127.0.0.1:16686";
const JAEGER_UI_URL_ENV: &str = "NANOCODEX_JAEGER_UI_URL";
const MOUSE_SCROLL_ROWS: usize = 3;

enum WorkerCommand {
    Prompt {
        target: PaneId,
        prompt_id: u64,
        prompt: SubmittedPrompt,
    },
    Steer {
        target: PaneId,
        id: u64,
        prompt: SubmittedPrompt,
    },
    Cancel {
        target: PaneId,
    },
    OpenBtw {
        id: u64,
        prompt_id: Option<u64>,
        prompt: Option<SubmittedPrompt>,
    },
    CloseBtw {
        id: u64,
    },
    EditHistorical {
        source_branch_id: u64,
        new_branch_id: u64,
        prompt_id: u64,
    },
    SwitchMainBranch {
        id: u64,
    },
}

enum WorkerEvent {
    TurnTraceStarted {
        target: PaneId,
        id: u64,
        span: tracing::Span,
    },
    TurnTraceRejected {
        target: PaneId,
        id: u64,
    },
    TurnFinished {
        target: PaneId,
        main_branch_id: Option<u64>,
        error: Option<String>,
    },
    SteerAdmitted {
        target: PaneId,
        id: u64,
    },
    SteerQueued {
        target: PaneId,
        id: u64,
        prompt: String,
    },
    SteerFailed {
        target: PaneId,
        id: u64,
        error: String,
    },
    CancelAccepted {
        target: PaneId,
    },
    CancelFailed {
        target: PaneId,
        error: String,
    },
    BtwOpened {
        id: u64,
        request_id: Arc<str>,
    },
    BtwOpenFailed {
        id: u64,
        error: String,
    },
    BtwAgentEvent {
        id: u64,
        event: TimedAgentEvent,
    },
    BtwEventStreamClosed {
        id: u64,
    },
    MainBranchOpened {
        id: u64,
        parent_id: u64,
        prompt_id: u64,
        request_id: Arc<str>,
    },
    MainBranchOpenFailed {
        id: u64,
        error: String,
    },
    MainBranchSwitched {
        id: u64,
        request_id: Arc<str>,
    },
    MainBranchSwitchFailed {
        id: u64,
        error: String,
    },
    MainBranchAgentEvent {
        id: u64,
        event: TimedAgentEvent,
    },
    MainBranchEventStreamClosed {
        id: u64,
    },
}

struct MainWorkerBranch {
    id: u64,
    request_id: Arc<str>,
    agent: Nanocodex,
    turns: VecDeque<TrackedTurn>,
    prompt_order: Vec<u64>,
    results: Vec<(u64, TurnResult)>,
}

struct BtwWorker {
    id: u64,
    request_id: Arc<str>,
    agent: Nanocodex,
    first_prompt: bool,
    turns: VecDeque<TrackedTurn>,
}

struct TrackedTurn {
    id: u64,
    prompt_id: u64,
    control: TurnControl,
    span: tracing::Span,
}

struct SteerRequest {
    id: u64,
    prompt: SubmittedPrompt,
}

#[derive(Clone, Copy)]
struct TurnTarget<'a> {
    session_id: &'a str,
    pane: PaneId,
    main_branch_id: Option<u64>,
}

impl BtwWorker {
    fn prepare_prompt(&mut self, prompt: SubmittedPrompt) -> SubmittedPrompt {
        prepare_btw_prompt(&mut self.first_prompt, prompt)
    }
}

fn prepare_btw_prompt(first_prompt: &mut bool, mut prompt: SubmittedPrompt) -> SubmittedPrompt {
    if *first_prompt {
        *first_prompt = false;
        prompt.prepend_text(BTW_BOUNDARY);
    }
    prompt
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalAction {
    Redraw,
    Ignore,
    Quit,
    ExternalEditor,
}

enum UiAction {
    Terminal(Event),
    Agent(AgentEvent),
    AgentStreamClosed,
    Worker(WorkerEvent),
    WorkerStopped,
    Tick,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RedrawPriority {
    Immediate,
    Streaming,
    InputBurst,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UiUpdate {
    Redraw(RedrawPriority),
    Ignore,
    Quit,
    ExternalEditor,
}

struct UiModel {
    app: App,
    root_session_id: Arc<str>,
    agent_events_open: bool,
    worker_updates_open: bool,
    terminal_focused: bool,
    pending_notification: Option<String>,
    pending_mouse_scroll: Option<MouseScrollBurst>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScrollDirection {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MouseScrollBurst {
    target: PaneId,
    direction: ScrollDirection,
    rows: usize,
}

impl MouseScrollBurst {
    fn new(target: PaneId, direction: ScrollDirection) -> Self {
        Self {
            target,
            direction,
            rows: MOUSE_SCROLL_ROWS,
        }
    }

    fn push(&mut self, target: PaneId, direction: ScrollDirection) {
        if self.target != target || self.direction != direction {
            *self = Self::new(target, direction);
            return;
        }
        self.rows = self.rows.saturating_add(MOUSE_SCROLL_ROWS);
    }

    fn apply(self, app: &mut App) {
        match self.direction {
            ScrollDirection::Up => app.scroll_up_in(self.target, self.rows),
            ScrollDirection::Down => app.scroll_down_in(self.target, self.rows),
        }
    }
}

impl UiModel {
    fn new(app: App, root_session_id: Arc<str>) -> Self {
        Self {
            app,
            root_session_id,
            agent_events_open: true,
            worker_updates_open: true,
            terminal_focused: true,
            pending_notification: None,
            pending_mouse_scroll: None,
        }
    }

    fn queue_mouse_scroll(&mut self, direction: ScrollDirection) {
        let target = self.app.focus;
        if let Some(pending) = &mut self.pending_mouse_scroll {
            pending.push(target, direction);
        } else {
            self.pending_mouse_scroll = Some(MouseScrollBurst::new(target, direction));
        }
    }

    fn apply_pending_mouse_scroll(&mut self) {
        if let Some(pending) = self.pending_mouse_scroll.take() {
            pending.apply(&mut self.app);
        }
    }

    fn update(
        &mut self,
        action: UiAction,
        commands: &mpsc::UnboundedSender<WorkerCommand>,
    ) -> Result<UiUpdate> {
        match action {
            UiAction::Terminal(event) => {
                let mouse_scroll = match event {
                    Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollUp => {
                        Some(ScrollDirection::Up)
                    }
                    Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollDown => {
                        Some(ScrollDirection::Down)
                    }
                    _ => None,
                };
                if let Some(direction) = mouse_scroll {
                    let _ = self.app.clear_mouse_selection();
                    self.queue_mouse_scroll(direction);
                    return Ok(UiUpdate::Redraw(RedrawPriority::InputBurst));
                }
                // A non-wheel event is an ordering barrier: apply the gesture to
                // the pane it started in before focus or viewport state can change.
                self.apply_pending_mouse_scroll();
                match event {
                    Event::FocusGained => {
                        self.terminal_focused = true;
                        self.pending_notification = None;
                        return Ok(UiUpdate::Ignore);
                    }
                    Event::FocusLost => {
                        self.terminal_focused = false;
                        return Ok(UiUpdate::Ignore);
                    }
                    _ => {}
                }
                match handle_terminal_event(event, &mut self.app, &self.root_session_id, commands)?
                {
                    TerminalAction::Redraw => Ok(UiUpdate::Redraw(RedrawPriority::Immediate)),
                    TerminalAction::Ignore => Ok(UiUpdate::Ignore),
                    TerminalAction::Quit => Ok(UiUpdate::Quit),
                    TerminalAction::ExternalEditor => Ok(UiUpdate::ExternalEditor),
                }
            }
            UiAction::Agent(event) => {
                let updated = self.app.on_main_agent_event(0, &event);
                request_navigated_branch_switch(&mut self.app, commands)?;
                if updated {
                    Ok(UiUpdate::Redraw(RedrawPriority::Streaming))
                } else {
                    Ok(UiUpdate::Ignore)
                }
            }
            UiAction::AgentStreamClosed => {
                self.app.main_branch_event_stream_closed(0);
                self.agent_events_open = false;
                Ok(UiUpdate::Redraw(RedrawPriority::Streaming))
            }
            UiAction::Worker(update) => {
                if !self.terminal_focused
                    && let WorkerEvent::TurnFinished { target, error, .. } = &update
                {
                    let scope = if matches!(target, PaneId::Main) {
                        "Nanocodex"
                    } else {
                        "Nanocodex BTW"
                    };
                    self.pending_notification = Some(if error.is_some() {
                        format!("{scope} needs attention")
                    } else {
                        format!("{scope} finished")
                    });
                }
                handle_worker_update(&mut self.app, update, commands)?;
                Ok(UiUpdate::Redraw(RedrawPriority::Streaming))
            }
            UiAction::WorkerStopped => {
                self.app
                    .main
                    .push_output(TranscriptItem::Error("agent worker stopped".to_owned()));
                self.worker_updates_open = false;
                Ok(UiUpdate::Redraw(RedrawPriority::Streaming))
            }
            UiAction::Tick => {
                self.app.on_tick();
                Ok(UiUpdate::Redraw(RedrawPriority::Streaming))
            }
        }
    }
}

#[derive(Clone, Copy)]
enum SubmitIntent {
    Immediate,
    Queue,
}

#[derive(Debug, Eq, PartialEq)]
enum Submission {
    Prompt(SubmittedPrompt),
    Btw(Option<SubmittedPrompt>),
    CloseBtw,
    Cancel,
    Trace,
}

pub(crate) async fn run(config: AgentArgs, initial_prompt: Option<String>) -> Result<()> {
    let cwd = config
        .cwd()
        .canonicalize()
        .wrap_err("failed to resolve the working directory")?;
    let configured = config.build()?;
    let agent = configured.handle;
    let mut agent_events = configured.events;
    let root_session_id = Arc::<str>::from(agent_events.request_id());
    let _child_agents = configured.child_agents;
    let (worker_tx, worker_rx) = mpsc::unbounded_channel();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    spawn_agent_worker(agent, Arc::clone(&root_session_id), worker_rx, update_tx);

    let mut terminal = TerminalSession::enter().wrap_err("failed to initialize the terminal")?;
    let mut input_events = EventStream::new();
    let mut ticker = ui_ticker();
    let mut ui = UiModel::new(App::new(cwd), Arc::clone(&root_session_id));
    let mut scheduler = RenderScheduler::new(STREAM_FRAME_INTERVAL, Instant::now());
    let mut stream_telemetry = StreamTelemetry::default();
    let mut view_telemetry = ViewTelemetry::new(Arc::clone(&root_session_id));
    let mut notifier = Notifier::from_env();

    submit_initial_prompt(&mut ui.app, &root_session_id, &worker_tx, initial_prompt)?;

    loop {
        view_telemetry.observe(&ui.app);
        render_due_frame(
            &mut ui,
            &mut terminal,
            &mut scheduler,
            &mut stream_telemetry,
            &mut notifier,
        )?;

        let render_deadline = scheduler.deadline();
        tokio::select! {
            () = async {
                if let Some(deadline) = render_deadline {
                    sleep_until(deadline.into()).await;
                }
            }, if render_deadline.is_some() => {}
            event = input_events.next() => {
                let event = event.transpose()?.ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "terminal input closed")
                })?;
                let update = ui.update(UiAction::Terminal(event), &worker_tx)?;
                if update == UiUpdate::ExternalEditor {
                    input_events = run_external_editor(input_events, &mut terminal, &mut ui.app).await?;
                    scheduler.request_immediate(Instant::now());
                } else if apply_update(update, &mut scheduler) {
                    return Ok(());
                }
            }
            event = agent_events.recv_timed(), if ui.agent_events_open => {
                let received = event
                    .as_ref()
                    .map(|event| stream_telemetry.event_received(PaneId::Main, event));
                let action = event.map_or(UiAction::AgentStreamClosed, |event| {
                    UiAction::Agent(event.event)
                });
                let update = ui.update(action, &worker_tx)?;
                if let Some(received) = received {
                    stream_telemetry.event_applied(
                        received,
                        matches!(update, UiUpdate::Redraw(RedrawPriority::Streaming)),
                    );
                }
                if apply_update(update, &mut scheduler) {
                    return Ok(());
                }
            }
            update = update_rx.recv(), if ui.worker_updates_open => {
                if update.as_ref().is_some_and(|update| {
                    handle_worker_telemetry(update, &mut stream_telemetry)
                }) {
                    continue;
                }
                let received = update
                    .as_ref()
                    .and_then(|update| worker_event_received(update, &stream_telemetry));
                let action = update.map_or(UiAction::WorkerStopped, UiAction::Worker);
                let update = ui.update(action, &worker_tx)?;
                if let Some(received) = received {
                    stream_telemetry.event_applied(
                        received,
                        matches!(update, UiUpdate::Redraw(RedrawPriority::Streaming)),
                    );
                }
                if apply_update(update, &mut scheduler) {
                    return Ok(());
                }
            }
            _ = ticker.tick(), if ui.app.main.running || ui.app.btw.as_ref().is_some_and(|btw| btw.conversation.running) => {
                if apply_update(ui.update(UiAction::Tick, &worker_tx)?, &mut scheduler) {
                    return Ok(());
                }
            }
        }
    }
}

fn render_due_frame(
    ui: &mut UiModel,
    terminal: &mut TerminalSession,
    scheduler: &mut RenderScheduler,
    stream_telemetry: &mut StreamTelemetry,
    notifier: &mut Notifier,
) -> Result<()> {
    if !scheduler.is_due(Instant::now()) {
        return Ok(());
    }
    ui.apply_pending_mouse_scroll();
    ui.app.advance_smooth_scroll();
    let render_started = Instant::now();
    let draw_metrics = terminal.draw(|frame| view::render(frame, &mut ui.app))?;
    if let Some(text) = ui.app.take_pending_copy()
        && let Err(error) = clipboard::copy_to_clipboard(&text)
    {
        tracing::warn!(%error, "failed to copy the mouse selection");
        ui.app
            .set_active_status(format!("Clipboard copy failed: {error}"));
    }
    let presented_at = Instant::now();
    scheduler.presented(presented_at);
    stream_telemetry.frame_presented(render_started, presented_at, draw_metrics, &ui.app);
    if let Some(message) = ui.pending_notification.take() {
        notifier.notify(terminal, &message);
    }
    if ui.app.smooth_scroll_pending() {
        scheduler.request_streaming(presented_at);
    }
    Ok(())
}

fn worker_event_received(
    update: &WorkerEvent,
    telemetry: &StreamTelemetry,
) -> Option<telemetry::ReceivedEvent> {
    match update {
        WorkerEvent::BtwAgentEvent { id, event } => {
            Some(telemetry.event_received(PaneId::Btw(*id), event))
        }
        WorkerEvent::MainBranchAgentEvent { event, .. } => {
            Some(telemetry.event_received(PaneId::Main, event))
        }
        _ => None,
    }
}

fn ui_ticker() -> tokio::time::Interval {
    let mut ticker = interval(Duration::from_millis(80));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker
}

fn handle_worker_telemetry(update: &WorkerEvent, telemetry: &mut StreamTelemetry) -> bool {
    match update {
        WorkerEvent::TurnTraceStarted { target, id, span } => {
            telemetry.register_turn(*target, *id, span.clone());
            true
        }
        WorkerEvent::TurnTraceRejected { target, id } => {
            telemetry.reject_turn(*target, *id);
            true
        }
        _ => false,
    }
}

fn submit_initial_prompt(
    app: &mut App,
    root_session_id: &str,
    worker: &mpsc::UnboundedSender<WorkerCommand>,
    initial_prompt: Option<String>,
) -> Result<()> {
    if let Some(prompt) = initial_prompt {
        app.input = prompt;
        app.cursor = app.input.len();
        submit(app, root_session_id, worker, SubmitIntent::Immediate)?;
    }
    Ok(())
}

fn apply_update(update: UiUpdate, scheduler: &mut RenderScheduler) -> bool {
    let now = Instant::now();
    match update {
        UiUpdate::Redraw(RedrawPriority::Immediate) => scheduler.request_immediate(now),
        UiUpdate::Redraw(RedrawPriority::Streaming) => scheduler.request_streaming(now),
        UiUpdate::Redraw(RedrawPriority::InputBurst) => scheduler.request_input_burst(now),
        UiUpdate::Ignore | UiUpdate::ExternalEditor => {}
        UiUpdate::Quit => return true,
    }
    false
}

fn handle_worker_update(
    app: &mut App,
    update: WorkerEvent,
    commands: &mpsc::UnboundedSender<WorkerCommand>,
) -> Result<()> {
    match update {
        WorkerEvent::TurnFinished {
            target,
            main_branch_id,
            error,
        } => {
            app.turn_finished(target, main_branch_id, error);
            request_navigated_branch_switch(app, commands)?;
        }
        WorkerEvent::TurnTraceStarted { .. } | WorkerEvent::TurnTraceRejected { .. } => {}
        WorkerEvent::SteerAdmitted { target, id } => app.steer_admitted(target, id),
        WorkerEvent::SteerQueued { target, id, prompt } => {
            app.steer_queued(target, id, prompt);
        }
        WorkerEvent::SteerFailed { target, id, error } => app.steer_failed(target, id, error),
        WorkerEvent::CancelAccepted { target } => app.cancel_accepted(target),
        WorkerEvent::CancelFailed { target, error } => app.cancel_failed(target, error),
        WorkerEvent::BtwOpened { id, request_id } => app.btw_opened(id, request_id),
        WorkerEvent::BtwOpenFailed { id, error } => app.btw_failed(id, error),
        WorkerEvent::BtwAgentEvent { id, event } => {
            let _ = app.on_agent_event(PaneId::Btw(id), &event.event);
        }
        WorkerEvent::BtwEventStreamClosed { id } => {
            if app.btw_id() == Some(id) {
                app.btw_failed(id, "BTW event stream closed".to_owned());
            }
        }
        WorkerEvent::MainBranchOpened {
            id,
            parent_id,
            prompt_id,
            request_id,
        } => {
            if let Some(prompt) = app.main_branch_opened(id, parent_id, prompt_id, request_id) {
                let prompt = SubmittedPrompt::text(prompt);
                let prompt_id = app
                    .queue_prompt(PaneId::Main, prompt.display().to_owned())
                    .ok_or_else(|| {
                        eyre::eyre!("historical branch disappeared before submission")
                    })?;
                send_command(
                    commands,
                    WorkerCommand::Prompt {
                        target: PaneId::Main,
                        prompt_id,
                        prompt,
                    },
                )?;
            }
        }
        WorkerEvent::MainBranchOpenFailed { id, error } => {
            app.main_branch_open_failed(id, &error);
        }
        WorkerEvent::MainBranchSwitched { id, request_id } => {
            app.main_branch_switched(id, request_id);
            request_navigated_branch_switch(app, commands)?;
        }
        WorkerEvent::MainBranchSwitchFailed { id, error } => {
            app.main_branch_switch_failed(id, &error);
        }
        WorkerEvent::MainBranchAgentEvent { id, event } => {
            let _ = app.on_main_agent_event(id, &event.event);
            request_navigated_branch_switch(app, commands)?;
        }
        WorkerEvent::MainBranchEventStreamClosed { id } => {
            app.main_branch_event_stream_closed(id);
        }
    }
    Ok(())
}

fn spawn_agent_worker(
    root: Nanocodex,
    root_session_id: Arc<str>,
    mut commands: mpsc::UnboundedReceiver<WorkerCommand>,
    updates: mpsc::UnboundedSender<WorkerEvent>,
) {
    tokio::spawn(async move {
        let (finished_tx, mut finished_rx) = mpsc::unbounded_channel::<FinishedTurn>();
        let mut worker = AgentWorker {
            main: MainWorkerBranch {
                id: 0,
                request_id: root_session_id,
                agent: root,
                turns: VecDeque::new(),
                prompt_order: Vec::new(),
                results: Vec::new(),
            },
            archived_main: Vec::new(),
            next_turn_id: 1,
            btw: None,
            finished: finished_tx,
            updates,
        };
        loop {
            tokio::select! {
                Some(finished) = finished_rx.recv() => {
                    worker.finish_turn(finished);
                }
                command = commands.recv() => {
                    let Some(command) = command else {
                        break;
                    };
                    worker.handle_command(command).await;
                }
            }
        }
    });
}

struct AgentWorker {
    main: MainWorkerBranch,
    archived_main: Vec<MainWorkerBranch>,
    next_turn_id: u64,
    btw: Option<BtwWorker>,
    finished: mpsc::UnboundedSender<FinishedTurn>,
    updates: mpsc::UnboundedSender<WorkerEvent>,
}

impl AgentWorker {
    async fn handle_command(&mut self, command: WorkerCommand) {
        match command {
            WorkerCommand::Prompt {
                target,
                prompt_id,
                prompt,
            } => self.prompt(target, prompt_id, prompt).await,
            WorkerCommand::Steer { target, id, prompt } => self.steer(target, id, prompt).await,
            WorkerCommand::Cancel { target } => self.cancel(target).await,
            WorkerCommand::OpenBtw {
                id,
                prompt_id,
                prompt,
            } => self.open_btw(id, prompt_id, prompt).await,
            WorkerCommand::CloseBtw { id } => {
                if self.btw.as_ref().is_some_and(|branch| branch.id == id) {
                    self.btw = None;
                }
            }
            WorkerCommand::EditHistorical {
                source_branch_id,
                new_branch_id,
                prompt_id,
            } => {
                self.edit_historical(source_branch_id, new_branch_id, prompt_id)
                    .await;
            }
            WorkerCommand::SwitchMainBranch { id } => self.switch_main_branch(id),
        }
    }

    async fn prompt(&mut self, target: PaneId, prompt_id: u64, prompt: SubmittedPrompt) {
        match target {
            PaneId::Main => {
                if let Some(turn) = start_turn(
                    &self.main.agent,
                    TurnTarget {
                        session_id: &self.main.request_id,
                        pane: target,
                        main_branch_id: Some(self.main.id),
                    },
                    prompt_id,
                    prompt,
                    &mut self.next_turn_id,
                    &self.finished,
                    &self.updates,
                )
                .await
                {
                    self.main.prompt_order.push(prompt_id);
                    self.main.turns.push_back(turn);
                }
            }
            PaneId::Btw(id) => {
                let Some(branch) = self.btw.as_mut().filter(|branch| branch.id == id) else {
                    drop(self.updates.send(WorkerEvent::TurnFinished {
                        target,
                        main_branch_id: None,
                        error: Some("BTW branch is not available".to_owned()),
                    }));
                    return;
                };
                let prompt = branch.prepare_prompt(prompt);
                if let Some(turn) = start_turn(
                    &branch.agent,
                    TurnTarget {
                        session_id: &branch.request_id,
                        pane: target,
                        main_branch_id: None,
                    },
                    prompt_id,
                    prompt,
                    &mut self.next_turn_id,
                    &self.finished,
                    &self.updates,
                )
                .await
                {
                    branch.turns.push_back(turn);
                }
            }
        }
    }

    async fn steer(&mut self, target: PaneId, steer_id: u64, prompt: SubmittedPrompt) {
        let turn = match target {
            PaneId::Main => {
                steer_turn(
                    &self.main.agent,
                    &self.main.turns,
                    TurnTarget {
                        session_id: &self.main.request_id,
                        pane: target,
                        main_branch_id: Some(self.main.id),
                    },
                    SteerRequest {
                        id: steer_id,
                        prompt,
                    },
                    &mut self.next_turn_id,
                    &self.finished,
                    &self.updates,
                )
                .await
            }
            PaneId::Btw(branch_id) => {
                let Some(branch) = self.btw.as_mut().filter(|branch| branch.id == branch_id) else {
                    drop(self.updates.send(WorkerEvent::SteerFailed {
                        target,
                        id: steer_id,
                        error: "BTW branch is not available".to_owned(),
                    }));
                    return;
                };
                steer_turn(
                    &branch.agent,
                    &branch.turns,
                    TurnTarget {
                        session_id: &branch.request_id,
                        pane: target,
                        main_branch_id: None,
                    },
                    SteerRequest {
                        id: steer_id,
                        prompt,
                    },
                    &mut self.next_turn_id,
                    &self.finished,
                    &self.updates,
                )
                .await
            }
        };
        if let Some(turn) = turn {
            match target {
                PaneId::Main => {
                    self.main.prompt_order.push(turn.prompt_id);
                    self.main.turns.push_back(turn);
                }
                PaneId::Btw(branch_id) => {
                    if let Some(branch) = self.btw.as_mut().filter(|branch| branch.id == branch_id)
                    {
                        branch.turns.push_back(turn);
                    }
                }
            }
        }
    }

    async fn cancel(&self, target: PaneId) {
        let (turns, session_id) = match target {
            PaneId::Main => (Some(&self.main.turns), self.main.request_id.as_ref()),
            PaneId::Btw(id) => self
                .btw
                .as_ref()
                .filter(|branch| branch.id == id)
                .map_or((None, ""), |branch| {
                    (Some(&branch.turns), branch.request_id.as_ref())
                }),
        };
        cancel_turn(turns, session_id, target, &self.updates).await;
    }

    async fn open_btw(&mut self, id: u64, prompt_id: Option<u64>, prompt: Option<SubmittedPrompt>) {
        self.btw = None;
        let span = info_span!(
            target: "nanocodex",
            parent: None,
            "tui.btw.open",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            session.id = self.main.request_id.as_ref(),
            tui.btw.id = id,
            tui.btw.session_id = tracing::field::Empty,
            status = tracing::field::Empty,
        );
        match self.main.agent.fork().instrument(span.clone()).await {
            Ok((agent, events)) => {
                let request_id = Arc::<str>::from(events.request_id());
                span.record("tui.btw.session_id", request_id.as_ref());
                span.record("status", "completed");
                span.record("otel.status_code", "OK");
                forward_btw_events(id, events, self.updates.clone());
                drop(self.updates.send(WorkerEvent::BtwOpened {
                    id,
                    request_id: Arc::clone(&request_id),
                }));
                let mut branch = BtwWorker {
                    id,
                    request_id,
                    agent,
                    first_prompt: true,
                    turns: VecDeque::new(),
                };
                if let Some(prompt) = prompt {
                    let prompt = branch.prepare_prompt(prompt);
                    let Some(prompt_id) = prompt_id else {
                        drop(self.updates.send(WorkerEvent::BtwOpenFailed {
                            id,
                            error: "BTW prompt identity was unavailable".to_owned(),
                        }));
                        return;
                    };
                    if let Some(turn) = start_turn(
                        &branch.agent,
                        TurnTarget {
                            session_id: &branch.request_id,
                            pane: PaneId::Btw(id),
                            main_branch_id: None,
                        },
                        prompt_id,
                        prompt,
                        &mut self.next_turn_id,
                        &self.finished,
                        &self.updates,
                    )
                    .await
                    {
                        branch.turns.push_back(turn);
                    }
                }
                self.btw = Some(branch);
            }
            Err(error) => {
                span.record("status", "failed");
                span.record("otel.status_code", "ERROR");
                drop(self.updates.send(WorkerEvent::BtwOpenFailed {
                    id,
                    error: error.to_string(),
                }));
            }
        }
    }

    async fn edit_historical(&mut self, source_branch_id: u64, new_branch_id: u64, prompt_id: u64) {
        if self.main.id != source_branch_id || self.btw.is_some() {
            drop(self.updates.send(WorkerEvent::MainBranchOpenFailed {
                id: new_branch_id,
                error: "close /btw before editing history".to_owned(),
            }));
            return;
        }
        let Some(position) = self
            .main
            .prompt_order
            .iter()
            .position(|candidate| *candidate == prompt_id)
        else {
            drop(self.updates.send(WorkerEvent::MainBranchOpenFailed {
                id: new_branch_id,
                error: "the selected prompt is not associated with this branch".to_owned(),
            }));
            return;
        };
        let parent = self.main.prompt_order[..position]
            .iter()
            .rev()
            .find_map(|candidate| {
                self.main
                    .results
                    .iter()
                    .find(|(completed_id, _)| completed_id == candidate)
                    .map(|(_, result)| result.clone())
            });
        let fork = if let Some(parent) = parent.as_ref() {
            self.main.agent.fork_from(parent).await
        } else {
            self.main.agent.spawn().await
        };
        let (agent, events) = match fork {
            Ok(branch) => branch,
            Err(error) => {
                drop(self.updates.send(WorkerEvent::MainBranchOpenFailed {
                    id: new_branch_id,
                    error: error.to_string(),
                }));
                return;
            }
        };

        let request_id = Arc::<str>::from(events.request_id());
        let inherited_ids = &self.main.prompt_order[..position];
        let inherited_results = self
            .main
            .results
            .iter()
            .filter(|(completed_id, _)| inherited_ids.contains(completed_id))
            .cloned()
            .collect();
        let branch = MainWorkerBranch {
            id: new_branch_id,
            request_id: Arc::clone(&request_id),
            agent,
            turns: VecDeque::new(),
            prompt_order: inherited_ids.to_vec(),
            results: inherited_results,
        };
        let parent_id = self.main.id;
        let previous = std::mem::replace(&mut self.main, branch);
        self.archived_main.push(previous);
        forward_main_branch_events(new_branch_id, events, self.updates.clone());
        drop(self.updates.send(WorkerEvent::MainBranchOpened {
            id: new_branch_id,
            parent_id,
            prompt_id,
            request_id,
        }));
    }

    fn switch_main_branch(&mut self, id: u64) {
        if self.main.id == id {
            drop(self.updates.send(WorkerEvent::MainBranchSwitched {
                id,
                request_id: Arc::clone(&self.main.request_id),
            }));
            return;
        }
        if !self.main.turns.is_empty() || self.btw.is_some() {
            drop(self.updates.send(WorkerEvent::MainBranchSwitchFailed {
                id,
                error: "finish the main turn and close /btw before switching branches".to_owned(),
            }));
            return;
        }
        let Some(position) = self.archived_main.iter().position(|branch| branch.id == id) else {
            drop(self.updates.send(WorkerEvent::MainBranchSwitchFailed {
                id,
                error: "the requested branch is no longer available".to_owned(),
            }));
            return;
        };
        if !self.archived_main[position].turns.is_empty() {
            drop(self.updates.send(WorkerEvent::MainBranchSwitchFailed {
                id,
                error: "the requested branch still has an active turn".to_owned(),
            }));
            return;
        }
        let requested = self.archived_main.swap_remove(position);
        let previous = std::mem::replace(&mut self.main, requested);
        self.archived_main.push(previous);
        drop(self.updates.send(WorkerEvent::MainBranchSwitched {
            id,
            request_id: Arc::clone(&self.main.request_id),
        }));
    }

    fn finish_turn(&mut self, finished: FinishedTurn) {
        let main_branch_id = finished.main_branch_id;
        match finished.target {
            PaneId::Main => {
                let branch_id = main_branch_id.unwrap_or(self.main.id);
                let branch = if self.main.id == branch_id {
                    Some(&mut self.main)
                } else {
                    self.archived_main
                        .iter_mut()
                        .find(|branch| branch.id == branch_id)
                };
                if let Some(branch) = branch {
                    remove_finished(&mut branch.turns, finished.id);
                    if let Some(result) = finished.result {
                        branch.results.push((finished.prompt_id, result));
                    }
                }
            }
            PaneId::Btw(id) => {
                if let Some(branch) = self.btw.as_mut().filter(|branch| branch.id == id) {
                    remove_finished(&mut branch.turns, finished.id);
                }
            }
        }
        drop(self.updates.send(WorkerEvent::TurnFinished {
            target: finished.target,
            main_branch_id,
            error: finished.error,
        }));
    }
}

async fn start_turn(
    agent: &Nanocodex,
    target: TurnTarget<'_>,
    prompt_id: u64,
    prompt: SubmittedPrompt,
    next_turn_id: &mut u64,
    finished: &mpsc::UnboundedSender<FinishedTurn>,
    updates: &mpsc::UnboundedSender<WorkerEvent>,
) -> Option<TrackedTurn> {
    let started_at = Instant::now();
    let id = *next_turn_id;
    let span = info_span!(
        target: "nanocodex",
        parent: None,
        "tui.turn",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        session.id = target.session_id,
        tui.turn.id = id,
        tui.pane = telemetry::pane_name(target.pane),
        tui.btw.id = telemetry::pane_btw_id(target.pane).unwrap_or_default(),
        status = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
    );
    drop(updates.send(WorkerEvent::TurnTraceStarted {
        target: target.pane,
        id,
        span: span.clone(),
    }));
    match agent
        .prompt(prompt.into_prompt())
        .instrument(span.clone())
        .await
    {
        Ok(turn) => {
            *next_turn_id = next_turn_id.saturating_add(1);
            let control = turn.control();
            let finished = finished.clone();
            let task_span = span.clone();
            tokio::spawn(
                async move {
                    let (result, error, status, otel_status) = match turn.result().await {
                        Ok(result) => (Some(result), None, "completed", "OK"),
                        Err(NanocodexError::TurnCancelled) => (None, None, "cancelled", "ERROR"),
                        Err(error) => (None, Some(error.to_string()), "failed", "ERROR"),
                    };
                    task_span.record("status", status);
                    task_span.record("otel.status_code", otel_status);
                    task_span.record(
                        "duration_ns",
                        telemetry::elapsed_ns(started_at, Instant::now()),
                    );
                    drop(finished.send(FinishedTurn {
                        id,
                        target: target.pane,
                        main_branch_id: target.main_branch_id,
                        prompt_id,
                        result,
                        error,
                    }));
                }
                .instrument(span.clone()),
            );
            Some(TrackedTurn {
                id,
                prompt_id,
                control,
                span,
            })
        }
        Err(error) => {
            drop(updates.send(WorkerEvent::TurnTraceRejected {
                target: target.pane,
                id,
            }));
            span.record("status", "rejected");
            span.record("otel.status_code", "ERROR");
            span.record(
                "duration_ns",
                telemetry::elapsed_ns(started_at, Instant::now()),
            );
            drop(updates.send(WorkerEvent::TurnFinished {
                target: target.pane,
                main_branch_id: target.main_branch_id,
                error: Some(error.to_string()),
            }));
            None
        }
    }
}

async fn steer_turn(
    agent: &Nanocodex,
    turns: &VecDeque<TrackedTurn>,
    target: TurnTarget<'_>,
    request: SteerRequest,
    next_turn_id: &mut u64,
    finished: &mpsc::UnboundedSender<FinishedTurn>,
    updates: &mpsc::UnboundedSender<WorkerEvent>,
) -> Option<TrackedTurn> {
    for turn in turns {
        let started_at = Instant::now();
        let span = info_span!(
            target: "nanocodex",
            parent: &turn.span,
            "tui.steer",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            session.id = target.session_id,
            tui.turn.id = turn.id,
            tui.steer.id = request.id,
            tui.pane = telemetry::pane_name(target.pane),
            status = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        let outcome = turn
            .control
            .steer(request.prompt.clone().into_prompt())
            .instrument(span.clone())
            .await;
        span.record(
            "duration_ns",
            telemetry::elapsed_ns(started_at, Instant::now()),
        );
        match outcome {
            Ok(()) => {
                span.record("status", "admitted");
                span.record("otel.status_code", "OK");
                drop(updates.send(WorkerEvent::SteerAdmitted {
                    target: target.pane,
                    id: request.id,
                }));
                return None;
            }
            Err(NanocodexError::TurnNotSteerable) => {
                span.record("status", "not_steerable");
                span.record("otel.status_code", "OK");
            }
            Err(error) => {
                span.record("status", "failed");
                span.record("otel.status_code", "ERROR");
                drop(updates.send(WorkerEvent::SteerFailed {
                    target: target.pane,
                    id: request.id,
                    error: error.to_string(),
                }));
                return None;
            }
        }
    }
    // Completion delivery can lag behind the driver's exact active-turn
    // state. If no retained capability is active, preserve this as a new turn.
    drop(updates.send(WorkerEvent::SteerQueued {
        target: target.pane,
        id: request.id,
        prompt: request.prompt.display().to_owned(),
    }));
    start_turn(
        agent,
        target,
        request.id,
        request.prompt,
        next_turn_id,
        finished,
        updates,
    )
    .await
}

async fn cancel_turn(
    turns: Option<&VecDeque<TrackedTurn>>,
    session_id: &str,
    target: PaneId,
    updates: &mpsc::UnboundedSender<WorkerEvent>,
) {
    let mut outcome = Err(NanocodexError::TurnNotCancellable);
    for turn in turns.into_iter().flatten() {
        let started_at = Instant::now();
        let span = info_span!(
            target: "nanocodex",
            parent: &turn.span,
            "tui.cancel",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            session.id = session_id,
            tui.turn.id = turn.id,
            tui.pane = telemetry::pane_name(target),
            status = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        let result = turn.control.cancel().instrument(span.clone()).await;
        span.record(
            "duration_ns",
            telemetry::elapsed_ns(started_at, Instant::now()),
        );
        match result {
            Err(NanocodexError::TurnNotCancellable) => {
                span.record("status", "not_cancellable");
                span.record("otel.status_code", "OK");
            }
            result => {
                span.record("status", if result.is_ok() { "accepted" } else { "failed" });
                span.record(
                    "otel.status_code",
                    if result.is_ok() { "OK" } else { "ERROR" },
                );
                outcome = result;
                break;
            }
        }
    }
    let event = match outcome {
        Ok(()) => WorkerEvent::CancelAccepted { target },
        Err(error) => WorkerEvent::CancelFailed {
            target,
            error: error.to_string(),
        },
    };
    drop(updates.send(event));
}

struct FinishedTurn {
    id: u64,
    target: PaneId,
    main_branch_id: Option<u64>,
    prompt_id: u64,
    result: Option<TurnResult>,
    error: Option<String>,
}

fn remove_finished(turns: &mut VecDeque<TrackedTurn>, id: u64) {
    if let Some(index) = turns.iter().position(|turn| turn.id == id) {
        drop(turns.remove(index));
    }
}

fn forward_btw_events(
    id: u64,
    mut events: AgentEvents,
    updates: mpsc::UnboundedSender<WorkerEvent>,
) {
    tokio::spawn(async move {
        while let Some(event) = events.recv_timed().await {
            if updates
                .send(WorkerEvent::BtwAgentEvent { id, event })
                .is_err()
            {
                return;
            }
        }
        drop(updates.send(WorkerEvent::BtwEventStreamClosed { id }));
    });
}

fn forward_main_branch_events(
    id: u64,
    mut events: AgentEvents,
    updates: mpsc::UnboundedSender<WorkerEvent>,
) {
    tokio::spawn(async move {
        while let Some(event) = events.recv_timed().await {
            if updates
                .send(WorkerEvent::MainBranchAgentEvent { id, event })
                .is_err()
            {
                return;
            }
        }
        drop(updates.send(WorkerEvent::MainBranchEventStreamClosed { id }));
    });
}

fn handle_terminal_event(
    event: Event,
    app: &mut App,
    root_session_id: &str,
    commands: &mpsc::UnboundedSender<WorkerCommand>,
) -> Result<TerminalAction> {
    match event {
        Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            let _ = app.clear_mouse_selection();
            handle_key(key, app, root_session_id, commands)
        }
        Event::Paste(text) => {
            let _ = app.clear_mouse_selection();
            app.handle_paste(&text);
            Ok(TerminalAction::Redraw)
        }
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let changed = app.begin_mouse_selection((mouse.column, mouse.row).into());
                Ok(if changed {
                    TerminalAction::Redraw
                } else {
                    TerminalAction::Ignore
                })
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let changed = app.drag_mouse_selection((mouse.column, mouse.row).into());
                Ok(if changed {
                    TerminalAction::Redraw
                } else {
                    TerminalAction::Ignore
                })
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let changed = app.finish_mouse_selection((mouse.column, mouse.row).into());
                Ok(if changed {
                    TerminalAction::Redraw
                } else {
                    TerminalAction::Ignore
                })
            }
            MouseEventKind::ScrollUp => {
                let _ = app.clear_mouse_selection();
                app.scroll_up(MOUSE_SCROLL_ROWS);
                Ok(TerminalAction::Redraw)
            }
            MouseEventKind::ScrollDown => {
                let _ = app.clear_mouse_selection();
                app.scroll_down(MOUSE_SCROLL_ROWS);
                Ok(TerminalAction::Redraw)
            }
            _ => Ok(TerminalAction::Ignore),
        },
        Event::Resize(_, _) => {
            let _ = app.clear_mouse_selection();
            Ok(TerminalAction::Redraw)
        }
        Event::FocusGained | Event::FocusLost | Event::Key(_) => Ok(TerminalAction::Ignore),
    }
}

fn handle_key(
    key: KeyEvent,
    app: &mut App,
    root_session_id: &str,
    commands: &mpsc::UnboundedSender<WorkerCommand>,
) -> Result<TerminalAction> {
    if matches!(key.code, KeyCode::Char('v' | 'V'))
        && key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        paste_clipboard_image(app, clipboard::paste_image_to_temp_png);
        return Ok(TerminalAction::Redraw);
    }

    if let Some(action) = handle_inline_historical_editor_key(key, app, commands)? {
        return Ok(action);
    }

    if let Some(action) = handle_global_navigation_key(key, app, commands)? {
        return Ok(action);
    }

    if let Some(action) = handle_branch_navigator_key(key, app, commands)? {
        return Ok(action);
    }

    if let Some(action) = handle_transcript_selection_key(key, app) {
        return Ok(action);
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => return Ok(TerminalAction::Quit),
            KeyCode::Char('g') => return Ok(TerminalAction::ExternalEditor),
            KeyCode::Char('d') if app.input.is_empty() => return Ok(TerminalAction::Quit),
            KeyCode::Char('d') => app.delete(),
            KeyCode::Char('h') => app.backspace(),
            KeyCode::Char('j') => app.insert_char('\n'),
            KeyCode::Char('a') => app.move_home(),
            KeyCode::Char('e') => app.move_end(),
            KeyCode::Char('b') => app.move_left(),
            KeyCode::Char('f') => app.move_right(),
            KeyCode::Char('p') => app.move_up(),
            KeyCode::Char('n') => app.move_down(),
            KeyCode::Char('w') => app.delete_word_before_cursor(),
            KeyCode::Char('u') => app.delete_to_line_start(),
            KeyCode::Char('k') => app.delete_to_line_end(),
            KeyCode::Left => app.move_word_left(),
            KeyCode::Right => app.move_word_right(),
            KeyCode::End => app.jump_to_bottom(),
            _ => {}
        }
        return Ok(TerminalAction::Redraw);
    }

    if key.modifiers.contains(KeyModifiers::ALT) {
        match key.code {
            KeyCode::Char('b') | KeyCode::Left => app.move_word_left(),
            KeyCode::Char('f') | KeyCode::Right => app.move_word_right(),
            _ => {}
        }
        return Ok(TerminalAction::Redraw);
    }

    match key.code {
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            app.insert_char('\n');
        }
        KeyCode::Enter => submit(app, root_session_id, commands, SubmitIntent::Immediate)?,
        KeyCode::Char(character) => app.insert_char(character),
        KeyCode::Backspace => app.backspace(),
        KeyCode::Delete => app.delete(),
        KeyCode::Left => app.move_left(),
        KeyCode::Right => app.move_right(),
        KeyCode::Home => app.move_home(),
        KeyCode::End => app.move_end(),
        KeyCode::Up => app.move_up(),
        KeyCode::Down => app.move_down(),
        KeyCode::PageUp => app.scroll_up(12),
        KeyCode::PageDown => app.scroll_down(12),
        KeyCode::Esc if key.kind == KeyEventKind::Repeat => {}
        KeyCode::Esc => {
            if let Some(target) = app.handle_escape(Instant::now()) {
                app.cancel_pending(target);
                send_command(commands, WorkerCommand::Cancel { target })?;
            }
        }
        KeyCode::Tab if app.has_input() => {
            submit(app, root_session_id, commands, SubmitIntent::Queue)?;
        }
        KeyCode::Tab | KeyCode::BackTab => app.toggle_focus(),
        KeyCode::Insert
        | KeyCode::F(_)
        | KeyCode::Null
        | KeyCode::CapsLock
        | KeyCode::ScrollLock
        | KeyCode::NumLock
        | KeyCode::PrintScreen
        | KeyCode::Pause
        | KeyCode::Menu
        | KeyCode::KeypadBegin
        | KeyCode::Media(_)
        | KeyCode::Modifier(_) => {}
    }
    Ok(TerminalAction::Redraw)
}

fn paste_clipboard_image(
    app: &mut App,
    paste: impl FnOnce() -> Result<std::path::PathBuf, String>,
) {
    match paste() {
        Ok(path) => {
            app.attach_local_image(path);
            app.insert_char(' ');
        }
        Err(error) => {
            tracing::warn!(%error, "failed to paste a clipboard image");
            app.push_active_error(format!("Failed to paste image: {error}"));
        }
    }
}

fn handle_global_navigation_key(
    key: KeyEvent,
    app: &mut App,
    commands: &mpsc::UnboundedSender<WorkerCommand>,
) -> Result<Option<TerminalAction>> {
    if !key
        .modifiers
        .contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return Ok(None);
    }
    if matches!(key.code, KeyCode::Char('b')) {
        let _ = app.toggle_branch_navigator();
        return Ok(Some(TerminalAction::Redraw));
    }
    let direction = match key.code {
        KeyCode::Up => Some(-1),
        KeyCode::Down => Some(1),
        _ => None,
    };
    let Some(direction) = direction else {
        return Ok(None);
    };
    if let Some(id) = app.cycle_main_branch(direction) {
        send_command(commands, WorkerCommand::SwitchMainBranch { id })?;
    }
    Ok(Some(TerminalAction::Redraw))
}

fn handle_branch_navigator_key(
    key: KeyEvent,
    app: &mut App,
    commands: &mpsc::UnboundedSender<WorkerCommand>,
) -> Result<Option<TerminalAction>> {
    if !app.branch_navigator_active() {
        return Ok(None);
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Ok(Some(TerminalAction::Quit));
    }
    if key.modifiers.is_empty() {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                app.move_branch_navigator(-1);
                request_navigated_branch_switch(app, commands)?;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.move_branch_navigator(1);
                request_navigated_branch_switch(app, commands)?;
            }
            KeyCode::Enter => request_navigated_branch_switch(app, commands)?,
            KeyCode::Esc | KeyCode::Char('q') => app.close_branch_navigator(),
            _ => {}
        }
    }
    Ok(Some(TerminalAction::Redraw))
}

fn request_navigated_branch_switch(
    app: &mut App,
    commands: &mpsc::UnboundedSender<WorkerCommand>,
) -> Result<()> {
    if let Some(id) = app.switch_to_navigated_branch() {
        send_command(commands, WorkerCommand::SwitchMainBranch { id })?;
    }
    Ok(())
}

fn handle_transcript_selection_key(key: KeyEvent, app: &mut App) -> Option<TerminalAction> {
    if !app.transcript_selection_active() {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => return Some(TerminalAction::Quit),
            KeyCode::Char('p') => app.move_up(),
            KeyCode::Char('n') => app.move_down(),
            _ => {}
        }
    } else if key.modifiers.is_empty() {
        match key.code {
            KeyCode::Up => app.move_up(),
            KeyCode::Down => app.move_down(),
            KeyCode::Char('e') => {
                let _ = app.start_historical_edit();
            }
            KeyCode::Esc => app.dismiss_transcript_selection(),
            _ => {}
        }
    }
    Some(TerminalAction::Redraw)
}

fn request_historical_edit(
    app: &mut App,
    commands: &mpsc::UnboundedSender<WorkerCommand>,
) -> Result<()> {
    let Some(request) = app.commit_historical_edit() else {
        return Ok(());
    };
    send_command(
        commands,
        WorkerCommand::EditHistorical {
            source_branch_id: request.source_branch,
            new_branch_id: request.new_branch,
            prompt_id: request.prompt,
        },
    )
}

fn handle_inline_historical_editor_key(
    key: KeyEvent,
    app: &mut App,
    commands: &mpsc::UnboundedSender<WorkerCommand>,
) -> Result<Option<TerminalAction>> {
    if !app.historical_editor_active() {
        return Ok(None);
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => return Ok(Some(TerminalAction::Quit)),
            KeyCode::Char('g') => return Ok(Some(TerminalAction::ExternalEditor)),
            KeyCode::Char('d') => app.delete(),
            KeyCode::Char('h') => app.backspace(),
            KeyCode::Char('j') => app.insert_char('\n'),
            KeyCode::Char('a') => app.move_home(),
            KeyCode::Char('e') => app.move_end(),
            KeyCode::Char('b') => app.move_left(),
            KeyCode::Char('f') => app.move_right(),
            KeyCode::Char('p') => app.move_inline_editor_up(),
            KeyCode::Char('n') => app.move_inline_editor_down(),
            KeyCode::Char('w') => app.delete_word_before_cursor(),
            KeyCode::Char('u') => app.delete_to_line_start(),
            KeyCode::Char('k') => app.delete_to_line_end(),
            KeyCode::Left => app.move_word_left(),
            KeyCode::Right => app.move_word_right(),
            _ => {}
        }
        return Ok(Some(TerminalAction::Redraw));
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        match key.code {
            KeyCode::Enter => app.insert_char('\n'),
            KeyCode::Char('b') | KeyCode::Left => app.move_word_left(),
            KeyCode::Char('f') | KeyCode::Right => app.move_word_right(),
            _ => {}
        }
        return Ok(Some(TerminalAction::Redraw));
    }

    match key.code {
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => app.insert_char('\n'),
        KeyCode::Enter => request_historical_edit(app, commands)?,
        KeyCode::Char(character) => app.insert_char(character),
        KeyCode::Backspace => app.backspace(),
        KeyCode::Delete => app.delete(),
        KeyCode::Left => app.move_left(),
        KeyCode::Right => app.move_right(),
        KeyCode::Home => app.move_home(),
        KeyCode::End => app.move_end(),
        KeyCode::Up => app.move_inline_editor_up(),
        KeyCode::Down => app.move_inline_editor_down(),
        KeyCode::Esc => app.cancel_historical_edit(),
        _ => {}
    }
    Ok(Some(TerminalAction::Redraw))
}

async fn edit_in_external_editor(terminal: &mut TerminalSession, app: &mut App) -> Result<()> {
    let editor = match external_editor::resolve_editor_command() {
        Ok(editor) => editor,
        Err(error) => {
            app.editor_failed(error);
            return Ok(());
        }
    };

    terminal.suspend()?;
    let editor_result = external_editor::edit(&app.input, &editor, &app.cwd).await;
    let resume_result = terminal.resume();
    resume_result?;

    match editor_result {
        Ok(input) => app.replace_input(input.trim_end().to_owned()),
        Err(error) => app.editor_failed(error),
    }
    Ok(())
}

async fn run_external_editor(
    input_events: EventStream,
    terminal: &mut TerminalSession,
    app: &mut App,
) -> Result<EventStream> {
    drop(input_events);
    edit_in_external_editor(terminal, app).await?;
    Ok(EventStream::new())
}

fn submit(
    app: &mut App,
    root_session_id: &str,
    commands: &mpsc::UnboundedSender<WorkerCommand>,
    intent: SubmitIntent,
) -> Result<()> {
    let Some(input) = app.take_submission() else {
        return Ok(());
    };
    match classify_submission(input) {
        Submission::Prompt(prompt) => {
            let target = app.focus;
            if matches!(intent, SubmitIntent::Immediate) && app.is_running(target) {
                if let Some(id) = app.queue_steer(target, prompt.display().to_owned()) {
                    send_command(commands, WorkerCommand::Steer { target, id, prompt })?;
                }
            } else if let Some(prompt_id) = app.queue_prompt(target, prompt.display().to_owned()) {
                send_command(
                    commands,
                    WorkerCommand::Prompt {
                        target,
                        prompt_id,
                        prompt,
                    },
                )?;
            }
        }
        Submission::Btw(prompt) => {
            if let Some(id) = app.btw_id() {
                app.focus_btw();
                if let Some(prompt) = prompt {
                    let target = PaneId::Btw(id);
                    if let Some(prompt_id) = app.queue_prompt(target, prompt.display().to_owned()) {
                        send_command(
                            commands,
                            WorkerCommand::Prompt {
                                target,
                                prompt_id,
                                prompt,
                            },
                        )?;
                    }
                }
            } else {
                let id = app.begin_btw();
                let prompt_id = prompt.as_ref().and_then(|prompt| {
                    app.queue_prompt(PaneId::Btw(id), prompt.display().to_owned())
                });
                send_command(
                    commands,
                    WorkerCommand::OpenBtw {
                        id,
                        prompt_id,
                        prompt,
                    },
                )?;
            }
        }
        Submission::CloseBtw => {
            if let Some(id) = app.btw_id() {
                if app.btw_busy() {
                    app.reject_btw_close_while_busy();
                } else {
                    app.close_btw(id);
                    send_command(commands, WorkerCommand::CloseBtw { id })?;
                }
            }
        }
        Submission::Cancel => {
            let target = app.focus;
            app.cancel_pending(target);
            send_command(commands, WorkerCommand::Cancel { target })?;
        }
        Submission::Trace => {
            let Some(session_id) = active_session_id(app, root_session_id) else {
                app.push_active_error("BTW traces are available after the fork finishes");
                return Ok(());
            };
            match open_session_traces(session_id) {
                Ok(()) => app.set_active_status("Opened session traces in Jaeger"),
                Err(error) => app.push_active_error(format!("failed to open Jaeger: {error}")),
            }
        }
    }
    Ok(())
}

fn send_command(
    commands: &mpsc::UnboundedSender<WorkerCommand>,
    command: WorkerCommand,
) -> Result<()> {
    commands
        .send(command)
        .map_err(|_| eyre::eyre!("agent worker stopped"))
}

fn classify_submission(input: impl Into<SubmittedPrompt>) -> Submission {
    let mut input = input.into();
    let trimmed = input.display().trim();
    if trimmed == "/btw" {
        return Submission::Btw(None);
    }
    if let Some(prompt) = trimmed.strip_prefix("/btw ") {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Submission::Btw(None);
        }
        input.set_display(prompt.to_owned());
        return Submission::Btw(Some(input));
    }
    if trimmed == "/close" {
        return Submission::CloseBtw;
    }
    if trimmed == "/cancel" {
        return Submission::Cancel;
    }
    if trimmed == "/trace" {
        return Submission::Trace;
    }
    Submission::Prompt(input)
}

fn active_session_id<'a>(app: &'a App, root_session_id: &'a str) -> Option<&'a str> {
    match app.focus {
        PaneId::Main => app.main_branch_request_id().or(Some(root_session_id)),
        PaneId::Btw(id) => app
            .btw
            .as_ref()
            .filter(|btw| btw.id == id)
            .and_then(|btw| btw.request_id.as_deref()),
    }
}

fn session_trace_url(base_url: &str, session_id: &str) -> Result<reqwest::Url> {
    let base = reqwest::Url::parse(base_url).wrap_err("invalid Jaeger UI URL")?;
    let mut url = base.join("search").wrap_err("invalid Jaeger search URL")?;
    let tags = serde_json::json!({ "session.id": session_id }).to_string();
    url.query_pairs_mut()
        .append_pair("service", "nanocodex")
        .append_pair("lookback", "1w")
        .append_pair("limit", "1500")
        .append_pair("tags", &tags);
    Ok(url)
}

fn open_session_traces(session_id: &str) -> Result<()> {
    let base_url =
        std::env::var(JAEGER_UI_URL_ENV).unwrap_or_else(|_| DEFAULT_JAEGER_UI_URL.to_owned());
    let url = session_trace_url(&base_url, session_id)?;
    let mut command = browser_command(url.as_str());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .wrap_err("browser launcher failed")?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn browser_command(url: &str) -> Command {
    let mut command = Command::new("open");
    command.arg(url);
    command
}

#[cfg(target_os = "windows")]
fn browser_command(url: &str) -> Command {
    let mut command = Command::new("cmd");
    command.args(["/C", "start", "", url]);
    command
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn browser_command(url: &str) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(url);
    command
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc, time::Duration};

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
    use futures_util::{SinkExt, StreamExt};
    use nanocodex::{Nanocodex, Responses, Thinking};
    use serde_json::{Value, json};
    use tokio::{net::TcpListener, sync::mpsc, time::timeout};
    use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

    use super::{
        BTW_BOUNDARY, PaneId, RedrawPriority, Submission, TerminalAction, UiAction, UiModel,
        UiUpdate, WorkerCommand, WorkerEvent, active_session_id, classify_submission, handle_key,
        handle_worker_update, paste_clipboard_image, prepare_btw_prompt, session_trace_url,
        spawn_agent_worker,
    };
    use crate::tui::app::App;

    fn mouse_scroll(kind: MouseEventKind) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn parses_tui_commands_without_capturing_similar_prompts() {
        assert_eq!(
            classify_submission("/btw".to_owned()),
            Submission::Btw(None)
        );
        assert_eq!(
            classify_submission(" /btw   inspect the cache  ".to_owned()),
            Submission::Btw(Some("inspect the cache".into()))
        );
        assert_eq!(
            classify_submission("/close".to_owned()),
            Submission::CloseBtw
        );
        assert_eq!(
            classify_submission("/cancel".to_owned()),
            Submission::Cancel
        );
        assert_eq!(
            classify_submission(" /trace ".to_owned()),
            Submission::Trace
        );
        assert_eq!(
            classify_submission("/btw-not-a-command".to_owned()),
            Submission::Prompt("/btw-not-a-command".into())
        );
        assert_eq!(
            classify_submission("/trace-this".to_owned()),
            Submission::Prompt("/trace-this".into())
        );
    }

    #[test]
    fn clipboard_image_paste_attaches_the_materialized_image() {
        let mut app = App::new("/workspace".into());
        let path = PathBuf::from("/tmp/copied-image.png");

        paste_clipboard_image(&mut app, || Ok(path.clone()));

        assert_eq!(app.input, "[Image #1] ");
        let submission = app.take_submission().unwrap();
        let nanocodex::PromptInput::Content(content) = submission.into_prompt().instruction else {
            panic!("clipboard image should produce typed content");
        };
        assert!(matches!(
            &content[0],
            nanocodex::UserInput::LocalImage {
                path: submitted_path,
                detail: None,
            } if submitted_path == &path
        ));
    }

    #[test]
    fn control_end_jumps_the_focused_transcript_to_the_tail() {
        let (commands, _worker) = mpsc::unbounded_channel();
        let mut app = App::new("/workspace".into());
        let btw_id = app.begin_btw();
        app.main.scroll_from_bottom = 11;
        app.main.has_unseen_output = true;
        app.btw.as_mut().unwrap().conversation.scroll_from_bottom = 8;
        app.btw.as_mut().unwrap().conversation.has_unseen_output = true;

        let key = KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL);
        assert_eq!(
            handle_key(key, &mut app, "main-session", &commands).unwrap(),
            TerminalAction::Redraw
        );

        assert_eq!(app.main.scroll_from_bottom, 11);
        assert!(app.main.has_unseen_output);
        let btw = &app.btw.as_ref().unwrap().conversation;
        assert_eq!(btw.scroll_from_bottom, 0);
        assert!(!btw.has_unseen_output);
        assert_eq!(app.focus, PaneId::Btw(btw_id));
    }

    #[test]
    fn jaeger_search_targets_the_focused_session_and_encodes_its_tag() {
        let mut app = App::new("/workspace".into());
        assert_eq!(
            active_session_id(&app, "main-session"),
            Some("main-session")
        );

        let btw_id = app.begin_btw();
        assert_eq!(active_session_id(&app, "main-session"), None);
        app.btw_opened(btw_id, std::sync::Arc::from("btw session/&"));
        let session_id = active_session_id(&app, "main-session").unwrap();
        assert_eq!(session_id, "btw session/&");

        let url = session_trace_url("http://127.0.0.1:16686", session_id).unwrap();
        assert_eq!(url.path(), "/search");
        let query = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(query.get("service").map(AsRef::as_ref), Some("nanocodex"));
        assert_eq!(query.get("lookback").map(AsRef::as_ref), Some("1w"));
        assert_eq!(query.get("limit").map(AsRef::as_ref), Some("1500"));
        assert_eq!(
            query.get("tags").map(AsRef::as_ref),
            Some(r#"{"session.id":"btw session/&"}"#)
        );
    }

    #[test]
    fn side_boundary_wraps_only_the_first_btw_prompt() {
        let mut first = true;
        assert_eq!(
            prepare_btw_prompt(&mut first, "first".into()).display(),
            format!("{BTW_BOUNDARY}first")
        );
        assert_eq!(
            prepare_btw_prompt(&mut first, "follow-up".into()).display(),
            "follow-up"
        );
    }

    #[test]
    fn all_event_sources_cross_the_ui_action_boundary() {
        let (commands, _worker) = mpsc::unbounded_channel();
        let mut ui = UiModel::new(
            App::new("/workspace".into()),
            std::sync::Arc::from("main-session"),
        );

        assert_eq!(
            ui.update(UiAction::Terminal(Event::Resize(100, 40)), &commands)
                .unwrap(),
            UiUpdate::Redraw(RedrawPriority::Immediate)
        );
        assert_eq!(
            ui.update(UiAction::Tick, &commands).unwrap(),
            UiUpdate::Redraw(RedrawPriority::Streaming)
        );
        assert_eq!(
            ui.update(UiAction::WorkerStopped, &commands).unwrap(),
            UiUpdate::Redraw(RedrawPriority::Streaming)
        );
        assert!(!ui.worker_updates_open);
    }

    #[test]
    fn reversing_a_queued_mouse_scroll_discards_the_previous_direction() {
        let (commands, _worker) = mpsc::unbounded_channel();
        let mut app = App::new("/workspace".into());
        app.main.scroll_from_bottom = 15;
        let mut ui = UiModel::new(app, std::sync::Arc::from("main-session"));

        for _ in 0..4 {
            assert_eq!(
                ui.update(
                    UiAction::Terminal(mouse_scroll(MouseEventKind::ScrollUp)),
                    &commands,
                )
                .unwrap(),
                UiUpdate::Redraw(RedrawPriority::InputBurst)
            );
        }
        assert_eq!(ui.app.main.scroll_from_bottom, 15);

        ui.update(
            UiAction::Terminal(mouse_scroll(MouseEventKind::ScrollDown)),
            &commands,
        )
        .unwrap();
        ui.apply_pending_mouse_scroll();

        assert_eq!(
            ui.app.main.scroll_from_bottom, 12,
            "the reverse tick should replace, not unwind, the queued upward ticks",
        );
        assert!(ui.pending_mouse_scroll.is_none());
    }

    #[test]
    fn same_direction_mouse_scrolls_accumulate_until_the_frame() {
        let (commands, _worker) = mpsc::unbounded_channel();
        let mut ui = UiModel::new(
            App::new("/workspace".into()),
            std::sync::Arc::from("main-session"),
        );

        for _ in 0..3 {
            ui.update(
                UiAction::Terminal(mouse_scroll(MouseEventKind::ScrollUp)),
                &commands,
            )
            .unwrap();
        }
        ui.apply_pending_mouse_scroll();

        assert_eq!(ui.app.main.scroll_from_bottom, 9);
    }

    #[test]
    fn completion_notification_is_queued_only_while_unfocused() {
        let (commands, _worker) = mpsc::unbounded_channel();
        let mut ui = UiModel::new(
            App::new("/workspace".into()),
            std::sync::Arc::from("main-session"),
        );

        ui.update(UiAction::Terminal(Event::FocusLost), &commands)
            .unwrap();
        ui.update(
            UiAction::Worker(WorkerEvent::TurnFinished {
                target: PaneId::Main,
                main_branch_id: Some(0),
                error: None,
            }),
            &commands,
        )
        .unwrap();
        assert_eq!(
            ui.pending_notification.as_deref(),
            Some("Nanocodex finished")
        );

        ui.update(UiAction::Terminal(Event::FocusGained), &commands)
            .unwrap();
        assert!(ui.pending_notification.is_none());
        ui.update(
            UiAction::Worker(WorkerEvent::TurnFinished {
                target: PaneId::Main,
                main_branch_id: Some(0),
                error: None,
            }),
            &commands,
        )
        .unwrap();
        assert!(ui.pending_notification.is_none());
    }

    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn tui_worker_steer_becomes_a_user_item_at_the_next_model_boundary() -> eyre::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("ws://{}", listener.local_addr()?);
        let (first_seen, first_seen_rx) = tokio::sync::oneshot::channel();
        let (release_first, release_first_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut socket = accept_async(stream).await?;
            let warmup = next_ws_json(&mut socket).await?;
            assert_eq!(warmup["generate"], false);
            send_ws_json(
                &mut socket,
                json!({
                    "type": "response.completed",
                    "response": { "id": "resp-warmup", "usage": null }
                }),
            )
            .await?;

            let initial = next_ws_json(&mut socket).await?;
            assert_eq!(initial["previous_response_id"], "resp-warmup");
            assert!(initial.to_string().contains("initial task"));
            first_seen
                .send(())
                .map_err(|()| eyre::eyre!("initial request signal receiver dropped"))?;
            release_first_rx
                .await
                .map_err(|_| eyre::eyre!("initial request release sender dropped"))?;
            send_completed(&mut socket, "resp-initial", "initial draft").await?;

            let steered = next_ws_json(&mut socket).await?;
            assert_eq!(steered["previous_response_id"], "resp-initial");
            assert_eq!(steered["input"].as_array().map(Vec::len), Some(1));
            assert_eq!(steered["input"][0]["role"], "user");
            assert_eq!(
                steered["input"][0]["content"][0]["text"],
                "steering correction"
            );
            send_completed(&mut socket, "resp-steered", "steered answer").await
        });

        let workspace = temporary_workspace("tui-steer")?;
        let responses = Responses::builder().websocket_url(endpoint).build();
        let (agent, mut events) = Nanocodex::builder("test-key")
            .thinking(Thinking::Low)
            .workspace(&workspace)
            .responses(responses)
            .session_id("tui-steer-test")
            .build()?;
        let (commands, worker_rx) = mpsc::unbounded_channel();
        let (updates, mut update_rx) = mpsc::unbounded_channel();
        spawn_agent_worker(
            agent,
            std::sync::Arc::from("tui-steer-test"),
            worker_rx,
            updates,
        );

        commands.send(WorkerCommand::Prompt {
            target: PaneId::Main,
            prompt_id: 1,
            prompt: "initial task".into(),
        })?;
        first_seen_rx.await?;
        commands.send(WorkerCommand::Steer {
            target: PaneId::Main,
            id: 7,
            prompt: "steering correction".into(),
        })?;
        timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    update_rx.recv().await,
                    Some(WorkerEvent::SteerAdmitted {
                        target: PaneId::Main,
                        id: 7
                    })
                ) {
                    break;
                }
            }
        })
        .await
        .map_err(|_| eyre::eyre!("TUI worker did not acknowledge the steer"))?;
        release_first
            .send(())
            .map_err(|()| eyre::eyre!("initial request release receiver dropped"))?;

        timeout(Duration::from_secs(5), async {
            loop {
                let event = events
                    .recv()
                    .await
                    .ok_or_else(|| eyre::eyre!("agent events closed before run.steered"))?;
                if event.kind == nanocodex::AgentEventKind::RunSteered {
                    return eyre::Result::<()>::Ok(());
                }
            }
        })
        .await
        .map_err(|_| eyre::eyre!("steer did not reach the model boundary"))??;
        timeout(Duration::from_secs(5), server)
            .await
            .map_err(|_| eyre::eyre!("mock Responses server did not finish"))???;
        drop(commands);
        std::fs::remove_dir_all(workspace)?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn historical_edit_forks_before_the_selected_prompt_and_keeps_the_parent_branch()
    -> eyre::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("ws://{}", listener.local_addr()?);
        let (second_seen, second_seen_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut root = accept_async(stream).await?;
            let warmup = next_ws_json(&mut root).await?;
            assert_eq!(warmup["generate"], false);
            send_ws_json(
                &mut root,
                json!({
                    "type": "response.completed",
                    "response": { "id": "resp-warmup", "usage": null }
                }),
            )
            .await?;

            let first = next_ws_json(&mut root).await?;
            assert!(first.to_string().contains("first prompt"));
            send_completed(&mut root, "resp-first", "first answer").await?;
            let second = next_ws_json(&mut root).await?;
            assert_eq!(second["previous_response_id"], "resp-first");
            assert!(second.to_string().contains("second prompt"));
            second_seen
                .send(())
                .map_err(|()| eyre::eyre!("second-request signal receiver dropped"))?;

            let (stream, _) = listener.accept().await?;
            let mut branch = accept_async(stream).await?;
            let edited = next_ws_json(&mut branch).await?;
            assert_eq!(edited["previous_response_id"], "resp-first");
            assert_eq!(edited["input"].as_array().map(Vec::len), Some(1));
            assert_eq!(
                edited["input"][0]["content"][0]["text"],
                "revised second prompt"
            );
            send_completed(&mut branch, "resp-edited", "edited answer").await?;
            send_completed(&mut root, "resp-second", "second answer").await
        });

        let workspace = temporary_workspace("tui-historical-edit")?;
        let responses = Responses::builder().websocket_url(endpoint).build();
        let (agent, mut events) = Nanocodex::builder("test-key")
            .thinking(Thinking::Low)
            .workspace(&workspace)
            .responses(responses)
            .session_id("tui-historical-edit-test")
            .build()?;
        let event_drain = tokio::spawn(async move { while events.recv().await.is_some() {} });
        let (commands, worker_rx) = mpsc::unbounded_channel();
        let (updates, mut update_rx) = mpsc::unbounded_channel();
        spawn_agent_worker(
            agent,
            std::sync::Arc::from("tui-historical-edit-test"),
            worker_rx,
            updates,
        );

        commands.send(WorkerCommand::Prompt {
            target: PaneId::Main,
            prompt_id: 1,
            prompt: "first prompt".into(),
        })?;
        timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    update_rx.recv().await,
                    Some(WorkerEvent::TurnFinished {
                        target: PaneId::Main,
                        main_branch_id: Some(0),
                        error: None,
                    })
                ) {
                    break;
                }
            }
        })
        .await
        .map_err(|_| eyre::eyre!("first root turn did not finish"))?;

        commands.send(WorkerCommand::Prompt {
            target: PaneId::Main,
            prompt_id: 2,
            prompt: "second prompt".into(),
        })?;
        timeout(Duration::from_secs(5), second_seen_rx)
            .await
            .map_err(|_| eyre::eyre!("second root turn did not start"))??;

        commands.send(WorkerCommand::EditHistorical {
            source_branch_id: 0,
            new_branch_id: 1,
            prompt_id: 2,
        })?;
        timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    update_rx.recv().await,
                    Some(WorkerEvent::MainBranchOpened {
                        id: 1,
                        parent_id: 0,
                        prompt_id: 2,
                        ..
                    })
                ) {
                    break;
                }
            }
        })
        .await
        .map_err(|_| eyre::eyre!("historical branch did not open"))?;

        commands.send(WorkerCommand::Prompt {
            target: PaneId::Main,
            prompt_id: 3,
            prompt: "revised second prompt".into(),
        })?;
        timeout(Duration::from_secs(5), async {
            let mut parent_finished = false;
            let mut branch_finished = false;
            loop {
                match update_rx.recv().await {
                    Some(WorkerEvent::TurnFinished {
                        target: PaneId::Main,
                        main_branch_id: Some(0),
                        error: None,
                    }) => parent_finished = true,
                    Some(WorkerEvent::TurnFinished {
                        target: PaneId::Main,
                        main_branch_id: Some(1),
                        error: None,
                    }) => branch_finished = true,
                    _ => {}
                }
                if parent_finished && branch_finished {
                    break;
                }
            }
        })
        .await
        .map_err(|_| eyre::eyre!("concurrent parent and edited branch turns did not finish"))?;

        commands.send(WorkerCommand::SwitchMainBranch { id: 0 })?;
        timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    update_rx.recv().await,
                    Some(WorkerEvent::MainBranchSwitched { id: 0, .. })
                ) {
                    break;
                }
            }
        })
        .await
        .map_err(|_| eyre::eyre!("parent branch was not retained"))?;

        drop(commands);
        timeout(Duration::from_secs(5), server)
            .await
            .map_err(|_| eyre::eyre!("mock Responses server did not finish"))???;
        event_drain.abort();
        std::fs::remove_dir_all(workspace)?;
        Ok(())
    }

    #[test]
    fn second_escape_sends_cancel_for_the_focused_turn() {
        let (commands, mut worker) = mpsc::unbounded_channel();
        let mut app = App::new("/workspace".into());
        app.main.running = true;
        app.input = "preserved draft".to_owned();
        app.cursor = app.input.len();
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);

        assert_eq!(
            handle_key(escape, &mut app, "main-session", &commands).unwrap(),
            TerminalAction::Redraw
        );
        assert!(worker.try_recv().is_err());
        assert_eq!(
            handle_key(escape, &mut app, "main-session", &commands).unwrap(),
            TerminalAction::Redraw
        );
        assert!(matches!(
            worker.try_recv(),
            Ok(WorkerCommand::Cancel {
                target: super::PaneId::Main
            })
        ));
        assert_eq!(app.input, "preserved draft");
    }

    #[test]
    fn control_g_requests_the_external_editor_without_changing_the_draft() {
        let (commands, _worker) = mpsc::unbounded_channel();
        let mut app = App::new("/workspace".into());
        app.input = "multiline\ndraft".to_owned();
        app.cursor = 4;
        let key = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);

        assert_eq!(
            handle_key(key, &mut app, "main-session", &commands).unwrap(),
            TerminalAction::ExternalEditor
        );
        assert_eq!(app.input, "multiline\ndraft");
        assert_eq!(app.cursor, 4);
    }

    #[test]
    fn up_selects_a_just_submitted_prompt_before_worker_events_arrive() {
        let (commands, mut worker) = mpsc::unbounded_channel();
        let mut app = App::new("/workspace".into());
        app.input = "message with typo".to_owned();
        app.cursor = app.input.len();

        assert_eq!(
            handle_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut app,
                "main-session",
                &commands,
            )
            .unwrap(),
            TerminalAction::Redraw
        );
        assert!(matches!(
            worker.try_recv(),
            Ok(WorkerCommand::Prompt { prompt, .. }) if prompt == "message with typo"
        ));
        assert_eq!(
            handle_key(
                KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
                &mut app,
                "main-session",
                &commands,
            )
            .unwrap(),
            TerminalAction::Redraw
        );
        assert!(app.transcript_selection_active());
        assert!(app.start_historical_edit());
        assert_eq!(app.input, "message with typo");
    }

    #[test]
    fn e_edits_the_selected_prompt_inline_before_requesting_a_fork() {
        let (commands, mut worker) = mpsc::unbounded_channel();
        let mut app = App::new("/workspace".into());
        app.main
            .transcript
            .push_editable_user("earlier prompt".to_owned(), 17);
        app.input = "current draft".to_owned();

        assert_eq!(
            handle_key(
                KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
                &mut app,
                "main-session",
                &commands,
            )
            .unwrap(),
            TerminalAction::Redraw
        );
        assert!(app.transcript_selection_active());

        assert_eq!(
            handle_key(
                KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
                &mut app,
                "main-session",
                &commands,
            )
            .unwrap(),
            TerminalAction::Redraw
        );
        assert_eq!(app.input, "earlier prompt");
        assert!(app.historical_editor_active());
        assert!(worker.try_recv().is_err());

        app.replace_input("revised prompt".to_owned());
        assert_eq!(
            handle_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut app,
                "main-session",
                &commands,
            )
            .unwrap(),
            TerminalAction::Redraw
        );
        assert_eq!(app.input, "current draft");
        assert!(!app.historical_editor_active());
        assert!(matches!(
            worker.try_recv(),
            Ok(WorkerCommand::EditHistorical {
                source_branch_id: 0,
                new_branch_id: 1,
                prompt_id: 17,
            })
        ));
    }

    #[test]
    fn escape_cancels_inline_history_edit_and_restores_the_composer_draft() {
        let (commands, mut worker) = mpsc::unbounded_channel();
        let mut app = App::new("/workspace".into());
        app.main
            .transcript
            .push_editable_user("earlier prompt".to_owned(), 17);
        app.input = "preserved draft".to_owned();
        app.cursor = app.input.len();
        app.move_up();
        assert!(app.start_historical_edit());
        app.replace_input("discard this revision".to_owned());

        assert_eq!(
            handle_key(
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                &mut app,
                "main-session",
                &commands,
            )
            .unwrap(),
            TerminalAction::Redraw
        );
        assert_eq!(app.input, "preserved draft");
        assert_eq!(app.cursor, app.input.len());
        assert!(!app.historical_editor_active());
        assert!(!app.transcript_selection_active());
        assert!(worker.try_recv().is_err());
    }

    #[test]
    fn opened_historical_branch_submits_the_inline_revision() {
        let mut app = App::new("/workspace".into());
        app.main
            .transcript
            .push_editable_user("earlier prompt".to_owned(), 17);
        app.move_up();
        assert!(app.start_historical_edit());
        app.replace_input("revised prompt".to_owned());
        let request = app
            .commit_historical_edit()
            .expect("inline edit should request a branch");
        let mut ui = UiModel::new(app, Arc::from("root-session"));
        let (commands, mut worker) = mpsc::unbounded_channel();

        assert_eq!(
            ui.update(
                UiAction::Worker(WorkerEvent::MainBranchOpened {
                    id: request.new_branch,
                    parent_id: request.source_branch,
                    prompt_id: request.prompt,
                    request_id: Arc::from("branch-session"),
                }),
                &commands,
            )
            .unwrap(),
            UiUpdate::Redraw(RedrawPriority::Streaming)
        );
        assert!(matches!(
            worker.try_recv(),
            Ok(WorkerCommand::Prompt {
                target: PaneId::Main,
                prompt_id: 1,
                prompt,
            }) if prompt == "revised prompt"
        ));
        assert!(ui.app.input.is_empty());
        assert_eq!(ui.app.main_branch_graph(), "0 1*←0");
    }

    #[test]
    fn control_alt_arrows_request_branch_navigation() {
        let (commands, mut worker) = mpsc::unbounded_channel();
        let mut app = App::new("/workspace".into());
        app.main
            .transcript
            .push_editable_user("earlier prompt".to_owned(), 17);
        app.move_up();
        assert!(app.start_historical_edit());
        let request = app
            .commit_historical_edit()
            .expect("inline editor should commit");
        let _ = app.main_branch_opened(
            request.new_branch,
            request.source_branch,
            request.prompt,
            std::sync::Arc::from("branch-session"),
        );

        let key = KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL | KeyModifiers::ALT);
        assert_eq!(
            handle_key(key, &mut app, "root-session", &commands).unwrap(),
            TerminalAction::Redraw
        );
        assert!(matches!(
            worker.try_recv(),
            Ok(WorkerCommand::SwitchMainBranch { id: 0 })
        ));
    }

    #[test]
    fn branch_navigator_switches_as_selection_moves() -> eyre::Result<()> {
        let (commands, mut worker) = mpsc::unbounded_channel();
        let mut app = App::new("/workspace".into());
        app.main
            .transcript
            .push_editable_user("root prompt".to_owned(), 17);
        app.move_up();
        assert!(app.start_historical_edit());
        app.replace_input("branch prompt".to_owned());
        let request = app.commit_historical_edit().unwrap();
        let _ = app.main_branch_opened(
            request.new_branch,
            request.source_branch,
            request.prompt,
            Arc::from("branch-session"),
        );
        app.main
            .transcript
            .push_editable_user("branch prompt".to_owned(), 18);

        assert_eq!(
            handle_key(
                KeyEvent::new(
                    KeyCode::Char('b'),
                    KeyModifiers::CONTROL | KeyModifiers::ALT,
                ),
                &mut app,
                "root-session",
                &commands,
            )
            .unwrap(),
            TerminalAction::Redraw
        );
        assert!(app.branch_navigator_active());
        assert!(worker.try_recv().is_err());

        let _ = handle_key(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &mut app,
            "root-session",
            &commands,
        )?;
        assert_eq!(
            app.branch_previews()
                .into_iter()
                .find(|preview| preview.selected)
                .map(|preview| preview.id),
            Some(0)
        );
        assert!(matches!(
            worker.try_recv(),
            Ok(WorkerCommand::SwitchMainBranch { id: 0 })
        ));

        let _ = handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &mut app,
            "root-session",
            &commands,
        )?;
        assert!(worker.try_recv().is_err());
        handle_worker_update(
            &mut app,
            WorkerEvent::MainBranchSwitched {
                id: 0,
                request_id: Arc::from("root-session"),
            },
            &commands,
        )?;
        assert!(matches!(
            worker.try_recv(),
            Ok(WorkerCommand::SwitchMainBranch { id: 1 })
        ));
        Ok(())
    }

    #[test]
    fn readline_control_keys_are_dispatched_to_the_composer() {
        let (commands, _worker) = mpsc::unbounded_channel();
        let mut app = App::new("/workspace".into());
        app.input = "one two".to_owned();
        app.cursor = app.input.len();

        for character in ['w', 'a', 'k'] {
            let key = KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL);
            assert_eq!(
                handle_key(key, &mut app, "main-session", &commands).unwrap(),
                TerminalAction::Redraw
            );
        }

        assert!(app.input.is_empty());
        assert_eq!(app.cursor, 0);
    }

    async fn next_ws_json<S>(socket: &mut WebSocketStream<S>) -> eyre::Result<Value>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        loop {
            let message = socket
                .next()
                .await
                .ok_or_else(|| eyre::eyre!("client closed before sending a request"))??;
            if let Message::Text(text) = message {
                return Ok(serde_json::from_str(text.as_str())?);
            }
        }
    }

    async fn send_completed<S>(
        socket: &mut WebSocketStream<S>,
        response_id: &str,
        text: &str,
    ) -> eyre::Result<()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        send_ws_json(
            socket,
            json!({
                "type": "response.completed",
                "response": {
                    "id": response_id,
                    "status": "completed",
                    "output": [{
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }]
                    }],
                    "usage": null
                }
            }),
        )
        .await
    }

    async fn send_ws_json<S>(socket: &mut WebSocketStream<S>, value: Value) -> eyre::Result<()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        socket.send(Message::Text(value.to_string().into())).await?;
        Ok(())
    }

    fn temporary_workspace(label: &str) -> eyre::Result<PathBuf> {
        let path = std::env::temp_dir().join(format!(
            "nanocodex-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }
}
