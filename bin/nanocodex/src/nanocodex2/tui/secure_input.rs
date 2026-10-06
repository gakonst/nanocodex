//! One guarded terminal entry point for every private-input family.
use super::{private_input as private, sudo_input as sudo};
use crossterm::event::Event;
use nanocodex_managed::{NativeSecureInputRequest, PrivateInputRequest};
pub(crate) use sudo::{Command, Status, protect_process};
pub(crate) const HELP: &str = "Use /secure-input to open the latest private browser, Vault or sudo input. Never enter credentials in chat.";
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Sudo(NativeSecureInputRequest),
    Private(PrivateInputRequest),
}
impl Request {
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Sudo(r) => &r.request_id,
            Self::Private(r) => &r.request_id,
        }
    }
    pub(crate) fn agent(&self) -> &str {
        match self {
            Self::Sudo(r) => &r.agent_id,
            Self::Private(r) => &r.agent_id,
        }
    }
    pub(crate) fn is_current(&self) -> bool {
        match self {
            Self::Sudo(r) => r.is_current(),
            Self::Private(r) => r.is_current(),
        }
    }
}
pub(crate) enum Flow {
    Sudo(sudo::Flow),
    Private(private::Flow),
}
impl Flow {
    pub(crate) fn loading(request: Request, generation: u64, pane: super::pane::PaneId) -> Self {
        match request {
            Request::Sudo(r) => Self::Sudo(sudo::Flow::loading(r, generation, pane)),
            Request::Private(r) => Self::Private(private::Flow::loading(r, generation, pane)),
        }
    }
    pub(crate) fn request(&self) -> Request {
        match self {
            Self::Sudo(f) => Request::Sudo(f.request.clone()),
            Self::Private(f) => Request::Private(f.request.clone()),
        }
    }
    pub(crate) fn generation(&self) -> u64 {
        match self {
            Self::Sudo(f) => f.generation,
            Self::Private(f) => f.generation,
        }
    }
    pub(crate) fn scope_matches(&self, a: &str, g: u64, p: Option<super::pane::PaneId>) -> bool {
        match self {
            Self::Sudo(f) => f.scope_matches(a, g, p),
            Self::Private(f) => f.scope_matches(a, g, p),
        }
    }
    pub(crate) fn is_sending(&self) -> bool {
        match self {
            Self::Sudo(f) => f.is_sending(),
            Self::Private(f) => f.is_sending(),
        }
    }
    pub(crate) fn can_dismiss(&self) -> bool {
        match self {
            Self::Sudo(f) => f.can_dismiss(),
            Self::Private(f) => f.can_dismiss(),
        }
    }
    pub(crate) fn take_drain(&mut self) -> bool {
        match self {
            Self::Sudo(f) => f.take_drain(),
            Self::Private(f) => f.take_drain(),
        }
    }
    pub(crate) fn cancel_local(&mut self) {
        match self {
            Self::Sudo(f) => f.cancel_local(),
            Self::Private(f) => f.cancel_local(),
        }
    }
    pub(crate) fn finish(&mut self, o: Outcome) {
        match (self, o) {
            (Self::Sudo(f), Outcome::Sudo(o)) => f.finish(o),
            (Self::Private(f), Outcome::Private(o)) => f.finish(o),
            (f, _) => f.cancel_local(),
        }
    }
    pub(crate) fn render(&mut self, f: &mut ratatui::Frame<'_>) {
        match self {
            Self::Sudo(v) => v.render(f),
            Self::Private(v) => v.render(f),
        }
    }
    pub(crate) fn intercept(&mut self, e: Event) -> Action {
        match self {
            Self::Sudo(f) => match f.intercept(e) {
                sudo::Action::None => Action::None,
                sudo::Action::Cancel => Action::Cancel,
                sudo::Action::Dismiss => Action::Dismiss,
                sudo::Action::Submit(v) => Action::Sudo(v),
            },
            Self::Private(f) => f.intercept(e),
        }
    }
}
pub(crate) enum Action {
    None,
    Cancel,
    Dismiss,
    Sudo(nanocodex_managed::NativeSecureInputEnvelope),
    Private(private::Operation),
    Browser,
}
pub(crate) enum Outcome {
    Sudo(sudo::Outcome),
    Private(private::Outcome),
}
pub(crate) type Completion = (String, u64, String, Outcome);

pub(crate) fn parse(value: &serde_json::Value) -> Option<Request> {
    if let Some(r) = NativeSecureInputRequest::parse(value) {
        return Some(Request::Sudo(r));
    }
    fn agent(v: &serde_json::Value, depth: usize) -> Option<String> {
        if depth > 12 {
            return None;
        }
        if let Some(s) = v.as_str() {
            if s.len() > 65536 {
                return None;
            }
            return agent(
                &serde_json::from_str::<serde_json::Value>(
                    nanocodex_managed::private_input_output_text(s),
                )
                .ok()?,
                depth + 1,
            );
        }
        if let Some(a) = v.get("agent_id").and_then(serde_json::Value::as_str) {
            return Some(a.into());
        }
        if let Some(a) = v.as_array() {
            return a.iter().find_map(|v| agent(v, depth + 1));
        }
        ["content", "text", "structuredContent", "result", "output"]
            .iter()
            .find_map(|k| v.get(k).and_then(|v| agent(v, depth + 1)))
    }
    PrivateInputRequest::parse(value, &agent(value, 0).unwrap_or_else(|| "pending".into()))
        .map(Request::Private)
}
pub(crate) fn request(record: &super::transcript::TranscriptRecord) -> Option<Request> {
    if record.kind() != "tool.result" {
        return None;
    }
    let v: serde_json::Value = record.decode_payload().ok()?;
    if v.get("status")?.as_str()? != "completed" {
        return None;
    }
    let tool = v.get("tool")?.as_str()?.split('.').next_back()?;
    if !matches!(
        tool,
        "request_native_secure_input"
            | "request_browser_login"
            | "request_browser_login_input"
            | "browser_vault_request_takeover"
            | "browser_vault_request_challenge"
            | "request_secure_input"
            | "request_vault_intake"
            | "exec"
            | "wait"
    ) {
        return None;
    }
    let mut request = v
        .get("structured_result")
        .and_then(parse)
        .or_else(|| v.get("result").and_then(parse))?;
    if let Request::Private(private) = &mut request
        && matches!(private.kind, nanocodex_managed::PrivateInputKind::Vault(_))
    {
        // Direct receipts and Code Mode echoes share their actual originating turn.
        // Identical intake metadata in a later turn must create a fresh local form.
        let turn = record
            .managed_turn_id()
            .or_else(|| record.agent_request_id())?;
        private.request_id = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            format!("nanocodex:private-intake:{turn}:{}", private.request_id).as_bytes(),
        )
        .to_string();
    }
    Some(request)
}
