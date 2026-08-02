use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode, MouseEventKind,
};
use eyre::Result;
use nanocodex_dictation::{DictationTranscript, MicrophoneLevel};
use tokio::sync::mpsc;

use super::{
    ComposerEdit, SubmitIntent, TerminalAction, WorkerCommand,
    app::{App, ComposerGeneration},
    send_command,
};

const SPACE_HOLD_DELAY: Duration = Duration::from_millis(250);
const SHORT_SPACE_CAPTURE: Duration = Duration::from_millis(150);
const MAX_QUEUED_INTENTS: usize = 64;
const MAX_QUEUED_TEXT_BYTES: usize = 256 * 1024;

pub(super) struct DictationUi {
    state: State,
    next_id: u64,
    enabled: bool,
    hold_enabled: bool,
    space_pressed_at: Option<Instant>,
}

enum State {
    Idle,
    Capturing {
        id: u64,
        generation: ComposerGeneration,
        started_at: Instant,
    },
    Finishing {
        id: u64,
        generation: ComposerGeneration,
        queued: VecDeque<ComposerEdit>,
        queued_text_bytes: usize,
        submission: Option<SubmitIntent>,
    },
}

pub(super) enum EventDisposition {
    Pass,
    Consume(TerminalAction),
}

impl DictationUi {
    pub(super) const fn new(enabled: bool, hold_enabled: bool) -> Self {
        Self {
            state: State::Idle,
            next_id: 1,
            enabled,
            hold_enabled,
            space_pressed_at: None,
        }
    }

    pub(super) const fn is_active(&self) -> bool {
        !matches!(self.state, State::Idle) || self.space_pressed_at.is_some()
    }

    pub(super) fn handle_event(
        &mut self,
        event: &Event,
        app: &mut App,
        voice_active: bool,
        commands: &mpsc::UnboundedSender<WorkerCommand>,
    ) -> Result<EventDisposition> {
        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Press
                && !is_space(key)
                && matches!(self.state, State::Idle)
                && self.space_pressed_at.take().is_some()
            {
                app.insert_char(' ');
            }
            if self.enabled
                && matches!(self.state, State::Idle)
                && is_plain_enter(key)
                && app.input.trim() == "/dictate"
            {
                app.clear_input();
                return self.start(app, voice_active, commands);
            }
            if self.enabled && is_right_option(key) {
                return self.handle_push_to_talk(*key, app, voice_active, commands);
            }
            if self.enabled && self.hold_enabled && is_space(key) {
                return self.handle_space(*key, app, voice_active, commands);
            }
        }

        if self.space_pressed_at.is_some() && matches!(self.state, State::Idle) {
            self.space_pressed_at = None;
            app.insert_char(' ');
        }

        match &mut self.state {
            State::Idle => Ok(EventDisposition::Pass),
            State::Capturing { id, .. } => match event {
                Event::Key(key) if is_plain_enter(key) => {
                    self.finish(app, commands)?;
                    self.queue_submission(SubmitIntent::Immediate);
                    Ok(EventDisposition::Consume(TerminalAction::Redraw))
                }
                Event::Key(key) if is_plain_tab(key) => {
                    self.finish(app, commands)?;
                    self.queue_submission(SubmitIntent::Queue);
                    Ok(EventDisposition::Consume(TerminalAction::Redraw))
                }
                Event::Key(key)
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('c')) =>
                {
                    let id = *id;
                    self.cancel(app, commands, id, false)?;
                    Ok(EventDisposition::Pass)
                }
                Event::Key(key)
                    if key.kind != KeyEventKind::Release && key.code == KeyCode::Esc =>
                {
                    let id = *id;
                    self.cancel(app, commands, id, false)?;
                    Ok(EventDisposition::Consume(TerminalAction::Redraw))
                }
                Event::Resize(_, _)
                | Event::FocusGained
                | Event::FocusLost
                | Event::Mouse(crossterm::event::MouseEvent {
                    kind: MouseEventKind::ScrollUp | MouseEventKind::ScrollDown,
                    ..
                }) => Ok(EventDisposition::Pass),
                Event::Key(_) | Event::Paste(_) | Event::Mouse(_) => {
                    Ok(EventDisposition::Consume(TerminalAction::Redraw))
                }
            },
            State::Finishing { id, generation, .. } => {
                if let Event::Key(key) = event {
                    if key.kind == KeyEventKind::Release {
                        return Ok(EventDisposition::Consume(TerminalAction::Ignore));
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('c'))
                    {
                        let id = *id;
                        self.cancel(app, commands, id, true)?;
                        return Ok(EventDisposition::Pass);
                    }
                    if key.kind != KeyEventKind::Release && key.code == KeyCode::Esc {
                        let id = *id;
                        self.cancel(app, commands, id, true)?;
                        return Ok(EventDisposition::Consume(TerminalAction::Redraw));
                    }
                    if let Some(intent) = submission_intent(key) {
                        self.queue_submission(intent);
                        return Ok(EventDisposition::Consume(TerminalAction::Redraw));
                    }
                }
                if matches!(
                    event,
                    Event::Resize(_, _) | Event::FocusGained | Event::FocusLost
                ) || matches!(
                    event,
                    Event::Mouse(crossterm::event::MouseEvent {
                        kind: MouseEventKind::ScrollUp | MouseEventKind::ScrollDown,
                        ..
                    })
                ) {
                    return Ok(EventDisposition::Pass);
                }
                let Some(intent) = ComposerEdit::from_event(event) else {
                    return Ok(EventDisposition::Consume(TerminalAction::Bell));
                };
                let current_generation = *generation;
                match self.queue(intent) {
                    Ok(()) => return Ok(EventDisposition::Consume(TerminalAction::Redraw)),
                    Err(trigger) => self.overflow(app, commands, current_generation, trigger)?,
                }
                Ok(EventDisposition::Consume(TerminalAction::Redraw))
            }
        }
    }

    pub(super) fn focus_lost(
        &mut self,
        app: &mut App,
        commands: &mpsc::UnboundedSender<WorkerCommand>,
    ) -> Result<()> {
        if self.space_pressed_at.take().is_some() && matches!(self.state, State::Idle) {
            app.insert_char(' ');
        }
        if matches!(self.state, State::Capturing { .. }) {
            self.finish(app, commands)?;
        }
        Ok(())
    }

    pub(super) fn transcript(&mut self, id: u64, transcript: DictationTranscript, app: &mut App) {
        if self.state.id() == Some(id) {
            app.update_dictation(id, transcript.stable, transcript.unstable);
        }
    }

    pub(super) fn audio_level(&self, id: u64, level: MicrophoneLevel, app: &mut App) {
        if self.state.id() == Some(id) {
            app.update_dictation_audio_level(id, level);
        }
    }

    pub(super) fn started(&self, id: u64, app: &mut App) {
        if self.state.id() == Some(id) {
            app.set_dictation_started(id);
        }
    }

    pub(super) fn finished(&mut self, id: u64, text: &str, app: &mut App) -> Option<SubmitIntent> {
        if self.state.id() != Some(id) {
            return None;
        }
        let state = std::mem::replace(&mut self.state, State::Idle);
        let State::Finishing {
            id: active,
            generation,
            queued,
            submission,
            ..
        } = state
        else {
            self.state = state;
            return None;
        };
        debug_assert_eq!(active, id);
        if app.composer_generation() == generation && app.commit_dictation(id, text) {
            replay(app, queued);
            submission
        } else {
            app.cancel_dictation(id);
            None
        }
    }

    pub(super) fn no_speech(&mut self, id: u64, app: &mut App) -> Option<SubmitIntent> {
        if self.state.id() != Some(id) {
            return None;
        }
        let state = std::mem::replace(&mut self.state, State::Idle);
        let (_, generation, queued, submission) = state.into_parts();
        app.cancel_dictation(id);
        self.space_pressed_at = None;
        if generation.is_some_and(|generation| app.composer_generation() == generation) {
            replay(app, queued);
            submission
        } else {
            None
        }
    }

    pub(super) fn failed(&mut self, id: u64, error: String, app: &mut App) {
        if self.state.id() != Some(id) {
            return;
        }
        let state = std::mem::replace(&mut self.state, State::Idle);
        let (_, generation, queued, _) = state.into_parts();
        app.cancel_dictation(id);
        self.space_pressed_at = None;
        app.set_dictation_notice(format!("Dictation: {error}"));
        if generation.is_some_and(|generation| app.composer_generation() == generation) {
            replay(app, queued);
        }
    }

    pub(super) fn stopped(&mut self, id: u64, app: &mut App) {
        if self.state.id() == Some(id) {
            app.cancel_dictation(id);
            self.state = State::Idle;
            self.space_pressed_at = None;
        }
    }

    pub(super) fn on_tick(
        &mut self,
        app: &mut App,
        voice_active: bool,
        commands: &mpsc::UnboundedSender<WorkerCommand>,
    ) -> Result<bool> {
        let now = Instant::now();
        if matches!(self.state, State::Idle)
            && self.space_pressed_at.is_some_and(|started_at| {
                now.saturating_duration_since(started_at) >= SPACE_HOLD_DELAY
            })
        {
            if voice_active {
                self.space_pressed_at = None;
                app.insert_char(' ');
                app.set_active_status("Voice active — /voice off before dictating");
            } else {
                let _ = self.start(app, false, commands)?;
                if matches!(self.state, State::Idle) {
                    self.space_pressed_at = None;
                }
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn handle_push_to_talk(
        &mut self,
        key: KeyEvent,
        app: &mut App,
        voice_active: bool,
        commands: &mpsc::UnboundedSender<WorkerCommand>,
    ) -> Result<EventDisposition> {
        match key.kind {
            KeyEventKind::Press if matches!(self.state, State::Idle) => {
                if !self.hold_enabled {
                    app.set_active_status("Dictation requires terminal key-release support");
                    return Ok(EventDisposition::Consume(TerminalAction::Bell));
                }
                self.start(app, voice_active, commands)
            }
            KeyEventKind::Release => {
                if matches!(self.state, State::Capturing { .. }) {
                    self.finish(app, commands)?;
                }
                Ok(EventDisposition::Consume(TerminalAction::Redraw))
            }
            KeyEventKind::Press | KeyEventKind::Repeat => {
                Ok(EventDisposition::Consume(TerminalAction::Redraw))
            }
        }
    }

    fn handle_space(
        &mut self,
        key: KeyEvent,
        app: &mut App,
        voice_active: bool,
        commands: &mpsc::UnboundedSender<WorkerCommand>,
    ) -> Result<EventDisposition> {
        match key.kind {
            KeyEventKind::Press if matches!(self.state, State::Idle) => {
                self.space_pressed_at.get_or_insert_with(Instant::now);
                if app.input.is_empty() {
                    let disposition = self.start(app, voice_active, commands)?;
                    if matches!(self.state, State::Idle) {
                        self.space_pressed_at = None;
                        app.insert_char(' ');
                    }
                    return Ok(disposition);
                }
                Ok(EventDisposition::Consume(TerminalAction::Ignore))
            }
            KeyEventKind::Repeat if matches!(self.state, State::Idle) => {
                Ok(EventDisposition::Consume(TerminalAction::Ignore))
            }
            KeyEventKind::Release if self.space_pressed_at.take().is_some() => {
                if matches!(self.state, State::Capturing { .. }) {
                    let short_capture = match &self.state {
                        State::Capturing { id, started_at, .. }
                            if Instant::now().saturating_duration_since(*started_at)
                                < SHORT_SPACE_CAPTURE =>
                        {
                            Some(*id)
                        }
                        State::Idle | State::Capturing { .. } | State::Finishing { .. } => None,
                    };
                    if let Some(id) = short_capture {
                        self.cancel(app, commands, id, false)?;
                        app.insert_char(' ');
                    } else {
                        self.finish(app, commands)?;
                    }
                } else if matches!(self.state, State::Idle) {
                    app.insert_char(' ');
                }
                Ok(EventDisposition::Consume(TerminalAction::Redraw))
            }
            KeyEventKind::Press | KeyEventKind::Repeat | KeyEventKind::Release => {
                Ok(EventDisposition::Consume(TerminalAction::Redraw))
            }
        }
    }

    fn start(
        &mut self,
        app: &mut App,
        voice_active: bool,
        commands: &mpsc::UnboundedSender<WorkerCommand>,
    ) -> Result<EventDisposition> {
        if !self.enabled {
            return Ok(EventDisposition::Consume(TerminalAction::Bell));
        }
        if voice_active {
            app.set_active_status("Voice active — /voice off before dictating");
            return Ok(EventDisposition::Consume(TerminalAction::Bell));
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let Some(generation) = app.begin_dictation(id) else {
            return Ok(EventDisposition::Consume(TerminalAction::Bell));
        };
        self.state = State::Capturing {
            id,
            generation,
            started_at: Instant::now(),
        };
        send_command(commands, WorkerCommand::DictationStart { id })?;
        Ok(EventDisposition::Consume(TerminalAction::Redraw))
    }

    fn finish(
        &mut self,
        app: &mut App,
        commands: &mpsc::UnboundedSender<WorkerCommand>,
    ) -> Result<()> {
        let state = std::mem::replace(&mut self.state, State::Idle);
        let (id, generation) = match state {
            State::Capturing { id, generation, .. } => (id, generation),
            state => {
                self.state = state;
                return Ok(());
            }
        };
        app.set_dictation_finishing(id);
        self.state = State::Finishing {
            id,
            generation,
            queued: VecDeque::new(),
            queued_text_bytes: 0,
            submission: None,
        };
        send_command(commands, WorkerCommand::DictationFinish { id })
    }

    fn cancel(
        &mut self,
        app: &mut App,
        commands: &mpsc::UnboundedSender<WorkerCommand>,
        id: u64,
        replay_queued: bool,
    ) -> Result<()> {
        let state = std::mem::replace(&mut self.state, State::Idle);
        let (_, generation, queued, _) = state.into_parts();
        self.space_pressed_at = None;
        app.cancel_dictation(id);
        send_command(commands, WorkerCommand::DictationCancel { id })?;
        if replay_queued
            && generation.is_some_and(|generation| app.composer_generation() == generation)
        {
            replay(app, queued);
        }
        Ok(())
    }

    fn queue(&mut self, mut edit: ComposerEdit) -> Result<(), ComposerEdit> {
        let State::Finishing {
            queued,
            queued_text_bytes,
            ..
        } = &mut self.state
        else {
            return Err(edit);
        };
        if let Some(ComposerEdit::Insert(previous)) = queued.back_mut() {
            match edit {
                ComposerEdit::Insert(text) => {
                    if queued_text_bytes.saturating_add(text.len()) > MAX_QUEUED_TEXT_BYTES {
                        return Err(ComposerEdit::Insert(text));
                    }
                    previous.push_str(&text);
                    *queued_text_bytes = queued_text_bytes.saturating_add(text.len());
                    return Ok(());
                }
                other => edit = other,
            }
        }
        let added = edit.text_bytes();
        if queued.len() >= MAX_QUEUED_INTENTS
            || queued_text_bytes.saturating_add(added) > MAX_QUEUED_TEXT_BYTES
        {
            return Err(edit);
        }
        queued.push_back(edit);
        *queued_text_bytes = queued_text_bytes.saturating_add(added);
        Ok(())
    }

    const fn queue_submission(&mut self, intent: SubmitIntent) {
        if let State::Finishing { submission, .. } = &mut self.state {
            *submission = Some(intent);
        }
    }

    fn overflow(
        &mut self,
        app: &mut App,
        commands: &mpsc::UnboundedSender<WorkerCommand>,
        generation: ComposerGeneration,
        trigger: ComposerEdit,
    ) -> Result<()> {
        let state = std::mem::replace(&mut self.state, State::Idle);
        let State::Finishing { id, mut queued, .. } = state else {
            self.state = state;
            return Ok(());
        };
        queued.push_back(trigger);
        send_command(commands, WorkerCommand::DictationCancel { id })?;
        if app.composer_generation() == generation {
            if !app.commit_dictation_draft(id) {
                app.cancel_dictation(id);
                app.push_active_error("Dictation: edit queue filled before speech was available");
            }
            replay(app, queued);
        } else {
            app.cancel_dictation(id);
        }
        Ok(())
    }
}

impl State {
    const fn id(&self) -> Option<u64> {
        match self {
            Self::Idle => None,
            Self::Capturing { id, .. } | Self::Finishing { id, .. } => Some(*id),
        }
    }

    fn into_parts(
        self,
    ) -> (
        Option<u64>,
        Option<ComposerGeneration>,
        VecDeque<ComposerEdit>,
        Option<SubmitIntent>,
    ) {
        match self {
            Self::Idle => (None, None, VecDeque::new(), None),
            Self::Capturing { id, generation, .. } => {
                (Some(id), Some(generation), VecDeque::new(), None)
            }
            Self::Finishing {
                id,
                generation,
                queued,
                submission,
                ..
            } => (Some(id), Some(generation), queued, submission),
        }
    }
}

fn replay(app: &mut App, queued: VecDeque<ComposerEdit>) {
    for edit in queued {
        edit.apply(app);
    }
}

const fn is_right_option(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Modifier(ModifierKeyCode::RightAlt))
}

const fn is_space(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(' ')) && key.modifiers.is_empty()
}

fn is_plain_enter(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press && key.code == KeyCode::Enter && key.modifiers.is_empty()
}

fn submission_intent(key: &KeyEvent) -> Option<SubmitIntent> {
    if is_plain_enter(key) {
        Some(SubmitIntent::Immediate)
    } else if is_plain_tab(key) {
        Some(SubmitIntent::Queue)
    } else {
        None
    }
}

fn is_plain_tab(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press && key.code == KeyCode::Tab && key.modifiers.is_empty()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        time::{Duration, Instant},
    };

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode};

    use super::{
        DictationUi, EventDisposition, MAX_QUEUED_INTENTS, SHORT_SPACE_CAPTURE, SPACE_HOLD_DELAY,
        State,
    };
    use crate::tui::{ComposerEdit, SubmitIntent, TerminalAction, WorkerCommand, app::App};

    #[test]
    fn dictate_command_starts_and_enter_defers_submission_until_final_text() {
        let mut app = App::new(".".into());
        app.input = "/dictate".to_owned();
        app.cursor = app.input.len();
        let mut ui = DictationUi::new(true, false);
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let enter = key(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Press);

        ui.handle_event(&enter, &mut app, false, &commands).unwrap();
        assert!(app.input.is_empty());
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationStart { id: 1 })
        ));

        ui.handle_event(&enter, &mut app, false, &commands).unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationFinish { id: 1 })
        ));
        assert_eq!(
            ui.finished(1, "send this", &mut app),
            Some(SubmitIntent::Immediate)
        );
        assert_eq!(app.input, "send this");
    }

    #[test]
    fn tab_during_capture_queues_submission_after_final_transcript() {
        let mut app = App::new(".".into());
        let mut ui = DictationUi::new(true, false);
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(ui.start(&mut app, false, &commands).is_ok());
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationStart { id: 1 })
        ));

        ui.handle_event(
            &key(KeyCode::Tab, KeyModifiers::NONE, KeyEventKind::Press),
            &mut app,
            false,
            &commands,
        )
        .unwrap();

        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationFinish { id: 1 })
        ));
        assert_eq!(
            ui.finished(1, "queue this", &mut app),
            Some(SubmitIntent::Queue)
        );
        assert_eq!(app.input, "queue this");
    }

    #[test]
    fn right_option_follows_standard_input_handling() {
        let mut app = App::new(".".into());
        let mut ui = DictationUi::new(false, false);
        let (commands, _) = tokio::sync::mpsc::unbounded_channel();
        let right_option = Event::Key(KeyEvent::new(
            KeyCode::Modifier(ModifierKeyCode::RightAlt),
            KeyModifiers::ALT,
        ));
        assert!(matches!(
            ui.handle_event(&right_option, &mut app, false, &commands)
                .unwrap(),
            EventDisposition::Pass
        ));
    }

    #[test]
    fn space_taps_are_text_and_holds_start_dictation_in_any_composer_context() {
        let mut app = App::new(".".into());
        app.input = "hello".to_owned();
        app.cursor = app.input.len();
        let mut ui = DictationUi::new(true, true);
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let press = key(KeyCode::Char(' '), KeyModifiers::NONE, KeyEventKind::Press);
        let release = key(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );

        ui.handle_event(&press, &mut app, false, &commands).unwrap();
        ui.handle_event(&release, &mut app, false, &commands)
            .unwrap();
        assert_eq!(app.input, "hello ");
        assert!(receiver.try_recv().is_err());

        let mut app = App::new(".".into());
        let mut ui = DictationUi::new(true, true);
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();

        ui.handle_event(&press, &mut app, false, &commands).unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationStart { id: 1 })
        ));
        ui.handle_event(&release, &mut app, false, &commands)
            .unwrap();
        assert_eq!(app.input, " ");
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationCancel { id: 1 })
        ));

        let mut app = App::new(".".into());
        app.input = "hello".to_owned();
        app.cursor = app.input.len();
        let mut ui = DictationUi::new(true, true);
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();

        ui.handle_event(&press, &mut app, false, &commands).unwrap();
        ui.space_pressed_at = Some(Instant::now() - SPACE_HOLD_DELAY);
        assert!(ui.on_tick(&mut app, false, &commands).unwrap());
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationStart { id: 1 })
        ));
        if let State::Capturing { started_at, .. } = &mut ui.state {
            *started_at = Instant::now() - SHORT_SPACE_CAPTURE;
        }

        ui.handle_event(&release, &mut app, false, &commands)
            .unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationFinish { id: 1 })
        ));
    }

    #[test]
    fn brief_space_release_inserts_text_after_hold_threshold() {
        let mut app = App::new(".".into());
        app.input = "hello".to_owned();
        app.cursor = app.input.len();
        let mut ui = DictationUi::new(true, true);
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let press = key(KeyCode::Char(' '), KeyModifiers::NONE, KeyEventKind::Press);
        let release = key(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );

        ui.handle_event(&press, &mut app, false, &commands).unwrap();
        ui.space_pressed_at = Some(Instant::now() - SPACE_HOLD_DELAY);
        assert!(ui.on_tick(&mut app, false, &commands).unwrap());
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationStart { id: 1 })
        ));
        if let State::Capturing { started_at, .. } = &mut ui.state {
            *started_at = Instant::now() + Duration::from_secs(60);
        }

        ui.handle_event(&release, &mut app, false, &commands)
            .unwrap();

        assert_eq!(app.input, "hello ");
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationCancel { id: 1 })
        ));
        assert!(!ui.is_active());
    }

    #[test]
    fn silent_attempt_preserves_queued_edits_quietly() {
        let mut app = App::new(".".into());
        app.set_active_status("Agent still working");
        let mut ui = finishing_ui(&mut app, 1);
        assert!(ui.queue(ComposerEdit::Insert("kept".to_owned())).is_ok());

        assert_eq!(ui.no_speech(1, &mut app), None);

        assert_eq!(app.input, "kept");
        assert_eq!(app.active_conversation().status, "Agent still working");
        assert!(!ui.is_active());
    }

    #[test]
    fn failure_appears_as_a_footer_notice() {
        let mut app = App::new(".".into());
        app.main.running = true;
        let mut ui = finishing_ui(&mut app, 1);

        ui.failed(1, "microphone permission denied".to_owned(), &mut app);

        assert_eq!(
            app.dictation_status().as_deref(),
            Some("Dictation: microphone permission denied")
        );
        assert!(app.main.transcript.is_empty());
        assert!(!ui.is_active());
    }

    #[test]
    fn right_option_press_and_release_control_one_attempt() {
        let mut app = App::new(".".into());
        let mut ui = DictationUi::new(true, true);
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();

        ui.handle_event(
            &key(
                KeyCode::Modifier(ModifierKeyCode::RightAlt),
                KeyModifiers::ALT,
                KeyEventKind::Press,
            ),
            &mut app,
            false,
            &commands,
        )
        .unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationStart { id: 1 })
        ));

        ui.handle_event(
            &key(
                KeyCode::Modifier(ModifierKeyCode::RightAlt),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            ),
            &mut app,
            false,
            &commands,
        )
        .unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationFinish { id: 1 })
        ));
    }

    #[test]
    fn finishing_replays_coalesced_edits_after_one_transcript_commit() {
        let mut app = App::new(".".into());
        app.input = "base".to_owned();
        app.cursor = app.input.len();
        let mut ui = finishing_ui(&mut app, 1);
        let (commands, _) = tokio::sync::mpsc::unbounded_channel();
        for code in [KeyCode::Char('!'), KeyCode::Backspace, KeyCode::Char('?')] {
            ui.handle_event(
                &key(code, KeyModifiers::NONE, KeyEventKind::Press),
                &mut app,
                false,
                &commands,
            )
            .unwrap();
        }

        assert_eq!(ui.finished(1, "hello", &mut app), None);

        assert_eq!(app.input, "base hello?");
        assert!(!ui.is_active());
    }

    #[test]
    fn enter_on_faded_text_waits_then_submits_one_stable_copy() {
        let mut app = App::new(".".into());
        app.main.running = true;
        let mut ui = finishing_ui(&mut app, 1);
        ui.transcript(
            1,
            nanocodex_dictation::DictationTranscript {
                stable: String::new(),
                unstable: "send this once".to_owned(),
            },
            &mut app,
        );
        let (commands, _) = tokio::sync::mpsc::unbounded_channel();

        ui.handle_event(
            &key(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Press),
            &mut app,
            false,
            &commands,
        )
        .unwrap();

        assert!(
            app.input.is_empty(),
            "partial speech stays virtual until finalization"
        );
        assert_eq!(
            ui.finished(1, "send this once", &mut app),
            Some(SubmitIntent::Immediate)
        );
        assert_eq!(app.input, "send this once");
    }

    #[test]
    fn finishing_escape_replays_reversible_edits_and_cancels_attempt() {
        let mut app = App::new(".".into());
        let mut ui = finishing_ui(&mut app, 9);
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(ui.queue(ComposerEdit::Insert("kept".to_owned())).is_ok());
        let event = key(KeyCode::Esc, KeyModifiers::NONE, KeyEventKind::Press);

        ui.handle_event(&event, &mut app, false, &commands).unwrap();
        assert_eq!(app.input, "kept");
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationCancel { id: 9 })
        ));
    }

    #[test]
    fn finishing_ignores_key_release_after_queuing_the_press() {
        let mut app = App::new(".".into());
        let mut ui = finishing_ui(&mut app, 1);
        let (commands, _) = tokio::sync::mpsc::unbounded_channel();
        let press = key(KeyCode::Char('x'), KeyModifiers::NONE, KeyEventKind::Press);
        let release = key(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );

        assert!(matches!(
            ui.handle_event(&press, &mut app, false, &commands).unwrap(),
            EventDisposition::Consume(TerminalAction::Redraw)
        ));
        assert!(matches!(
            ui.handle_event(&release, &mut app, false, &commands)
                .unwrap(),
            EventDisposition::Consume(TerminalAction::Ignore)
        ));
        assert_eq!(ui.finished(1, "speech", &mut app), None);
        assert_eq!(app.input, "speechx");
    }

    #[test]
    fn queue_overflow_commits_partial_text_before_replaying_edits() {
        let mut app = App::new(".".into());
        app.input = "base".to_owned();
        app.cursor = app.input.len();
        let mut ui = finishing_ui(&mut app, 1);
        ui.transcript(
            1,
            nanocodex_dictation::DictationTranscript {
                stable: String::new(),
                unstable: "latest".to_owned(),
            },
            &mut app,
        );
        for _ in 0..MAX_QUEUED_INTENTS {
            assert!(ui.queue(ComposerEdit::Right).is_ok());
        }
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();

        ui.handle_event(
            &key(KeyCode::Char('!'), KeyModifiers::NONE, KeyEventKind::Press),
            &mut app,
            false,
            &commands,
        )
        .unwrap();

        assert_eq!(app.input, "base latest!");
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationCancel { id: 1 })
        ));
    }

    #[test]
    fn composer_generation_change_preserves_current_composer_state() {
        let mut app = App::new(".".into());
        app.input = "base".to_owned();
        app.cursor = app.input.len();
        let mut ui = finishing_ui(&mut app, 1);
        assert!(ui.queue(ComposerEdit::Insert("queued".to_owned())).is_ok());
        app.insert_char('x');

        assert_eq!(ui.finished(1, "late", &mut app), None);

        assert_eq!(app.input, "basex");
    }

    fn key(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> Event {
        let mut key = KeyEvent::new(code, modifiers);
        key.kind = kind;
        Event::Key(key)
    }

    fn finishing_ui(app: &mut App, id: u64) -> DictationUi {
        let generation = app.begin_dictation(id).unwrap();
        app.set_dictation_finishing(id);
        DictationUi {
            state: State::Finishing {
                id,
                generation,
                queued: VecDeque::new(),
                queued_text_bytes: 0,
                submission: None,
            },
            next_id: id + 1,
            enabled: true,
            hold_enabled: true,
            space_pressed_at: None,
        }
    }
}
