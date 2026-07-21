mod app;
mod scheduler;
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
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use eyre::{Result, WrapErr};
use futures_util::StreamExt;
use nanocodex::{AgentEvent, AgentEvents, Nanocodex, NanocodexError, TurnControl};
use tokio::{
    sync::mpsc,
    time::{MissedTickBehavior, interval, sleep_until},
};
use tracing::{Instrument, info_span};

use self::{
    app::{App, PaneId},
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

enum WorkerCommand {
    Prompt {
        target: PaneId,
        prompt: String,
    },
    Steer {
        target: PaneId,
        id: u64,
        prompt: String,
    },
    Cancel {
        target: PaneId,
    },
    OpenBtw {
        id: u64,
        prompt: Option<String>,
    },
    CloseBtw {
        id: u64,
    },
}

enum WorkerEvent {
    TurnFinished {
        target: PaneId,
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
        event: AgentEvent,
    },
    BtwEventStreamClosed {
        id: u64,
    },
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
    control: TurnControl,
    span: tracing::Span,
}

struct SteerRequest {
    id: u64,
    prompt: String,
}

#[derive(Clone, Copy)]
struct TurnTarget<'a> {
    session_id: &'a str,
    pane: PaneId,
}

impl BtwWorker {
    fn prepare_prompt(&mut self, prompt: String) -> String {
        prepare_btw_prompt(&mut self.first_prompt, prompt)
    }
}

fn prepare_btw_prompt(first_prompt: &mut bool, prompt: String) -> String {
    if *first_prompt {
        *first_prompt = false;
        format!("{BTW_BOUNDARY}{prompt}")
    } else {
        prompt
    }
}

enum TerminalAction {
    Redraw,
    Ignore,
    Quit,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UiUpdate {
    Redraw(RedrawPriority),
    Ignore,
    Quit,
}

struct UiModel {
    app: App,
    root_session_id: Arc<str>,
    agent_events_open: bool,
    worker_updates_open: bool,
}

impl UiModel {
    fn new(app: App, root_session_id: Arc<str>) -> Self {
        Self {
            app,
            root_session_id,
            agent_events_open: true,
            worker_updates_open: true,
        }
    }

    fn update(
        &mut self,
        action: UiAction,
        commands: &mpsc::UnboundedSender<WorkerCommand>,
    ) -> Result<UiUpdate> {
        match action {
            UiAction::Terminal(event) => {
                match handle_terminal_event(event, &mut self.app, &self.root_session_id, commands)?
                {
                    TerminalAction::Redraw => Ok(UiUpdate::Redraw(RedrawPriority::Immediate)),
                    TerminalAction::Ignore => Ok(UiUpdate::Ignore),
                    TerminalAction::Quit => Ok(UiUpdate::Quit),
                }
            }
            UiAction::Agent(event) => {
                if self.app.on_agent_event(PaneId::Main, &event) {
                    Ok(UiUpdate::Redraw(RedrawPriority::Streaming))
                } else {
                    Ok(UiUpdate::Ignore)
                }
            }
            UiAction::AgentStreamClosed => {
                self.app.main.transcript.push(TranscriptItem::Error(
                    "agent event stream closed".to_owned(),
                ));
                self.app.main.running = false;
                "Agent stopped".clone_into(&mut self.app.main.status);
                self.agent_events_open = false;
                Ok(UiUpdate::Redraw(RedrawPriority::Streaming))
            }
            UiAction::Worker(update) => {
                handle_worker_update(&mut self.app, update);
                Ok(UiUpdate::Redraw(RedrawPriority::Streaming))
            }
            UiAction::WorkerStopped => {
                self.app
                    .main
                    .transcript
                    .push(TranscriptItem::Error("agent worker stopped".to_owned()));
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
    Prompt(String),
    Btw(Option<String>),
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
    let thinking = configured.thinking;
    let agent = configured.handle;
    let mut agent_events = configured.events;
    let root_session_id = Arc::<str>::from(agent_events.request_id());
    let _child_agents = configured.child_agents;
    let (worker_tx, worker_rx) = mpsc::unbounded_channel();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    spawn_agent_worker(agent, Arc::clone(&root_session_id), worker_rx, update_tx);

    let mut terminal = TerminalSession::enter().wrap_err("failed to initialize the terminal")?;
    let mut input_events = EventStream::new();
    let mut ticker = interval(Duration::from_millis(80));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut ui = UiModel::new(App::new(cwd, thinking), Arc::clone(&root_session_id));
    let mut scheduler = RenderScheduler::new(STREAM_FRAME_INTERVAL, Instant::now());
    let mut stream_telemetry = StreamTelemetry::default();
    let mut view_telemetry = ViewTelemetry::new(Arc::clone(&root_session_id));

    submit_initial_prompt(&mut ui.app, &root_session_id, &worker_tx, initial_prompt)?;

    loop {
        view_telemetry.observe(&ui.app);
        let now = Instant::now();
        if scheduler.is_due(now) {
            let render_started = Instant::now();
            terminal.draw(|frame| view::render(frame, &ui.app))?;
            let presented_at = Instant::now();
            scheduler.presented(presented_at);
            stream_telemetry.frame_presented(render_started, presented_at, &ui.app);
        }

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
                if apply_update(
                    ui.update(UiAction::Terminal(event), &worker_tx)?,
                    &mut scheduler,
                ) {
                    return Ok(());
                }
            }
            event = agent_events.recv(), if ui.agent_events_open => {
                let received = event
                    .as_ref()
                    .and_then(|event| StreamTelemetry::event_received(PaneId::Main, event));
                let action = event.map_or(UiAction::AgentStreamClosed, UiAction::Agent);
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
                let received = update.as_ref().and_then(|update| match update {
                    WorkerEvent::BtwAgentEvent { id, event } => {
                        StreamTelemetry::event_received(PaneId::Btw(*id), event)
                    }
                    _ => None,
                });
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
        UiUpdate::Ignore => {}
        UiUpdate::Quit => return true,
    }
    false
}

fn handle_worker_update(app: &mut App, update: WorkerEvent) {
    match update {
        WorkerEvent::TurnFinished { target, error } => app.turn_finished(target, error),
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
            let _ = app.on_agent_event(PaneId::Btw(id), &event);
        }
        WorkerEvent::BtwEventStreamClosed { id } => {
            if app.btw_id() == Some(id) {
                app.btw_failed(id, "BTW event stream closed".to_owned());
            }
        }
    }
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
            root,
            root_session_id,
            main_turns: VecDeque::new(),
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
    root: Nanocodex,
    root_session_id: Arc<str>,
    main_turns: VecDeque<TrackedTurn>,
    next_turn_id: u64,
    btw: Option<BtwWorker>,
    finished: mpsc::UnboundedSender<FinishedTurn>,
    updates: mpsc::UnboundedSender<WorkerEvent>,
}

impl AgentWorker {
    async fn handle_command(&mut self, command: WorkerCommand) {
        match command {
            WorkerCommand::Prompt { target, prompt } => self.prompt(target, prompt).await,
            WorkerCommand::Steer { target, id, prompt } => self.steer(target, id, prompt).await,
            WorkerCommand::Cancel { target } => self.cancel(target).await,
            WorkerCommand::OpenBtw { id, prompt } => self.open_btw(id, prompt).await,
            WorkerCommand::CloseBtw { id } => {
                if self.btw.as_ref().is_some_and(|branch| branch.id == id) {
                    self.btw = None;
                }
            }
        }
    }

    async fn prompt(&mut self, target: PaneId, prompt: String) {
        match target {
            PaneId::Main => {
                if let Some(turn) = start_turn(
                    &self.root,
                    TurnTarget {
                        session_id: &self.root_session_id,
                        pane: target,
                    },
                    prompt,
                    &mut self.next_turn_id,
                    &self.finished,
                    &self.updates,
                )
                .await
                {
                    self.main_turns.push_back(turn);
                }
            }
            PaneId::Btw(id) => {
                let Some(branch) = self.btw.as_mut().filter(|branch| branch.id == id) else {
                    drop(self.updates.send(WorkerEvent::TurnFinished {
                        target,
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
                    },
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

    async fn steer(&mut self, target: PaneId, steer_id: u64, prompt: String) {
        let turn = match target {
            PaneId::Main => {
                steer_turn(
                    &self.root,
                    &self.main_turns,
                    TurnTarget {
                        session_id: &self.root_session_id,
                        pane: target,
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
                PaneId::Main => self.main_turns.push_back(turn),
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
            PaneId::Main => (Some(&self.main_turns), self.root_session_id.as_ref()),
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

    async fn open_btw(&mut self, id: u64, prompt: Option<String>) {
        self.btw = None;
        let span = info_span!(
            target: "nanocodex",
            parent: None,
            "tui.btw.open",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            session.id = self.root_session_id.as_ref(),
            tui.btw.id = id,
            tui.btw.session_id = tracing::field::Empty,
            status = tracing::field::Empty,
        );
        match self.root.fork().instrument(span.clone()).await {
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
                    if let Some(turn) = start_turn(
                        &branch.agent,
                        TurnTarget {
                            session_id: &branch.request_id,
                            pane: PaneId::Btw(id),
                        },
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

    fn finish_turn(&mut self, finished: FinishedTurn) {
        match finished.target {
            PaneId::Main => remove_finished(&mut self.main_turns, finished.id),
            PaneId::Btw(id) => {
                if let Some(branch) = self.btw.as_mut().filter(|branch| branch.id == id) {
                    remove_finished(&mut branch.turns, finished.id);
                }
            }
        }
        drop(self.updates.send(WorkerEvent::TurnFinished {
            target: finished.target,
            error: finished.error,
        }));
    }
}

async fn start_turn(
    agent: &Nanocodex,
    target: TurnTarget<'_>,
    prompt: String,
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
    match agent.prompt(prompt).instrument(span.clone()).await {
        Ok(turn) => {
            *next_turn_id = next_turn_id.saturating_add(1);
            let control = turn.control();
            let finished = finished.clone();
            let task_span = span.clone();
            tokio::spawn(
                async move {
                    let (error, status, otel_status) = match turn.result().await {
                        Ok(_) => (None, "completed", "OK"),
                        Err(NanocodexError::TurnCancelled) => (None, "cancelled", "ERROR"),
                        Err(error) => (Some(error.to_string()), "failed", "ERROR"),
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
                        error,
                    }));
                }
                .instrument(span.clone()),
            );
            Some(TrackedTurn { id, control, span })
        }
        Err(error) => {
            span.record("status", "rejected");
            span.record("otel.status_code", "ERROR");
            span.record(
                "duration_ns",
                telemetry::elapsed_ns(started_at, Instant::now()),
            );
            drop(updates.send(WorkerEvent::TurnFinished {
                target: target.pane,
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
            .steer(request.prompt.clone())
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
        prompt: request.prompt.clone(),
    }));
    start_turn(
        agent,
        target,
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
        while let Some(event) = events.recv().await {
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

fn handle_terminal_event(
    event: Event,
    app: &mut App,
    root_session_id: &str,
    commands: &mpsc::UnboundedSender<WorkerCommand>,
) -> Result<TerminalAction> {
    match event {
        Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            if handle_key(key, app, root_session_id, commands)? {
                Ok(TerminalAction::Quit)
            } else {
                Ok(TerminalAction::Redraw)
            }
        }
        Event::Paste(text) => {
            app.insert_str(&text.replace("\r\n", "\n").replace('\r', "\n"));
            Ok(TerminalAction::Redraw)
        }
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => {
                app.scroll_up(3);
                Ok(TerminalAction::Redraw)
            }
            MouseEventKind::ScrollDown => {
                app.scroll_down(3);
                Ok(TerminalAction::Redraw)
            }
            _ => Ok(TerminalAction::Ignore),
        },
        Event::Resize(_, _) => Ok(TerminalAction::Redraw),
        Event::FocusGained | Event::FocusLost | Event::Key(_) => Ok(TerminalAction::Ignore),
    }
}

fn handle_key(
    key: KeyEvent,
    app: &mut App,
    root_session_id: &str,
    commands: &mpsc::UnboundedSender<WorkerCommand>,
) -> Result<bool> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => return Ok(true),
            KeyCode::Char('d') if app.input.is_empty() => return Ok(true),
            KeyCode::Char('j') => app.insert_char('\n'),
            KeyCode::Char('a') => app.move_home(),
            KeyCode::Char('e') => app.move_end(),
            KeyCode::Char('p') => app.previous_history(),
            KeyCode::Char('n') => app.next_history(),
            _ => {}
        }
        return Ok(false);
    }

    if key.modifiers.contains(KeyModifiers::SUPER) {
        match key.code {
            KeyCode::Left => app.move_home(),
            KeyCode::Right => app.move_end(),
            KeyCode::Up => app.move_input_start(),
            KeyCode::Down => app.move_input_end(),
            KeyCode::Backspace => app.backspace_to_line_start(),
            KeyCode::Delete => app.delete_to_line_end(),
            _ => {}
        }
        return Ok(false);
    }

    if key.modifiers.contains(KeyModifiers::ALT) {
        match key.code {
            KeyCode::Left => {
                app.move_word_left();
                return Ok(false);
            }
            KeyCode::Right => {
                app.move_word_right();
                return Ok(false);
            }
            KeyCode::Backspace => {
                app.backspace_word();
                return Ok(false);
            }
            KeyCode::Delete => {
                app.delete_word();
                return Ok(false);
            }
            _ => {}
        }
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
        KeyCode::Up => app.previous_history(),
        KeyCode::Down => app.next_history(),
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
    Ok(false)
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
                if let Some(id) = app.queue_steer(target, prompt.clone()) {
                    send_command(commands, WorkerCommand::Steer { target, id, prompt })?;
                }
            } else if app.queue_prompt(target, prompt.clone()) {
                send_command(commands, WorkerCommand::Prompt { target, prompt })?;
            }
        }
        Submission::Btw(prompt) => {
            if let Some(id) = app.btw_id() {
                app.focus_btw();
                if let Some(prompt) = prompt {
                    let target = PaneId::Btw(id);
                    if app.queue_prompt(target, prompt.clone()) {
                        send_command(commands, WorkerCommand::Prompt { target, prompt })?;
                    }
                }
            } else {
                let id = app.begin_btw();
                if let Some(prompt) = prompt.as_ref() {
                    let _ = app.queue_prompt(PaneId::Btw(id), prompt.clone());
                }
                send_command(commands, WorkerCommand::OpenBtw { id, prompt })?;
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

fn classify_submission(input: String) -> Submission {
    let trimmed = input.trim();
    if trimmed == "/btw" {
        return Submission::Btw(None);
    }
    if let Some(prompt) = trimmed.strip_prefix("/btw ") {
        let prompt = prompt.trim();
        return Submission::Btw((!prompt.is_empty()).then(|| prompt.to_owned()));
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
        PaneId::Main => Some(root_session_id),
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
    use std::{path::PathBuf, time::Duration};

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use futures_util::{SinkExt, StreamExt};
    use nanocodex::{Nanocodex, Responses, Thinking};
    use serde_json::{Value, json};
    use tokio::{net::TcpListener, sync::mpsc, time::timeout};
    use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

    use super::{
        BTW_BOUNDARY, PaneId, RedrawPriority, Submission, UiAction, UiModel, UiUpdate,
        WorkerCommand, WorkerEvent, active_session_id, classify_submission, handle_key,
        prepare_btw_prompt, session_trace_url, spawn_agent_worker,
    };
    use crate::tui::app::App;

    #[test]
    fn parses_tui_commands_without_capturing_similar_prompts() {
        assert_eq!(
            classify_submission("/btw".to_owned()),
            Submission::Btw(None)
        );
        assert_eq!(
            classify_submission(" /btw   inspect the cache  ".to_owned()),
            Submission::Btw(Some("inspect the cache".to_owned()))
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
            Submission::Prompt("/btw-not-a-command".to_owned())
        );
        assert_eq!(
            classify_submission("/trace-this".to_owned()),
            Submission::Prompt("/trace-this".to_owned())
        );
    }

    #[test]
    fn jaeger_search_targets_the_focused_session_and_encodes_its_tag() {
        let mut app = App::new("/workspace".into(), Thinking::Medium);
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
            prepare_btw_prompt(&mut first, "first".to_owned()),
            format!("{BTW_BOUNDARY}first")
        );
        assert_eq!(
            prepare_btw_prompt(&mut first, "follow-up".to_owned()),
            "follow-up"
        );
    }

    #[test]
    fn all_event_sources_cross_the_ui_action_boundary() {
        let (commands, _worker) = mpsc::unbounded_channel();
        let mut ui = UiModel::new(
            App::new("/workspace".into(), Thinking::Medium),
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
            prompt: "initial task".to_owned(),
        })?;
        first_seen_rx.await?;
        commands.send(WorkerCommand::Steer {
            target: PaneId::Main,
            id: 7,
            prompt: "steering correction".to_owned(),
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

    #[test]
    fn second_escape_sends_cancel_for_the_focused_turn() {
        let (commands, mut worker) = mpsc::unbounded_channel();
        let mut app = App::new("/workspace".into(), Thinking::Medium);
        app.main.running = true;
        app.input = "preserved draft".to_owned();
        app.cursor = app.input.len();
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);

        assert!(!handle_key(escape, &mut app, "main-session", &commands).unwrap());
        assert!(worker.try_recv().is_err());
        assert!(!handle_key(escape, &mut app, "main-session", &commands).unwrap());
        assert!(matches!(
            worker.try_recv(),
            Ok(WorkerCommand::Cancel {
                target: super::PaneId::Main
            })
        ));
        assert_eq!(app.input, "preserved draft");
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
    #[test]
    fn alt_and_command_edit_the_composer_by_word_line_and_draft() {
        let (commands, _worker) = mpsc::unbounded_channel();
        let mut app = App::new("/workspace".into(), Thinking::Medium);
        app.input = "one two\nthree four".to_owned();
        app.cursor = app.input.len();

        let alt_left = KeyEvent::new(KeyCode::Left, KeyModifiers::ALT);
        let alt_backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT);
        let command_left = KeyEvent::new(KeyCode::Left, KeyModifiers::SUPER);
        let command_delete = KeyEvent::new(KeyCode::Delete, KeyModifiers::SUPER);
        let command_up = KeyEvent::new(KeyCode::Up, KeyModifiers::SUPER);
        let command_down = KeyEvent::new(KeyCode::Down, KeyModifiers::SUPER);

        assert!(!handle_key(alt_left, &mut app, "main-session", &commands).unwrap());
        assert_eq!(&app.input[app.cursor..], "four");

        assert!(!handle_key(alt_backspace, &mut app, "main-session", &commands).unwrap());
        assert_eq!(app.input, "one two\nfour");

        assert!(!handle_key(command_left, &mut app, "main-session", &commands).unwrap());
        assert_eq!(app.cursor, "one two\n".len());

        assert!(!handle_key(command_delete, &mut app, "main-session", &commands).unwrap());
        assert_eq!(app.input, "one two\n");

        assert!(!handle_key(command_up, &mut app, "main-session", &commands).unwrap());
        assert_eq!(app.cursor, 0);
        assert!(!handle_key(command_down, &mut app, "main-session", &commands).unwrap());
        assert_eq!(app.cursor, app.input.len());

        app.input = "one two\nthree four".to_owned();
        app.cursor = "one two\n".len();
        let alt_right = KeyEvent::new(KeyCode::Right, KeyModifiers::ALT);
        let alt_delete = KeyEvent::new(KeyCode::Delete, KeyModifiers::ALT);
        let command_right = KeyEvent::new(KeyCode::Right, KeyModifiers::SUPER);
        let command_backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::SUPER);

        assert!(!handle_key(alt_right, &mut app, "main-session", &commands).unwrap());
        assert_eq!(&app.input[..app.cursor], "one two\nthree");
        assert!(!handle_key(alt_delete, &mut app, "main-session", &commands).unwrap());
        assert_eq!(app.input, "one two\nthree");

        assert!(!handle_key(command_left, &mut app, "main-session", &commands).unwrap());
        assert!(!handle_key(command_right, &mut app, "main-session", &commands).unwrap());
        assert_eq!(app.cursor, app.input.len());
        assert!(!handle_key(command_backspace, &mut app, "main-session", &commands).unwrap());
        assert_eq!(app.input, "one two\n");

        let command_character = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::SUPER);
        assert!(!handle_key(command_character, &mut app, "main-session", &commands).unwrap());
        assert_eq!(app.input, "one two\n");
    }
}
