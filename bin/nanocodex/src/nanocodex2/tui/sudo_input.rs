//! Trusted caller-local sudo approval. This module is NOT an App/Root event.
//!
//! Terminal events must enter `Flow::intercept` before clipboard, editor, debug,
//! control bridge or transcript handling. Only encrypted envelopes leave it.
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use nanocodex_managed::{
    NativeSecureInputDescription, NativeSecureInputEnvelope, NativeSecureInputRequest,
};
use ratatui::{
    Frame,
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    Latest,
    Select { agent: String, request: String },
    Help,
}
impl Command {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let words: Vec<_> = text.split_whitespace().collect();
        if words.first().copied() != Some("/secure-input") {
            return None;
        }
        Some(match words.as_slice() {
            [_] => Self::Latest,
            [_, agent, request]
                if valid_selector(agent) && uuid::Uuid::parse_str(request).is_ok() =>
            {
                Self::Select {
                    agent: (*agent).into(),
                    request: (*request).into(),
                }
            }
            _ => Self::Help,
        })
    }
    pub(crate) fn matches(&self, intake: &NativeSecureInputRequest) -> bool {
        match self {
            Self::Latest => true,
            Self::Select { agent, request } => {
                &intake.agent_id == agent && &intake.request_id == request
            }
            Self::Help => false,
        }
    }
}
fn valid_selector(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}

// Intentionally no Debug, Serialize, Clone or accessor. Drop wipes allocations.
struct Password {
    bytes: Zeroizing<[u8; 4096]>,
    len: usize,
}
impl Password {
    fn new() -> Self {
        Self {
            bytes: Zeroizing::new([0; 4096]),
            len: 0,
        }
    }
    fn push(&mut self, text: &str) {
        if self.len + text.len() <= self.bytes.len() && !text.chars().any(char::is_control) {
            self.bytes[self.len..self.len + text.len()].copy_from_slice(text.as_bytes());
            self.len += text.len();
        }
    }
    fn pop(&mut self) {
        if self.len == 0 {
            return;
        }
        let mut end = self.len - 1;
        while end > 0 && self.bytes[end] & 0xc0 == 0x80 {
            end -= 1;
        }
        self.bytes[end..self.len].zeroize();
        self.len = end;
    }
    fn secret(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).expect("UTF-8 password invariant")
    }
}

/// Permanent process hardening BEFORE creating a password buffer. This is
/// deliberately not restored: a cancelled field must not reopen inspection.
#[allow(unsafe_code)]
pub(crate) fn protect_process() -> std::io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use nix::libc;
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: constant kernel arguments and valid stack pointer.
        if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        #[cfg(target_os = "linux")]
        {
            if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0
                || unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) } != 0
            {
                return Err(std::io::Error::other(
                    "private input protection unavailable",
                ));
            }
        }
        #[cfg(target_os = "linux")]
        {
            let status = std::fs::read_to_string("/proc/self/status")?;
            let tracer = status
                .lines()
                .find_map(|line| line.strip_prefix("TracerPid:"))
                .and_then(|value| value.trim().parse::<u32>().ok());
            if tracer != Some(0) {
                return Err(std::io::Error::other(
                    "private input protection unavailable",
                ));
            }
        }
        #[cfg(target_os = "macos")]
        {
            static DENIED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if !*DENIED.get_or_init(|| unsafe { libc::ptrace(31, 0, std::ptr::null_mut(), 0) } == 0)
            {
                return Err(std::io::Error::other(
                    "private input protection unavailable",
                ));
            }
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    Err(std::io::Error::other("private input unsupported"))
}

// These private states never enter ordinary component or control snapshots.
enum Phase {
    Loading,
    Review(NativeSecureInputDescription),
    Password(NativeSecureInputDescription, Password),
    Sending,
    Status(&'static str),
}
pub(crate) struct Flow {
    pub(crate) request: NativeSecureInputRequest,
    pub(crate) generation: u64,
    pub(crate) pane: super::pane::PaneId,
    phase: Phase,
    fully_visible: bool,
    focused: bool,
    drain: bool,
    token: String,
    token_matched: usize,
    token_ready: bool,
}
pub(crate) enum Action {
    None,
    Cancel,
    Dismiss,
    Submit(NativeSecureInputEnvelope),
}
pub(crate) enum Outcome {
    Description(NativeSecureInputDescription),
    Status(Status),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Status {
    Completed,
    Failed,
    Unknown,
    Cancelled,
    Unavailable,
}
impl Status {
    pub(crate) fn wire(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Unknown => "outcome_unknown",
            Self::Cancelled => "cancelled",
            Self::Unavailable => "unavailable",
        }
    }
    fn message(self) -> &'static str {
        match self {
            Self::Completed => "Protected command completed successfully.",
            Self::Failed => "Protected command failed. Check the machine before continuing.",
            Self::Unknown => UNKNOWN,
            Self::Cancelled => "Secure input cancelled.",
            Self::Unavailable => {
                "Private secure input unavailable. No password requested or submitted."
            }
        }
    }
}
pub(crate) type Completion = (String, u64, String, Outcome);
pub(crate) const HELP: &str = "Use /secure-input to review the latest protected sudo request, or /secure-input AGENT_ID REQUEST_ID to select a pending request. Never put a password in chat.";
pub(crate) const UNKNOWN: &str = "Protected command outcome unknown. Check the machine before any further attempt. This submission will not be retried.";
impl Flow {
    pub(crate) fn loading(
        request: NativeSecureInputRequest,
        generation: u64,
        pane: super::pane::PaneId,
    ) -> Self {
        Self {
            request,
            generation,
            pane,
            phase: Phase::Loading,
            fully_visible: false,
            focused: true,
            drain: true,
            token: fresh_token(),
            token_matched: 0,
            token_ready: false,
        }
    }
    pub(crate) fn scope_matches(
        &self,
        agent: &str,
        generation: u64,
        pane: Option<super::pane::PaneId>,
    ) -> bool {
        matches!(self.phase, Phase::Status(_))
            || (self.request.agent_id == agent
                && self.generation == generation
                && pane == Some(self.pane)
                && self.request.is_current())
    }
    fn reset_token(&mut self) {
        self.token = fresh_token();
        self.token_matched = 0;
        self.token_ready = false;
        self.fully_visible = false;
    }
    pub(crate) fn is_sending(&self) -> bool {
        matches!(self.phase, Phase::Sending)
    }
    pub(crate) fn can_dismiss(&self) -> bool {
        self.focused && self.fully_visible && self.token_ready
    }
    pub(crate) fn take_drain(&mut self) -> bool {
        std::mem::take(&mut self.drain)
    }
    pub(crate) fn cancel_local(&mut self) {
        if !matches!(self.phase, Phase::Status(_)) {
            self.phase = Phase::Status(if matches!(self.phase, Phase::Sending) {
                UNKNOWN
            } else {
                "Secure input cancelled."
            });
            self.reset_token();
        }
        self.fully_visible = false;
        self.drain = true;
    }
    pub(crate) fn finish(&mut self, outcome: Outcome) {
        if matches!(self.phase, Phase::Status(_)) && matches!(outcome, Outcome::Description(_)) {
            return;
        }
        self.phase = match outcome {
            Outcome::Description(value) => {
                self.request.machine_id = Some(value.machine_id.clone());
                self.request.expires_at = Some(value.expires_at);
                Phase::Review(value)
            }
            Outcome::Status(value) => Phase::Status(value.message()),
        };
        self.reset_token();
        self.drain = true;
    }
    /// Consumes ALL terminal events, including paste, during private approval.
    /// No caller may forward the consumed event to AppEvent::Terminal.
    pub(crate) fn intercept(&mut self, mut event: Event) -> Action {
        // Focus/pane shortcuts cancel locally and are NOT forwarded. Ctrl+C
        // wipes before shutdown. Mouse cannot silently switch panes underneath.
        if matches!(&event, Event::FocusGained) {
            self.focused = true;
            if matches!(self.phase, Phase::Password(_, _) | Phase::Sending) {
                self.cancel_local();
                return Action::Cancel;
            }
            self.reset_token();
            return Action::None;
        }
        if matches!(&event, Event::FocusLost) {
            self.focused = false;
            self.reset_token();
        }
        let cancel = matches!(&event, Event::FocusLost)
            || matches!(&event, Event::Key(key) if key.kind != KeyEventKind::Release && (key.code == KeyCode::Esc || key.code == KeyCode::Tab || key.code == KeyCode::BackTab || (key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c' | 'd' | 'z')))));
        if cancel {
            wipe_paste(&mut event);
            if matches!(self.phase, Phase::Status(_)) {
                // Focus loss never dismisses the guard. Pasted/queued secrets
                // remain intercepted until a focused, rendered explicit Esc.
                return if self.focused
                    && self.fully_visible
                    && self.token_ready
                    && matches!(event, Event::Key(key) if key.code == KeyCode::Esc)
                {
                    self.drain = true;
                    Action::Dismiss
                } else {
                    Action::None
                };
            }
            self.cancel_local(); // drops/wipes Password BEFORE cancel HTTP
            return Action::Cancel;
        }
        // Fresh, unpredictable, typed-only token proves these events follow
        // THIS phase's rendered frame. Crossterm retains incomplete paste/escape
        // parser state; a timing-based queue drain alone cannot prove freshness.
        // Only a prefix index is retained: mistyped secrets are never buffered.
        if !self.token_ready
            && matches!(
                self.phase,
                Phase::Review(_) | Phase::Password(_, _) | Phase::Status(_)
            )
        {
            if self.fully_visible
                && self.focused
                && let Event::Key(key) = &event
                && key.kind == KeyEventKind::Press
                && key.modifiers == KeyModifiers::NONE
                && let KeyCode::Char(c) = key.code
            {
                if c.is_ascii()
                    && Some(c as u8) == self.token.as_bytes().get(self.token_matched).copied()
                {
                    self.token_matched += 1;
                    if self.token_matched == self.token.len() {
                        self.token_ready = true;
                        self.fully_visible = false;
                        self.drain = true;
                    }
                } else {
                    self.token_matched = 0;
                }
            }
            wipe_paste(&mut event);
            return Action::None;
        }
        let action = match (&mut self.phase, &mut event) {
            (Phase::Review(_), Event::Key(key))
                if key.kind == KeyEventKind::Press
                    && key.code == KeyCode::Enter
                    && key.modifiers == KeyModifiers::CONTROL
                    && self.fully_visible =>
            {
                let Phase::Review(description) = std::mem::replace(&mut self.phase, Phase::Loading)
                else {
                    unreachable!()
                };
                self.phase = if protect_process().is_ok() {
                    Phase::Password(description, Password::new())
                } else {
                    Phase::Status(
                        "Private input protection unavailable. No password requested or submitted.",
                    )
                };
                self.reset_token();
                self.drain = true;
                Action::None
            }
            (Phase::Password(_, password), Event::Paste(text))
                if self.fully_visible && self.focused =>
            {
                password.push(text);
                text.zeroize();
                Action::None
            }
            (Phase::Password(_, password), Event::Key(key))
                if self.fully_visible
                    && self.focused
                    && key.kind == KeyEventKind::Press
                    && key
                        .modifiers
                        .intersection(
                            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                        )
                        .is_empty() =>
            {
                match key.code {
                    KeyCode::Char(character) => {
                        let mut bytes = [0_u8; 4];
                        password.push(character.encode_utf8(&mut bytes));
                        bytes.zeroize();
                        Action::None
                    }
                    KeyCode::Backspace => {
                        password.pop();
                        Action::None
                    }
                    KeyCode::Enter
                        if key.kind == KeyEventKind::Press
                            && self.fully_visible
                            && password.len != 0 =>
                    {
                        let Phase::Password(description, password) =
                            std::mem::replace(&mut self.phase, Phase::Sending)
                        else {
                            unreachable!()
                        };
                        // Encrypt synchronously; no plaintext is ever moved into
                        // a task, model input, HTTP builder or application event.
                        let encrypted = description.encrypt(password.secret());
                        drop(password);
                        self.drain = true;
                        match encrypted {
                            Ok(envelope) => Action::Submit(envelope),
                            Err(_) => {
                                self.phase = Phase::Status(
                                    "Secure input could not be encrypted. No submission sent.",
                                );
                                self.reset_token();
                                Action::None
                            }
                        }
                    }
                    _ => Action::None,
                }
            }
            _ => Action::None,
        };
        wipe_paste(&mut event);
        action
    }
    pub(crate) fn render(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title(" Private protected sudo approval ")
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Yellow));
        let body = block.inner(area);
        frame.render_widget(block, area);
        let mut description = match &self.phase {
            Phase::Loading => "Fetching command privately from your authenticated account.\nPassword input is disabled. Esc cancels.".to_owned(),
            Phase::Review(value) | Phase::Password(value, _) => {
                let args = value.arguments.iter().enumerate().map(|(i, arg)| format!("argv[{i}]: {}", literal(arg))).collect::<Vec<_>>().join("\n");
                let footer = if let Phase::Password(_, _) = &self.phase {
                    format!("Password: {}\nEnter: encrypt and submit once. Esc: cancel.\nYour password never enters chat, tools, history or logs.", "********")
                } else {
                    "Review EVERY argument before approving.\nCtrl+Enter: approve command and open private password field.\nEsc: cancel. Executables/scripts can change after approval.".into()
                };
                format!("Account: {}\nAgent: {}\nCommand digest (SHA-256): {}\nMachine: {}\nRequest: {}\nUID: {}\nExpires (Unix ms): {}\nExecutable: {}\nWorking directory: {}\n{}\n\n{}", literal(&value.account_id), literal(&self.request.agent_id), value.command_digest(), literal(&value.machine_id), value.request_id, value.uid, value.expires_at, literal(&value.executable), literal(&value.cwd), args, footer)
            }
            Phase::Sending => "Submitting encrypted approval once…\nPassword discarded. Unknown outcomes are never retried.\nEsc closes this local panel; it cannot undo a sent command.".into(),
            Phase::Status(status) => format!("{status}\n\nEsc closes this private panel."),
        };
        if matches!(
            self.phase,
            Phase::Review(_) | Phase::Password(_, _) | Phase::Status(_)
        ) {
            if self.token_ready {
                description.push_str("\n\nSafety token verified. Controls above are now enabled.");
            } else {
                description.push_str(&format!("\n\nInput disabled until this fresh token is typed, NOT pasted.\nType safety token (keys only): {}", self.token));
            }
        }
        let lines = super::vault::review_lines(&description, body.width);
        self.fully_visible = body.width >= 20 && lines.len() <= usize::from(body.height);
        if self.fully_visible {
            frame.render_widget(Paragraph::new(lines.join("\n")), body);
        } else {
            frame.render_widget(Paragraph::new("Enlarge terminal to review ALL command details. Approval and submission disabled. Esc cancels."), body);
        }
    }
}
fn fresh_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
fn wipe_paste(event: &mut Event) {
    if let Event::Paste(text) = event {
        text.zeroize();
    }
}
/// Render controls, bidi and formatting characters literally, never as terminal escapes.
fn literal(value: &str) -> String {
    let quoted = serde_json::to_string(value).unwrap_or_else(|_| "[invalid]".into());
    quoted.chars().map(|c| {
        if c.is_control() || matches!(c, '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}') { format!("\\u{{{:x}}}", c as u32) } else { c.to_string() }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn request() -> NativeSecureInputRequest {
        NativeSecureInputRequest::selector("cbbfa5ef-2e4b-45f7-9c98-3913f8ca87cf", "agent").unwrap()
    }
    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    #[test]
    fn fixed_secret_buffer_erases_unicode_backspace_without_clone() {
        let mut password = Password::new();
        password.push("a雪");
        password.pop();
        assert_eq!(password.secret(), "a");
        assert_eq!(&password.bytes[1..4], &[0, 0, 0]);
        password.push("control\n");
        assert_eq!(password.secret(), "a");
        password.pop();
        assert_eq!(password.len, 0);
        assert!(password.bytes.iter().all(|byte| *byte == 0));
    }
    #[test]
    fn loading_and_cancelled_guard_consume_all_keys_paste_focus_and_tail() {
        let mut flow = Flow::loading(request(), 1, super::super::pane::PaneId::Main);
        assert!(flow.take_drain());
        for event in [
            key(KeyCode::Char('x')),
            Event::Paste("SYNTHETIC_SECRET".into()),
            key(KeyCode::Enter),
        ] {
            assert!(matches!(flow.intercept(event), Action::None));
            assert!(matches!(flow.phase, Phase::Loading));
        }
        assert!(matches!(flow.intercept(Event::FocusLost), Action::Cancel));
        assert!(matches!(flow.phase, Phase::Status(_)));
        flow.fully_visible = true;
        assert!(matches!(flow.intercept(key(KeyCode::Esc)), Action::None));
        assert!(matches!(
            flow.intercept(Event::Paste("QUEUED_TAIL_SECRET".into())),
            Action::None
        ));
        assert!(matches!(flow.intercept(Event::FocusGained), Action::None));
        // A queued Esc before the cancellation frame has been drawn is ignored.
        assert!(matches!(flow.intercept(key(KeyCode::Esc)), Action::None));
        flow.fully_visible = true;
        flow.token_ready = true;
        assert!(matches!(flow.intercept(key(KeyCode::Esc)), Action::Dismiss));
        assert!(flow.take_drain());
    }
    #[test]
    fn freshness_token_rejects_paste_repeats_and_old_focus_scope() {
        let mut flow = Flow::loading(request(), 1, super::super::pane::PaneId::Main);
        flow.cancel_local();
        flow.fully_visible = true;
        let old = flow.token.clone(); // token is NONSECRET random UI metadata
        assert!(matches!(
            flow.intercept(Event::Paste(old.clone())),
            Action::None
        ));
        assert!(!flow.token_ready);
        let mut repeat = KeyEvent::new(
            KeyCode::Char(old.chars().next().unwrap()),
            KeyModifiers::NONE,
        );
        repeat.kind = KeyEventKind::Repeat;
        flow.intercept(Event::Key(repeat));
        assert_eq!(flow.token_matched, 0);
        flow.intercept(Event::FocusLost);
        flow.intercept(Event::FocusGained);
        assert_ne!(flow.token, old);
        flow.fully_visible = true;
        for c in old.chars() {
            flow.intercept(key(KeyCode::Char(c)));
        }
        assert!(!flow.token_ready);
        let token = flow.token.clone();
        // Force a mismatch to discard any coincidental old-prefix progress.
        flow.intercept(key(KeyCode::Char('z')));
        for c in token.chars() {
            flow.intercept(key(KeyCode::Char(c)));
        }
        assert!(flow.token_ready);
        assert!(flow.take_drain());
        assert!(!flow.fully_visible); // no exit until verified frame is rendered
        assert!(matches!(flow.intercept(key(KeyCode::Esc)), Action::None));
        flow.fully_visible = true;
        assert!(matches!(flow.intercept(key(KeyCode::Esc)), Action::Dismiss));
    }
    #[test]
    fn status_requires_complete_render_and_focused_fresh_token() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut flow = Flow::loading(request(), 1, super::super::pane::PaneId::Main);
        flow.token_ready = true;
        let old = flow.token.clone();
        flow.finish(Outcome::Status(Status::Failed));
        assert_ne!(flow.token, old);
        assert!(!flow.token_ready);
        assert!(flow.take_drain());
        let mut small = Terminal::new(TestBackend::new(19, 3)).unwrap();
        small.draw(|frame| flow.render(frame)).unwrap();
        assert!(!flow.fully_visible);
        for c in flow.token.clone().chars() {
            flow.intercept(key(KeyCode::Char(c)));
        }
        assert!(!flow.token_ready);
        let mut full = Terminal::new(TestBackend::new(100, 20)).unwrap();
        full.draw(|frame| flow.render(frame)).unwrap();
        assert!(flow.fully_visible);
        flow.intercept(Event::FocusLost);
        full.draw(|frame| flow.render(frame)).unwrap();
        for c in flow.token.clone().chars() {
            flow.intercept(key(KeyCode::Char(c)));
        }
        assert!(!flow.token_ready);
        flow.intercept(Event::FocusGained);
        assert!(!flow.fully_visible);
        full.draw(|frame| flow.render(frame)).unwrap();
        for c in flow.token.clone().chars() {
            flow.intercept(key(KeyCode::Char(c)));
        }
        assert!(flow.token_ready);
        assert!(!flow.can_dismiss()); // verified frame must also be presented
        full.draw(|frame| flow.render(frame)).unwrap();
        assert!(flow.can_dismiss());
        small.draw(|frame| flow.render(frame)).unwrap();
        assert!(!flow.can_dismiss());
    }
    #[test]
    fn unexpected_sending_focus_cancels_to_fresh_unknown_quarantine() {
        let mut flow = Flow::loading(request(), 1, super::super::pane::PaneId::Main);
        flow.phase = Phase::Sending;
        flow.fully_visible = true;
        flow.token_ready = true;
        let old = flow.token.clone();
        assert!(matches!(flow.intercept(Event::FocusGained), Action::Cancel));
        assert!(matches!(flow.phase, Phase::Status(UNKNOWN)));
        assert_ne!(flow.token, old);
        assert!(!flow.token_ready);
        assert!(!flow.can_dismiss());
        assert!(flow.take_drain());
        // Scope changes retain quarantine rather than exposing queued tail to chat.
        assert!(flow.scope_matches("replacement-agent", 99, None));
        assert!(matches!(flow.intercept(key(KeyCode::Esc)), Action::None));
    }
    #[test]
    fn review_literals_escape_ansi_del_c1_and_bidi() {
        let text = literal("\x1b[31m\u{7f}\u{85}\u{202e}");
        assert!(!text.chars().any(char::is_control));
        assert!(!text.contains('\u{202e}'));
        assert!(text.contains("\\u{7f}"));
        assert!(text.contains("\\u{85}"));
        assert!(text.contains("\\u{202e}"));
    }
}
