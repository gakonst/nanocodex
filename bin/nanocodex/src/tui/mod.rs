mod app;
mod scheduler;
mod telemetry;
mod terminal;
mod transcript;
mod view;

use std::{
    collections::VecDeque,
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

use self::{
    app::{App, PaneId},
    scheduler::{RenderScheduler, STREAM_FRAME_INTERVAL},
    telemetry::StreamTelemetry,
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

enum WorkerCommand {
    Prompt { target: PaneId, prompt: String },
    Steer { target: PaneId, prompt: String },
    Cancel { target: PaneId },
    OpenBtw { id: u64, prompt: Option<String> },
    CloseBtw { id: u64 },
}

enum WorkerEvent {
    TurnFinished {
        target: PaneId,
        error: Option<String>,
    },
    SteerAccepted {
        target: PaneId,
        prompt: String,
    },
    SteerQueued {
        target: PaneId,
        prompt: String,
    },
    SteerFailed {
        target: PaneId,
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
    agent: Nanocodex,
    first_prompt: bool,
    turns: VecDeque<TrackedTurn>,
}

struct TrackedTurn {
    id: u64,
    control: TurnControl,
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
    agent_events_open: bool,
    worker_updates_open: bool,
}

impl UiModel {
    fn new(app: App) -> Self {
        Self {
            app,
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
                match handle_terminal_event(event, &mut self.app, commands)? {
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
}

pub(crate) async fn run(config: AgentArgs, initial_prompt: Option<String>) -> Result<()> {
    let cwd = config
        .cwd()
        .canonicalize()
        .wrap_err("failed to resolve the working directory")?;
    let configured = config.build()?;
    let agent = configured.handle;
    let mut agent_events = configured.events;
    let _child_agents = configured.child_agents;
    let (worker_tx, worker_rx) = mpsc::unbounded_channel();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    spawn_agent_worker(agent, worker_rx, update_tx);

    let mut terminal = TerminalSession::enter().wrap_err("failed to initialize the terminal")?;
    let mut input_events = EventStream::new();
    let mut ticker = interval(Duration::from_millis(80));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut ui = UiModel::new(App::new(cwd));
    let mut scheduler = RenderScheduler::new(STREAM_FRAME_INTERVAL, Instant::now());
    let mut stream_telemetry = StreamTelemetry::default();

    if let Some(prompt) = initial_prompt {
        ui.app.input = prompt;
        ui.app.cursor = ui.app.input.len();
        submit(&mut ui.app, &worker_tx, SubmitIntent::Immediate)?;
    }

    loop {
        let now = Instant::now();
        if scheduler.is_due(now) {
            let render_started = Instant::now();
            terminal.draw(|frame| view::render(frame, &ui.app))?;
            let presented_at = Instant::now();
            scheduler.presented(presented_at);
            stream_telemetry.frame_presented(render_started, presented_at);
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
                let received = event.as_ref().and_then(StreamTelemetry::event_received);
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
                    WorkerEvent::BtwAgentEvent { event, .. } => {
                        StreamTelemetry::event_received(event)
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
        WorkerEvent::SteerAccepted { target, prompt } => app.steer_accepted(target, prompt),
        WorkerEvent::SteerQueued { target, prompt } => app.steer_queued(target, prompt),
        WorkerEvent::SteerFailed { target, error } => app.steer_failed(target, error),
        WorkerEvent::CancelAccepted { target } => app.cancel_accepted(target),
        WorkerEvent::CancelFailed { target, error } => app.cancel_failed(target, error),
        WorkerEvent::BtwOpened { id } => app.btw_opened(id),
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
    mut commands: mpsc::UnboundedReceiver<WorkerCommand>,
    updates: mpsc::UnboundedSender<WorkerEvent>,
) {
    tokio::spawn(async move {
        let (finished_tx, mut finished_rx) = mpsc::unbounded_channel::<FinishedTurn>();
        let mut worker = AgentWorker {
            root,
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
            WorkerCommand::Steer { target, prompt } => self.steer(target, prompt).await,
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
                    target,
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
                    target,
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

    async fn steer(&mut self, target: PaneId, prompt: String) {
        let turn = match target {
            PaneId::Main => {
                steer_turn(
                    &self.root,
                    &self.main_turns,
                    target,
                    prompt,
                    &mut self.next_turn_id,
                    &self.finished,
                    &self.updates,
                )
                .await
            }
            PaneId::Btw(id) => {
                let Some(branch) = self.btw.as_mut().filter(|branch| branch.id == id) else {
                    drop(self.updates.send(WorkerEvent::SteerFailed {
                        target,
                        error: "BTW branch is not available".to_owned(),
                    }));
                    return;
                };
                steer_turn(
                    &branch.agent,
                    &branch.turns,
                    target,
                    prompt,
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
                PaneId::Btw(id) => {
                    if let Some(branch) = self.btw.as_mut().filter(|branch| branch.id == id) {
                        branch.turns.push_back(turn);
                    }
                }
            }
        }
    }

    async fn cancel(&self, target: PaneId) {
        let turns = match target {
            PaneId::Main => Some(&self.main_turns),
            PaneId::Btw(id) => self
                .btw
                .as_ref()
                .filter(|branch| branch.id == id)
                .map(|branch| &branch.turns),
        };
        cancel_turn(turns, target, &self.updates).await;
    }

    async fn open_btw(&mut self, id: u64, prompt: Option<String>) {
        self.btw = None;
        match self.root.fork().await {
            Ok((agent, events)) => {
                forward_btw_events(id, events, self.updates.clone());
                drop(self.updates.send(WorkerEvent::BtwOpened { id }));
                let mut branch = BtwWorker {
                    id,
                    agent,
                    first_prompt: true,
                    turns: VecDeque::new(),
                };
                if let Some(prompt) = prompt {
                    let prompt = branch.prepare_prompt(prompt);
                    if let Some(turn) = start_turn(
                        &branch.agent,
                        PaneId::Btw(id),
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
    target: PaneId,
    prompt: String,
    next_turn_id: &mut u64,
    finished: &mpsc::UnboundedSender<FinishedTurn>,
    updates: &mpsc::UnboundedSender<WorkerEvent>,
) -> Option<TrackedTurn> {
    match agent.prompt(prompt).await {
        Ok(turn) => {
            let id = *next_turn_id;
            *next_turn_id = next_turn_id.saturating_add(1);
            let control = turn.control();
            let finished = finished.clone();
            tokio::spawn(async move {
                let error = match turn.result().await {
                    Ok(_) | Err(NanocodexError::TurnCancelled) => None,
                    Err(error) => Some(error.to_string()),
                };
                drop(finished.send(FinishedTurn { id, target, error }));
            });
            Some(TrackedTurn { id, control })
        }
        Err(error) => {
            drop(updates.send(WorkerEvent::TurnFinished {
                target,
                error: Some(error.to_string()),
            }));
            None
        }
    }
}

async fn steer_turn(
    agent: &Nanocodex,
    turns: &VecDeque<TrackedTurn>,
    target: PaneId,
    prompt: String,
    next_turn_id: &mut u64,
    finished: &mpsc::UnboundedSender<FinishedTurn>,
    updates: &mpsc::UnboundedSender<WorkerEvent>,
) -> Option<TrackedTurn> {
    for turn in turns {
        match turn.control.steer(prompt.clone()).await {
            Ok(()) => {
                drop(updates.send(WorkerEvent::SteerAccepted { target, prompt }));
                return None;
            }
            Err(NanocodexError::TurnNotSteerable) => {}
            Err(error) => {
                drop(updates.send(WorkerEvent::SteerFailed {
                    target,
                    error: error.to_string(),
                }));
                return None;
            }
        }
    }
    // Completion delivery can lag behind the driver's exact active-turn
    // state. If no retained capability is active, preserve this as a new turn.
    drop(updates.send(WorkerEvent::SteerQueued {
        target,
        prompt: prompt.clone(),
    }));
    start_turn(agent, target, prompt, next_turn_id, finished, updates).await
}

async fn cancel_turn(
    turns: Option<&VecDeque<TrackedTurn>>,
    target: PaneId,
    updates: &mpsc::UnboundedSender<WorkerEvent>,
) {
    let mut outcome = Err(NanocodexError::TurnNotCancellable);
    for turn in turns.into_iter().flatten() {
        match turn.control.cancel().await {
            Err(NanocodexError::TurnNotCancellable) => {}
            result => {
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
    commands: &mpsc::UnboundedSender<WorkerCommand>,
) -> Result<TerminalAction> {
    match event {
        Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            if handle_key(key, app, commands)? {
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

    match key.code {
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            app.insert_char('\n');
        }
        KeyCode::Enter => submit(app, commands, SubmitIntent::Immediate)?,
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
        KeyCode::Tab if app.has_input() => submit(app, commands, SubmitIntent::Queue)?,
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
                if app.queue_steer(target, prompt.clone()) {
                    send_command(commands, WorkerCommand::Steer { target, prompt })?;
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
    Submission::Prompt(input)
}

#[cfg(test)]
mod tests {
    use crossterm::event::Event;
    use tokio::sync::mpsc;

    use super::{
        BTW_BOUNDARY, RedrawPriority, Submission, UiAction, UiModel, UiUpdate, classify_submission,
        prepare_btw_prompt,
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
            classify_submission("/btw-not-a-command".to_owned()),
            Submission::Prompt("/btw-not-a-command".to_owned())
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
        let mut ui = UiModel::new(App::new("/workspace".into()));

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
}
