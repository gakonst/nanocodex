use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode, MouseEventKind,
};
use eyre::Result;
use nanocodex_dictation::DictationTranscript;
use tokio::sync::mpsc;

use super::{
    ComposerEdit, TerminalAction, WorkerCommand,
    app::{App, ComposerGeneration},
    send_command,
};

const SPACE_HOLD_DELAY: Duration = Duration::from_millis(250);
const TOGGLE_PREFIX_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_QUEUED_INTENTS: usize = 64;
const MAX_QUEUED_TEXT_BYTES: usize = 256 * 1024;

pub(super) struct DictationUi {
    state: State,
    next_id: u64,
    enabled: bool,
    hold_enabled: bool,
    space_pressed_at: Option<Instant>,
    toggle_prefix_at: Option<Instant>,
}

enum State {
    Idle,
    Capturing {
        id: u64,
        generation: ComposerGeneration,
    },
    Finishing {
        id: u64,
        generation: ComposerGeneration,
        queued: VecDeque<ComposerEdit>,
        queued_text_bytes: usize,
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
            toggle_prefix_at: None,
        }
    }

    pub(super) const fn is_active(&self) -> bool {
        !matches!(self.state, State::Idle)
            || self.space_pressed_at.is_some()
            || self.toggle_prefix_at.is_some()
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
            if self.enabled && is_dictation_toggle_prefix(key) {
                self.toggle_prefix_at = Some(Instant::now());
                return Ok(EventDisposition::Consume(TerminalAction::Ignore));
            }
            if key.kind == KeyEventKind::Press
                && let Some(started_at) = self.toggle_prefix_at.take()
                && Instant::now().saturating_duration_since(started_at) <= TOGGLE_PREFIX_TIMEOUT
                && is_dictation_toggle_suffix(key)
            {
                return self.handle_toggle(app, voice_active, commands);
            }
            if is_right_option(key) {
                return self.handle_push_to_talk(*key, app, voice_active, commands);
            }
            if self.enabled && self.hold_enabled && is_space(key) {
                return self.handle_space(*key, app, commands);
            }
            if key.kind == KeyEventKind::Press {
                self.toggle_prefix_at = None;
            }
        } else {
            self.toggle_prefix_at = None;
        }

        if self.space_pressed_at.is_some() && matches!(self.state, State::Idle) {
            self.space_pressed_at = None;
            app.insert_char(' ');
        }

        match &mut self.state {
            State::Idle => Ok(EventDisposition::Pass),
            State::Capturing { id, .. } => match event {
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
        self.toggle_prefix_at = None;
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

    pub(super) fn audio_level(&self, id: u64, peak: u16, app: &mut App) {
        if self.state.id() == Some(id) {
            app.update_dictation_audio_level(id, peak);
        }
    }

    pub(super) fn started(&self, id: u64, app: &mut App) {
        if self.state.id() == Some(id) {
            app.set_dictation_started(id);
        }
    }

    pub(super) fn finished(&mut self, id: u64, text: &str, app: &mut App) {
        if self.state.id() != Some(id) {
            return;
        }
        let state = std::mem::replace(&mut self.state, State::Idle);
        let State::Finishing {
            id: active,
            generation,
            queued,
            ..
        } = state
        else {
            self.state = state;
            return;
        };
        debug_assert_eq!(active, id);
        if app.composer_generation() == generation && app.commit_dictation(id, text) {
            replay(app, queued);
        } else {
            app.cancel_dictation(id);
        }
    }

    pub(super) fn no_speech(&mut self, id: u64, app: &mut App) {
        if self.state.id() != Some(id) {
            return;
        }
        let state = std::mem::replace(&mut self.state, State::Idle);
        let (_, generation, queued) = state.into_parts();
        app.cancel_dictation(id);
        self.space_pressed_at = None;
        if generation.is_some_and(|generation| app.composer_generation() == generation) {
            replay(app, queued);
        }
    }

    pub(super) fn failed(&mut self, id: u64, error: String, app: &mut App) {
        if self.state.id() != Some(id) {
            return;
        }
        let state = std::mem::replace(&mut self.state, State::Idle);
        let (_, generation, queued) = state.into_parts();
        app.cancel_dictation(id);
        self.space_pressed_at = None;
        app.push_active_error(format!("Dictation: {error}"));
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
        if self.toggle_prefix_at.is_some_and(|started_at| {
            now.saturating_duration_since(started_at) > TOGGLE_PREFIX_TIMEOUT
        }) {
            self.toggle_prefix_at = None;
        }
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
                if !self.enabled || !self.hold_enabled {
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
        commands: &mpsc::UnboundedSender<WorkerCommand>,
    ) -> Result<EventDisposition> {
        match key.kind {
            KeyEventKind::Press if matches!(self.state, State::Idle) => {
                self.space_pressed_at.get_or_insert_with(Instant::now);
                Ok(EventDisposition::Consume(TerminalAction::Ignore))
            }
            KeyEventKind::Repeat if matches!(self.state, State::Idle) => {
                Ok(EventDisposition::Consume(TerminalAction::Ignore))
            }
            KeyEventKind::Release if self.space_pressed_at.take().is_some() => {
                if matches!(self.state, State::Capturing { .. }) {
                    self.finish(app, commands)?;
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

    fn handle_toggle(
        &mut self,
        app: &mut App,
        voice_active: bool,
        commands: &mpsc::UnboundedSender<WorkerCommand>,
    ) -> Result<EventDisposition> {
        match self.state {
            State::Idle => self.start(app, voice_active, commands),
            State::Capturing { .. } => {
                self.finish(app, commands)?;
                Ok(EventDisposition::Consume(TerminalAction::Redraw))
            }
            State::Finishing { .. } => Ok(EventDisposition::Consume(TerminalAction::Bell)),
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
        self.state = State::Capturing { id, generation };
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
            State::Capturing { id, generation } => (id, generation),
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
        let (_, generation, queued) = state.into_parts();
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
    ) {
        match self {
            Self::Idle => (None, None, VecDeque::new()),
            Self::Capturing { id, generation, .. } => (Some(id), Some(generation), VecDeque::new()),
            Self::Finishing {
                id,
                generation,
                queued,
                ..
            } => (Some(id), Some(generation), queued),
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

fn is_dictation_toggle_prefix(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && matches!(key.code, KeyCode::Char('x'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn is_dictation_toggle_suffix(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V'))
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, time::Instant};

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode};

    use super::{DictationUi, EventDisposition, MAX_QUEUED_INTENTS, SPACE_HOLD_DELAY, State};
    use crate::tui::{ComposerEdit, WorkerCommand, app::App};

    #[test]
    fn ctrl_x_v_toggles_dictation_without_key_release_support() {
        let mut app = App::new(".".into());
        let mut ui = DictationUi::new(true, false);
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();

        toggle(&mut ui, &mut app, &commands);
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationStart { id: 1 })
        ));

        toggle(&mut ui, &mut app, &commands);
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationFinish { id: 1 })
        ));
    }

    #[test]
    fn ctrl_x_passes_through_when_dictation_is_disabled() {
        let mut app = App::new(".".into());
        let mut ui = DictationUi::new(false, false);
        let (commands, _) = tokio::sync::mpsc::unbounded_channel();
        let prefix = key(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        );

        assert!(matches!(
            ui.handle_event(&prefix, &mut app, false, &commands)
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
        ui.handle_event(&release, &mut app, false, &commands)
            .unwrap();
        assert_eq!(app.input, " ");
        assert!(receiver.try_recv().is_err());

        let mut app = App::new(".".into());
        let mut ui = DictationUi::new(true, true);
        let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();

        ui.handle_event(&press, &mut app, false, &commands).unwrap();
        ui.space_pressed_at = Some(Instant::now() - SPACE_HOLD_DELAY);
        assert!(ui.on_tick(&mut app, false, &commands).unwrap());
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationStart { id: 1 })
        ));

        ui.handle_event(&release, &mut app, false, &commands)
            .unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::DictationFinish { id: 1 })
        ));
    }

    #[test]
    fn no_speech_is_a_quiet_no_op_and_preserves_queued_edits() {
        let mut app = App::new(".".into());
        app.set_active_status("Agent still working");
        let mut ui = finishing_ui(&mut app, 1);
        assert!(ui.queue(ComposerEdit::Insert("kept".to_owned())).is_ok());

        ui.no_speech(1, &mut app);

        assert_eq!(app.input, "kept");
        assert_eq!(app.active_conversation().status, "Agent still working");
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

        ui.finished(1, "hello", &mut app);

        assert_eq!(app.input, "base hello?");
        assert!(!ui.is_active());
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
    fn composer_generation_change_discards_late_commit_and_queue() {
        let mut app = App::new(".".into());
        app.input = "base".to_owned();
        app.cursor = app.input.len();
        let mut ui = finishing_ui(&mut app, 1);
        assert!(ui.queue(ComposerEdit::Insert("queued".to_owned())).is_ok());
        app.insert_char('x');

        ui.finished(1, "late", &mut app);

        assert_eq!(app.input, "basex");
    }

    fn key(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> Event {
        let mut key = KeyEvent::new(code, modifiers);
        key.kind = kind;
        Event::Key(key)
    }

    fn toggle(
        ui: &mut DictationUi,
        app: &mut App,
        commands: &tokio::sync::mpsc::UnboundedSender<WorkerCommand>,
    ) {
        for event in [
            key(
                KeyCode::Char('x'),
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            ),
            key(KeyCode::Char('v'), KeyModifiers::NONE, KeyEventKind::Press),
        ] {
            ui.handle_event(&event, app, false, commands).unwrap();
        }
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
            },
            next_id: id + 1,
            enabled: true,
            hold_enabled: true,
            space_pressed_at: None,
            toggle_prefix_at: None,
        }
    }
}
