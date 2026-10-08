//! Caller-local forms. No entered value implements Debug or enters a component event.
use super::secure_input::Action;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use nanocodex_managed::{
    ManagedClient, PrivateInputBody, PrivateInputKind as Kind, PrivateInputRequest as Request,
    PrivateVaultItem,
};
use ratatui::{
    Frame,
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};
use serde_json::{Value, json};
use zeroize::{Zeroize, Zeroizing};

const SAVE_ROLES: &[&str] = &[
    "username",
    "password",
    "api_key",
    "phone_number",
    "card_number",
    "expiry_month",
    "expiry_year",
    "billing_zip",
    "address_line_1",
    "address_line_2",
    "city",
    "state",
    "zip",
    "country",
];
struct Secret {
    bytes: Zeroizing<Vec<u8>>,
}
impl Secret {
    fn new() -> Self {
        Self {
            bytes: Zeroizing::new(Vec::with_capacity(8192)),
        }
    }
    fn value(&self) -> &str {
        std::str::from_utf8(&self.bytes).unwrap_or("")
    }
    fn push(&mut self, text: &str, multiline: bool, max: usize) {
        if self.bytes.len() + text.len() <= max
            && !text
                .chars()
                .any(|c| c == '\0' || (c.is_control() && !(multiline && matches!(c, '\n' | '\t'))))
        {
            self.bytes.extend_from_slice(text.as_bytes());
        }
    }
    fn pop(&mut self) {
        if let Some((i, _)) = self.value().char_indices().last() {
            self.bytes[i..].zeroize();
            self.bytes.truncate(i);
        }
    }
    fn set(&mut self, value: &str) {
        self.bytes.zeroize();
        self.bytes.clear();
        self.push(value, false, 8192);
    }
}
#[derive(Clone)]
struct Field {
    id: String,
    label: String,
    kind: String,
    multiline: bool,
    options: Vec<(usize, String)>,
    checked: bool,
    max: usize,
    optional: bool,
    initial: String,
    role: Option<&'static str>,
    transient: bool,
    autocomplete: String,
}
pub(crate) struct Form {
    fields: Vec<Field>,
    document: Option<String>,
    origin: String,
    reusable: bool,
    classification: bool,
}
#[derive(Clone)]
struct Mapping {
    item: PrivateVaultItem,
    role: &'static str,
}
struct Picker {
    choices: Vec<Mapping>,
    index: usize,
}
struct Inputs {
    form: Form,
    values: Vec<Secret>,
    index: usize,
    save: bool,
    opted_out: bool,
    mappings: Vec<Option<Mapping>>,
    picker: Option<Picker>,
}
impl Inputs {
    fn new(form: Form) -> Self {
        let values = form
            .fields
            .iter()
            .map(|f| {
                let mut s = Secret::new();
                if f.kind == "checkbox" {
                    s.set(if f.checked { "true" } else { "false" });
                } else {
                    s.set(&f.initial);
                }
                s
            })
            .collect();
        let save = form.reusable;
        let mappings = vec![None; form.fields.len()];
        Self {
            mappings,
            picker: None,
            form,
            values,
            index: 0,
            save,
            opted_out: false,
        }
    }
    fn body(&self, r: &Request) -> Option<PrivateInputBody> {
        if self.mappings.iter().any(Option::is_some) {
            // A reuse submission contains only safe references, never entered values.
            // Mixed manual/reuse entry needs a fresh browser request after this fill.
            if self.form.document.is_none()
                || self
                    .form
                    .fields
                    .iter()
                    .zip(&self.values)
                    .any(|(f, v)| f.kind != "checkbox" && !v.value().is_empty())
            {
                return None;
            }
            return PrivateInputBody::encode(&json!({"challenge_id":r.request_id,
                "action":"fill_vault_fields","document_id":self.form.document,
                "fields":self.form.fields.iter().zip(&self.mappings).filter_map(|(f,m)|
                    m.as_ref().map(|m|json!({"ref":f.id,"vault_id":m.item.id,"field":m.role})))
                    .collect::<Vec<_>>()}))
            .ok();
        }
        let pairs: Vec<_> = self
            .form
            .fields
            .iter()
            .zip(&self.values)
            .filter(|(f, v)| {
                f.id != "__vault_username"
                    && (!matches!(r.kind, Kind::Vault(_) | Kind::Credential(_))
                        || !f.optional
                        || !v.value().is_empty())
            })
            .collect();
        if pairs
            .iter()
            .any(|(f, v)| !f.optional && v.value().is_empty())
        {
            return None;
        }
        if pairs.iter().map(|(_, v)| v.value().len()).sum::<usize>() > 32768 {
            return None;
        }
        let mut value = match &r.kind {
            Kind::Login | Kind::Takeover => {
                json!({"challenge_id":r.request_id,"action":"fill_fields","document_id":self.form.document,"fields":pairs.iter().map(|(f,v)|json!({"ref":f.id,"value":v.value()})).collect::<Vec<_>>(),"save_to_vault":self.save})
            }
            Kind::Password => {
                json!({"request_id":r.request_id,"value":self.values.first()?.value(),"save_to_vault":self.save})
            }
            Kind::Form => {
                json!({"request_id":r.request_id,"values":pairs.iter().map(|(f,v)|(f.id.clone(),Value::String(v.value().into()))).collect::<serde_json::Map<_,_>>(),"save_to_vault":self.save})
            }
            Kind::Otp => {
                let code = self.values.first()?.value();
                if !(4..=10).contains(&code.len()) || !code.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                json!({"challenge_id":r.request_id,"code":code})
            }
            Kind::Connector(_) => return None,
            Kind::Credential(kind) => {
                let mut o = pairs
                    .iter()
                    .map(|(f, v)| (f.id.clone(), Value::String(v.value().into())))
                    .collect::<serde_json::Map<_, _>>();
                if kind == "ssh" {
                    let port = o.remove("port")?.as_str()?.parse::<u16>().ok()?;
                    if port == 0 {
                        return None;
                    }
                    o.insert("port".into(), json!(port));
                    if o.get("private_key")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    {
                        o.remove("private_key");
                        o.insert("generate".into(), json!(true));
                    }
                }
                Value::Object(o)
            }
            Kind::Vault(_) => {
                let mut o = pairs
                    .iter()
                    .map(|(f, v)| (f.id.clone(), Value::String(v.value().into())))
                    .collect::<serde_json::Map<_, _>>();
                if !r.origin.is_empty() {
                    o.insert("browser_origin".into(), json!(r.origin));
                }
                Value::Object(o)
            }
        };
        if self.save && self.form.classification {
            let mut details = json!({});
            let roles: serde_json::Map<String, Value> = self
                .form
                .fields
                .iter()
                .filter_map(|f| {
                    if f.transient || f.id == "__vault_username" {
                        None
                    } else {
                        f.role.map(|role| (f.id.clone(), json!(role)))
                    }
                })
                .collect();
            if !roles.is_empty() {
                details["fields"] = Value::Object(roles);
            }
            if let Some((_, username)) = self
                .form
                .fields
                .iter()
                .zip(&self.values)
                .find(|(f, _)| f.id == "__vault_username")
                && !username.value().is_empty()
            {
                details["username"] = json!(username.value());
            }
            if details.as_object().is_some_and(|o| !o.is_empty()) {
                value["save_details"] = details;
            }
        }
        let result = PrivateInputBody::encode(&value).ok();
        wipe_value(&mut value);
        result
    }
}
fn wipe_value(v: &mut Value) {
    match v {
        Value::String(s) => s.zeroize(),
        Value::Array(a) => a.iter_mut().for_each(wipe_value),
        Value::Object(o) => o.values_mut().for_each(wipe_value),
        _ => {}
    }
}
pub(crate) enum Operation {
    ListVault,
    Open,
    Submit(PrivateInputBody),
    Finish,
    RetryVaultSave,
    Cancel,
}
pub(crate) enum Outcome {
    VaultItems(Vec<PrivateVaultItem>),
    Display(Zeroizing<String>, u64),
    Review(bool),
    Form(Form),
    Fallback,
    Receipt(String, &'static str),
    Failed,
}
enum Phase {
    Display(Zeroizing<String>, u64),
    VaultLoading(Inputs),
    Loading,
    Review(bool),
    Fields(Inputs),
    Fallback,
    Sending,
    Status(&'static str),
}
pub(crate) struct Flow {
    pub(crate) request: Request,
    pub(crate) generation: u64,
    pane: super::pane::PaneId,
    phase: Phase,
    visible: bool,
    focused: bool,
    drain: bool,
    token: String,
    matched: usize,
    ready: bool,
    retry_save: bool,
}
impl Flow {
    pub(crate) fn loading(request: Request, generation: u64, pane: super::pane::PaneId) -> Self {
        Self {
            request,
            generation,
            pane,
            phase: Phase::Loading,
            visible: false,
            focused: true,
            drain: true,
            token: safety_token(),
            matched: 0,
            ready: false,
            retry_save: false,
        }
    }
    /// Phase boundary: discard typeahead and require one rendered frame before
    /// input. The typed safety token is per flow, so it is not re-requested at
    /// every step (review, fields, Vault picker) of the same private request.
    fn reset(&mut self) {
        self.matched = 0;
        self.visible = false;
        self.drain = true;
    }
    pub(crate) fn scope_matches(&self, a: &str, g: u64, p: Option<super::pane::PaneId>) -> bool {
        if let Phase::Display(_, expiry) = &self.phase
            && *expiry <= current_millis()
        {
            return false;
        }
        matches!(self.phase, Phase::Status(_))
            || (a == self.request.agent_id
                && g == self.generation
                && p == Some(self.pane)
                && self.request.is_current())
    }
    pub(crate) fn is_sending(&self) -> bool {
        matches!(self.phase, Phase::Sending | Phase::VaultLoading(_))
    }
    pub(crate) fn can_dismiss(&self) -> bool {
        self.visible && self.focused && self.ready
    }
    pub(crate) fn take_drain(&mut self) -> bool {
        std::mem::take(&mut self.drain)
    }
    pub(crate) fn cancel_local(&mut self) {
        if !matches!(self.phase, Phase::Status(_)) {
            self.phase = Phase::Status(if self.is_sending() {
                "Submission outcome unknown. Check the private session or Vault before another attempt."
            } else {
                "Private input cancelled."
            });
            self.reset();
        }
        self.drain = true;
    }
    pub(crate) fn finish(&mut self, o: Outcome) {
        if let Outcome::VaultItems(items) = o {
            let phase = std::mem::replace(&mut self.phase, Phase::Loading);
            self.phase = match phase {
                Phase::VaultLoading(mut inputs) => {
                    let field = &inputs.form.fields[inputs.index];
                    let choices = items
                        .into_iter()
                        .flat_map(|item| {
                            vault_roles(&item.kind)
                                .iter()
                                .filter(|role| compatible(field, role))
                                .map(|role| Mapping {
                                    item: item.clone(),
                                    role,
                                })
                                .collect::<Vec<_>>()
                        })
                        .collect();
                    inputs.picker = Some(Picker { choices, index: 0 });
                    Phase::Fields(inputs)
                }
                other => other,
            };
            self.reset();
            return;
        }
        if matches!(self.phase, Phase::Status(_))
            && !matches!(o, Outcome::Receipt(_, _) | Outcome::Failed)
        {
            return;
        }
        self.retry_save = match &o {
            Outcome::Receipt(receipt, _) => serde_json::from_str::<Value>(receipt)
                .ok()
                .and_then(|v| v.get("vault_save").cloned())
                .is_some_and(|save| {
                    s(&save, "status") == "failed"
                        && save.get("retryable") == Some(&Value::Bool(true))
                }),
            _ => false,
        };
        self.phase = match o {
            Outcome::Display(text, expiry) => Phase::Display(text, expiry),
            Outcome::VaultItems(_) => unreachable!(),
            Outcome::Review(approved) => Phase::Review(approved),
            Outcome::Form(f) => Phase::Fields(Inputs::new(f)),
            Outcome::Fallback => Phase::Fallback,
            Outcome::Receipt(_, message) => Phase::Status(message),
            Outcome::Failed => Phase::Status(
                "Private operation could not be confirmed. Check the same browser or Vault before retrying.",
            ),
        };
        self.reset();
    }
    pub(crate) fn intercept(&mut self, mut event: Event) -> Action {
        // Switching to a password manager and back must not destroy the form.
        // Input is ignored while unfocused; regaining focus discards typeahead
        // and waits for a fresh frame, keeping entered values masked.
        if matches!(event, Event::FocusGained) {
            self.focused = true;
            self.reset();
            return Action::None;
        }
        if matches!(event, Event::FocusLost) {
            self.focused = false;
            return Action::None;
        }
        let cancel = matches!(&event,Event::Key(k) if k.kind!=KeyEventKind::Release && (k.code==KeyCode::Esc || (k.modifiers.contains(KeyModifiers::CONTROL)&&matches!(k.code,KeyCode::Char('c'|'d'|'z')))));
        if cancel {
            wipe_paste(&mut event);
            if matches!(self.phase, Phase::Status(_)) {
                return if self.can_dismiss() && matches!(event,Event::Key(k)if k.code==KeyCode::Esc)
                {
                    self.drain = true;
                    Action::Dismiss
                } else {
                    Action::None
                };
            }
            self.cancel_local();
            return Action::Cancel;
        }
        if !self.ready
            && !matches!(
                self.phase,
                Phase::Loading | Phase::Sending | Phase::VaultLoading(_)
            )
        {
            if self.visible
                && self.focused
                && let Event::Key(k) = &event
                && k.kind == KeyEventKind::Press
                && k.modifiers == KeyModifiers::NONE
                && let KeyCode::Char(c) = k.code
            {
                if c.is_ascii() && Some(c as u8) == self.token.as_bytes().get(self.matched).copied()
                {
                    self.matched += 1;
                    if self.matched == self.token.len() {
                        self.ready = true;
                        self.visible = false;
                        self.drain = true;
                    }
                } else {
                    self.matched = 0;
                }
            }
            wipe_paste(&mut event);
            return Action::None;
        }
        if !self.visible || !self.focused {
            wipe_paste(&mut event);
            return Action::None;
        }
        let action = match (&mut self.phase, &mut event) {
            (Phase::Status(_), Event::Key(k))
                if self.retry_save
                    && self.request.is_current()
                    && k.kind == KeyEventKind::Press
                    && k.code == KeyCode::F(5)
                    && k.modifiers.is_empty() =>
            {
                self.retry_save = false;
                self.phase = Phase::Sending;
                self.reset();
                Action::Private(Operation::RetryVaultSave)
            }
            (Phase::Review(_), Event::Key(k))
                if k.kind == KeyEventKind::Press
                    && k.code == KeyCode::Enter
                    && k.modifiers == KeyModifiers::CONTROL =>
            {
                self.phase = Phase::Sending;
                self.reset();
                Action::Private(Operation::Open)
            }
            (Phase::Fields(inputs), Event::Paste(text)) => {
                if inputs.picker.is_some() || inputs.mappings[inputs.index].is_some() {
                    text.zeroize();
                    return Action::None;
                }
                let f = &inputs.form.fields[inputs.index];
                inputs.values[inputs.index].push(text, f.multiline, f.max);
                text.zeroize();
                Action::None
            }
            (Phase::Fields(inputs), Event::Key(k)) if k.kind == KeyEventKind::Press => {
                let i = inputs.index;
                let f = &inputs.form.fields[i];
                if let Some(picker) = &mut inputs.picker {
                    match k.code {
                        KeyCode::Up | KeyCode::Left => {
                            picker.index =
                                (picker.index + picker.choices.len()) % (picker.choices.len() + 1)
                        }
                        KeyCode::Down | KeyCode::Right => {
                            picker.index = (picker.index + 1) % (picker.choices.len() + 1)
                        }
                        KeyCode::Enter if k.modifiers.is_empty() => {
                            inputs.mappings[i] = picker
                                .index
                                .checked_sub(1)
                                .and_then(|n| picker.choices.get(n))
                                .cloned();
                            inputs.values[i].set("");
                            inputs.picker = None;
                            self.reset();
                        }
                        _ => {}
                    }
                    Action::None
                } else if k.code == KeyCode::F(3)
                    && inputs.form.document.is_some()
                    && !f.transient
                    && f.id != "__vault_username"
                    && f.kind != "checkbox"
                {
                    let Phase::Fields(inputs) = std::mem::replace(&mut self.phase, Phase::Sending)
                    else {
                        unreachable!()
                    };
                    self.phase = Phase::VaultLoading(inputs);
                    self.reset();
                    Action::Private(Operation::ListVault)
                } else if k.code == KeyCode::Enter && k.modifiers == KeyModifiers::CONTROL {
                    if let Some(body) = inputs.body(&self.request) {
                        self.phase = Phase::Sending;
                        self.reset();
                        Action::Private(Operation::Submit(body))
                    } else {
                        Action::None
                    }
                } else if k.code == KeyCode::F(4)
                    && inputs.form.classification
                    && !f.transient
                    && f.id != "__vault_username"
                    && !matches!(f.kind.as_str(), "checkbox" | "select")
                {
                    let next = f
                        .role
                        .and_then(|r| SAVE_ROLES.iter().position(|s| *s == r))
                        .map_or(0, |i| i + 1);
                    inputs.form.fields[i].role = SAVE_ROLES.get(next).copied();
                    if inputs.form.fields[i].role.is_some() {
                        inputs.form.reusable = true;
                        inputs.save = !inputs.opted_out;
                    }
                    Action::None
                } else if k.code == KeyCode::F(2) && inputs.form.reusable {
                    inputs.save = !inputs.save;
                    inputs.opted_out = !inputs.save;
                    Action::None
                } else if matches!(k.code, KeyCode::Tab | KeyCode::Enter) && k.modifiers.is_empty()
                {
                    inputs.index = (i + 1) % inputs.values.len();
                    Action::None
                } else if k.code == KeyCode::BackTab {
                    inputs.index = (i + inputs.values.len() - 1) % inputs.values.len();
                    Action::None
                } else if k.code == KeyCode::Enter
                    && k.modifiers == KeyModifiers::ALT
                    && f.multiline
                {
                    inputs.values[i].push("\n", true, f.max);
                    Action::None
                } else if inputs.mappings[i].is_some() {
                    Action::None
                } else if k
                    .modifiers
                    .intersection(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
                    .is_empty()
                {
                    match k.code {
                        KeyCode::Char(' ') if f.kind == "checkbox" => {
                            let value = if inputs.values[i].value() == "true" {
                                "false"
                            } else {
                                "true"
                            };
                            inputs.values[i].set(value);
                        }
                        KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
                            if f.kind == "select" =>
                        {
                            if !f.options.is_empty() {
                                let old = f
                                    .options
                                    .iter()
                                    .position(|(option_index, _)| {
                                        option_index.to_string() == inputs.values[i].value()
                                    })
                                    .unwrap_or(0);
                                let next = if matches!(k.code, KeyCode::Left | KeyCode::Up) {
                                    (old + f.options.len() - 1) % f.options.len()
                                } else {
                                    (old + 1) % f.options.len()
                                };
                                inputs.values[i].set(&f.options[next].0.to_string());
                            }
                        }
                        KeyCode::Char(c) if !matches!(f.kind.as_str(), "select" | "checkbox") => {
                            let mut b = [0; 4];
                            inputs.values[i].push(c.encode_utf8(&mut b), f.multiline, f.max);
                            b.zeroize();
                        }
                        KeyCode::Backspace => inputs.values[i].pop(),
                        _ => {}
                    }
                    Action::None
                } else {
                    Action::None
                }
            }
            (Phase::Display(_, _), Event::Key(k))
                if k.kind == KeyEventKind::Press && k.code == KeyCode::F(5) =>
            {
                self.phase = Phase::Loading;
                self.reset();
                Action::Private(Operation::Open)
            }
            (Phase::Fallback, Event::Key(k))
                if k.kind == KeyEventKind::Press
                    && k.code == KeyCode::Char('b')
                    && k.modifiers == KeyModifiers::CONTROL =>
            {
                Action::Browser
            }
            (Phase::Fallback, Event::Key(k))
                if k.kind == KeyEventKind::Press
                    && k.code == KeyCode::Enter
                    && k.modifiers == KeyModifiers::CONTROL =>
            {
                self.phase = Phase::Sending;
                self.reset();
                Action::Private(Operation::Finish)
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
            .title(" Private input · credentials never enter chat ")
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Yellow));
        let body = block.inner(area);
        frame.render_widget(block, area);
        let mut text = format!(
            "Agent: {}\nRequest: {}\nWebsite: {}\n",
            literal(&self.request.agent_id),
            literal(&self.request.request_id),
            if self.request.origin.is_empty() {
                "Your encrypted Vault".into()
            } else {
                literal(&self.request.origin)
            }
        );
        text.push_str(&match &self.phase {
            Phase::Display(value, expiry)=>if *expiry > current_millis() { format!("{}\nF5: check the same attempt · Esc: close", value.as_str()) } else { "Private code expired. Close this panel.".into() },
            Phase::VaultLoading(_)=>"Loading safe Vault item names. No saved values enter this terminal. Esc: cancel.".into(),
            Phase::Loading=>"Loading authenticated private input. Esc: cancel.".into(),
            Phase::Review(approved)=>format!("{}\nAllowed websites: {}\nCtrl+Enter: review accepted; open private fields. Esc: cancel.",if *approved{"This session's website consent is already approved."}else{"Review the requested destination before entering private information."},self.request.allowed_origins.iter().map(|s|literal(s)).collect::<Vec<_>>().join(", ")),
            Phase::Fields(inputs)=>{let f=&inputs.form.fields[inputs.index];let value=&inputs.values[inputs.index];let display=if f.kind=="checkbox"{format!("[{}] Space toggles",if value.value()=="true"{"x"}else{" "})}else if f.kind=="select"{format!("{} · arrows select",f.options.iter().find(|(i,_)|i.to_string()==value.value()).map(|(_,s)|literal(s)).unwrap_or_else(||"Choose an option".into()))}else if value.value().is_empty(){"(empty)".into()}else{"********".into()};format!("Field {}/{}: {}{}\n{}\n{}\nTab / Enter: next · Shift+Tab: previous{}\nCtrl+Enter: submit once · Esc: cancel\n{}",inputs.index+1,inputs.form.fields.len(),literal(&f.label),if f.optional{" (optional)"}else{""},display,if matches!(self.request.kind,Kind::Vault(_)){"This form saves a new item to your encrypted Vault.".into()}else if inputs.form.reusable{format!("[{}] Save to Vault · F2 toggles",if inputs.save{"x"}else{" "})}else{"One-time input · verification codes and security codes are never saved.".into()},if f.multiline{" · Alt+Enter: newline"}else{""},if matches!(self.request.kind,Kind::Login|Kind::Takeover){"Fill fields and hand back this same browser. The agent verifies the result."}else{"Values go directly to the authenticated private endpoint."})},
            Phase::Fallback=>"This page needs browser controls (for example CAPTCHA or passkey).\nCtrl+B: open the same private session in your local browser.\nAfter completing it there, Ctrl+Enter: hand back. Esc: cancel.\nThe terminal does not implement passkeys or browser-only controls.".into(),
            Phase::Sending=>"Sending privately once. Values discarded. No automatic retries.\nEsc cannot undo a submission already sent.".into(),
            Phase::Status(message)=>format!("{message}\nEsc closes this private panel."),
        });
        if self.retry_save && self.request.is_current() && matches!(self.phase, Phase::Status(_)) {
            text.push_str("\nF5: retry Vault saving only. Browser input will not be repeated.");
        }
        if let Phase::Fields(inputs) = &self.phase {
            let f = &inputs.form.fields[inputs.index];
            if inputs.form.document.is_some()
                && !f.transient
                && f.id != "__vault_username"
                && f.kind != "checkbox"
            {
                text.push_str("\nF3: choose saved Vault field (all supported item types)");
            }
            if let Some(picker) = &inputs.picker {
                let choice = picker
                    .index
                    .checked_sub(1)
                    .and_then(|i| picker.choices.get(i));
                text.push_str(&format!(
                    "\nVault picker {}/{}: {}\nArrows: choose · Enter: use for this field",
                    picker.index + 1,
                    picker.choices.len() + 1,
                    choice
                        .map(|m| format!(
                            "{} · {} · {}",
                            literal(&m.item.name),
                            m.item.kind,
                            m.role
                        ))
                        .unwrap_or_else(|| "Enter manually / remove mapping".into())
                ));
            }
            if let Some(m) = &inputs.mappings[inputs.index] {
                text.push_str(&format!(
                    "\nSelected Vault: {} · {}",
                    literal(&m.item.name),
                    m.role
                ));
            }
            if inputs.mappings.iter().any(Option::is_some) {
                text.push_str("\nCtrl+Enter fills selected Vault fields and hands back.\nUnmapped fields stay unchanged. Clear manual entries before reuse.");
            }
            text.push_str(&format!(
                "\nCurrent website: {}",
                literal(&inputs.form.origin)
            ));
            if inputs.form.classification
                && !f.transient
                && f.id != "__vault_username"
                && !matches!(f.kind.as_str(), "checkbox" | "select")
            {
                text.push_str(&format!(
                    "\nSave field as: {} · F4 changes role",
                    f.role.unwrap_or("automatic")
                ));
            }
        }
        if !matches!(
            self.phase,
            Phase::Loading | Phase::Sending | Phase::VaultLoading(_)
        ) {
            text.push_str(&if self.ready {
                "\n\nSafety token verified. Controls above are now enabled.".into()
            } else {
                format!(
                    "\n\nType {} to enable input (typed, not pasted).",
                    self.token
                )
            });
        }
        let lines = super::vault::review_lines(&text, body.width);
        self.visible = body.width >= 20 && lines.len() <= usize::from(body.height);
        frame.render_widget(
            Paragraph::new(if self.visible {
                lines.join("\n")
            } else {
                "Enlarge terminal to review all private input details. Input disabled. Esc cancels."
                    .into()
            }),
            body,
        );
    }
}
fn wipe_paste(e: &mut Event) {
    if let Event::Paste(s) = e {
        s.zeroize();
    }
}
fn literal(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default().chars().map(|c|if c.is_control()||matches!(c,'\u{061c}'|'\u{200b}'..='\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2060}'..='\u{206f}'|'\u{feff}'){format!("\\u{{{:x}}}",c as u32)}else{c.to_string()}).collect()
}
fn vault_roles(kind: &str) -> &'static [&'static str] {
    match kind {
        "login" => &["username", "password"],
        "api_key" => &["api_key"],
        "card" => &[
            "card_number",
            "expiry_month",
            "expiry_year",
            "card_expiry",
            "billing_zip",
        ],
        "address" => &[
            "address_line_1",
            "address_line_2",
            "city",
            "state",
            "zip",
            "country",
        ],
        "phone" => &["phone_number"],
        _ => &[],
    }
}
fn compatible(f: &Field, role: &str) -> bool {
    if f.transient || f.autocomplete == "new-password" || f.id == "__vault_username" {
        return false;
    }
    let hint = f.autocomplete.as_str();
    match role {
        "password" => f.kind == "password",
        "username" => {
            matches!(f.kind.as_str(), "text" | "email")
                && matches!(hint, "" | "off" | "on" | "username" | "email")
        }
        "api_key" => {
            matches!(f.kind.as_str(), "text" | "password") && matches!(hint, "" | "off" | "on")
        }
        _ => {
            (match role {
                "card_number" => hint == "cc-number",
                "expiry_month" => hint == "cc-exp-month",
                "expiry_year" => hint == "cc-exp-year",
                "card_expiry" => hint == "cc-exp",
                "billing_zip" | "zip" => hint == "postal-code",
                "address_line_1" => matches!(hint, "address-line1" | "street-address"),
                "address_line_2" => hint == "address-line2",
                "city" => hint == "address-level2",
                "state" => hint == "address-level1",
                "country" => matches!(hint, "country" | "country-name"),
                "phone_number" => hint == "tel",
                _ => false,
            }) && matches!(
                f.kind.as_str(),
                "text" | "tel" | "number" | "email" | "select"
            )
        }
    }
}
fn field(id: &str, label: &str, kind: &str) -> Field {
    Field {
        id: id.into(),
        label: label.into(),
        kind: kind.into(),
        multiline: false,
        options: vec![],
        checked: false,
        max: 4096,
        optional: false,
        initial: String::new(),
        role: None,
        transient: matches!(kind, "otp" | "card_cvc"),
        autocomplete: String::new(),
    }
}
fn local_form(r: &Request) -> Option<Form> {
    let mut fields = match &r.kind {
        Kind::Otp => vec![field("code", "Verification code", "otp")],
        Kind::Credential(kind) => {
            if kind == "openai" {
                vec![field("api_key", "OpenAI API key", "password")]
            } else if kind == "ssh" {
                vec![
                    field("hostname", "Server hostname", "text"),
                    field("port", "SSH port", "text"),
                    field("username", "SSH username", "text"),
                    field("host_key_sha256", "Trusted host SHA256 fingerprint", "text"),
                    field(
                        "private_key",
                        "Private PEM key (empty generates in Vault)",
                        "password",
                    ),
                ]
            } else {
                return None;
            }
        }
        Kind::Vault(kind) => {
            let mut f = vec![field("name", "Vault item name", "text")];
            f[0].initial = r.name.clone();
            f[0].max = 120;
            let entries: &[(&str, &str, &str)] = match kind.as_str() {
                "login" => &[
                    ("username", "Username", "text"),
                    ("password", "Password", "password"),
                ],
                "api_key" => &[("api_key", "API key", "password")],
                "card" => &[
                    ("card_number", "Card number", "password"),
                    ("expiry_month", "Expiry month", "text"),
                    ("expiry_year", "Expiry year", "text"),
                    ("billing_zip", "Billing postal code", "text"),
                ],
                "address" => &[
                    ("address_line_1", "Address", "text"),
                    ("address_line_2", "Address line 2", "text"),
                    ("city", "City", "text"),
                    ("state", "State", "text"),
                    ("zip", "Postal code", "text"),
                    ("country", "Country", "text"),
                ],
                "phone" => &[("phone_number", "Phone number", "text")],
                _ => return None,
            };
            f.extend(entries.iter().map(|(i, l, k)| field(i, l, k)));
            f
        }
        _ => return None,
    };
    for f in &mut fields {
        f.optional = matches!(f.id.as_str(), "address_line_2" | "private_key");
        if f.id == "private_key" {
            f.multiline = true;
            f.max = 32768;
        }
        if f.id == "port" {
            f.initial = "22".into();
        }
        if matches!(f.id.as_str(), "api_key" | "password") {
            f.max = 8192;
        }
    }
    Some(Form {
        fields,
        document: None,
        origin: r.origin.clone(),
        reusable: false,
        classification: false,
    })
}
fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}
fn parse_form(r: &Request, v: &Value) -> Option<Form> {
    let native = matches!(r.kind, Kind::Login | Kind::Takeover);
    if native && s(v, "status") != "active" {
        return None;
    }
    let origin = if r.kind == Kind::Login {
        s(v, "origin")
    } else {
        &r.origin
    };
    if r.kind == Kind::Login && !r.allowed_origins.iter().any(|s| s == origin) {
        return None;
    }
    let form = if native { v.get("native_form")? } else { v };
    if !native
        && (s(v, "request_id") != r.request_id
            || s(v, "origin") != r.origin
            || v.get("expires_at").and_then(Value::as_u64) != r.expires_at)
    {
        return None;
    }
    let document = if native {
        let id = s(form, "document_id");
        uuid::Uuid::parse_str(id).ok()?;
        Some(id.into())
    } else {
        None
    };
    let raw = form.get("fields")?.as_array()?;
    if raw.is_empty() || raw.len() > if native { 32 } else { 8 } {
        return None;
    }
    let mut fields = Vec::new();
    let mut reusable = false;
    for entry in raw {
        let id = s(entry, if native { "ref" } else { "id" });
        if id.is_empty() || id.len() > 64 || fields.iter().any(|f: &Field| f.id == id) {
            return None;
        }
        if native {
            uuid::Uuid::parse_str(id).ok()?;
        }
        let kind = s(entry, if native { "type" } else { "kind" });
        if !(if native {
            [
                "text", "email", "url", "tel", "number", "password", "select", "checkbox",
            ]
            .contains(&kind)
        } else {
            [
                "password",
                "card_number",
                "card_expiry",
                "card_cvc",
                "sensitive_text",
            ]
            .contains(&kind)
        }) {
            return None;
        }
        let label = if native {
            s(entry, "label")
        } else {
            match kind {
                "password" => "Password",
                "card_number" => "Card number",
                "card_expiry" => "Expiry date",
                "card_cvc" => "Security code",
                _ => "Sensitive value",
            }
        };
        if label.is_empty() || label.len() > 640 || label.chars().any(char::is_control) {
            return None;
        }
        let mut f = field(id, label, kind);
        f.multiline = entry.get("multiline") == Some(&Value::Bool(true));
        if f.multiline && kind != "text" {
            return None;
        }
        if kind == "select" {
            for opt in entry.get("options")?.as_array()? {
                let i = opt.get("index")?.as_u64()? as usize;
                let l = s(opt, "label");
                if i >= 200
                    || l.len() > 640
                    || l.chars().any(char::is_control)
                    || f.options.last().is_some_and(|(old, _)| *old >= i)
                {
                    return None;
                }
                f.options.push((i, l.into()));
            }
        }
        if kind == "checkbox" {
            f.checked = entry.get("checked")?.as_bool()?;
        }
        // Native controls accept empty values (including optional text/notes).
        // Keep every selected native ref in the submission; the browser owns validity.
        f.optional = native && kind != "select";
        let hint = s(entry, "autocomplete");
        f.autocomplete = hint.into();
        f.transient |= matches!(hint, "one-time-code" | "cc-csc");
        reusable |= !matches!(hint, "one-time-code" | "cc-csc")
            && (matches!(kind, "password" | "email" | "tel" | "card_number")
                || matches!(
                    hint,
                    "username"
                        | "email"
                        | "tel"
                        | "cc-number"
                        | "cc-exp"
                        | "cc-exp-month"
                        | "cc-exp-year"
                        | "postal-code"
                        | "street-address"
                        | "address-line1"
                        | "address-line2"
                        | "address-level1"
                        | "address-level2"
                        | "country"
                        | "country-name"
                ));
        fields.push(f);
    }
    if fields.iter().any(|f| f.kind == "password" && !f.transient) {
        let mut username = field(
            "__vault_username",
            "Username for Vault only (optional; for password-only forms)",
            "text",
        );
        username.optional = true;
        username.max = 512;
        fields.push(username);
    }
    Some(Form {
        fields,
        document,
        origin: origin.into(),
        reusable,
        classification: true,
    })
}
async fn post(c: &ManagedClient, r: &Request, v: Value) -> Option<Value> {
    c.private_input_post(r, PrivateInputBody::encode(&v).ok()?)
        .await
        .ok()
}
pub(crate) async fn describe(c: &ManagedClient, r: &Request) -> Outcome {
    if matches!(r.kind, Kind::Connector(_)) {
        return connector_display(c, r, true).await;
    }
    if let Some(form) = local_form(r) {
        return Outcome::Form(form);
    }
    if r.kind == Kind::Login {
        let Some(v) = post(c, r, r.control("describe")).await else {
            return Outcome::Failed;
        };
        let Some(current) = Request::parse(&v, &r.agent_id) else {
            return Outcome::Failed;
        };
        if current != *r {
            return Outcome::Failed;
        }
        return match v.get("approved").and_then(Value::as_bool) {
            Some(b) => Outcome::Review(b),
            None => Outcome::Failed,
        };
    }
    if r.kind == Kind::Takeover {
        let Some(v) = post(c, r, r.control("describe")).await else {
            return Outcome::Failed;
        };
        if Request::parse(&v, &r.agent_id).as_ref() != Some(r) {
            return Outcome::Failed;
        }
        return Outcome::Review(true);
    }
    match post(c, r, r.control("describe"))
        .await
        .and_then(|v| parse_form(r, &v))
    {
        Some(form) => Outcome::Form(form),
        None => Outcome::Failed,
    }
}
pub(crate) async fn run(c: &ManagedClient, r: &Request, operation: Operation) -> Outcome {
    match operation {
        Operation::ListVault => match c.private_vault_items().await {
            Ok(items) => Outcome::VaultItems(items),
            Err(_) => Outcome::Failed,
        },
        Operation::Open => {
            if matches!(r.kind, Kind::Connector(_)) {
                return connector_display(c, r, false).await;
            }
            if r.kind == Kind::Login {
                let Some(v) = post(c, r, r.control("describe")).await else {
                    return Outcome::Failed;
                };
                let Some(current) = Request::parse(&v, &r.agent_id) else {
                    return Outcome::Failed;
                };
                if current != *r {
                    return Outcome::Failed;
                }
                if v.get("approved") == Some(&Value::Bool(false))
                    && !post(c, r, r.control("approve"))
                        .await
                        .is_some_and(|v| s(&v, "status") == "approved")
                {
                    return Outcome::Failed;
                }
            }
            let mut body = r.control("observe");
            body["native_fields"] = json!(true);
            body["native_field_hints"] = json!(true);
            body["native_field_controls"] = json!(true);
            // The terminal never renders the frame; request the compact encoding.
            body["image_format"] = json!("jpeg");
            let Some(v) = post(c, r, body).await else {
                return Outcome::Failed;
            };
            if s(&v, "native_form_status") == "stale" {
                return finish(c, r, true).await;
            }
            if v.get("native_form").is_none() && s(&v, "status") == "active" {
                return Outcome::Fallback;
            }
            match parse_form(r, &v) {
                Some(f) => Outcome::Form(f),
                None => Outcome::Failed,
            }
        }
        Operation::Submit(body) => {
            let Ok(v) = c.private_input_post(r, body).await else {
                return Outcome::Failed;
            };
            match &r.kind {
                Kind::Login | Kind::Takeover => {
                    if s(&v, "status") != "active" {
                        return Outcome::Failed;
                    }
                    finish(c, r, s(&v, "native_form_status") == "stale").await
                }
                Kind::Credential(_) => Outcome::Receipt(json!({"type":"private_input_receipt","request_id":r.request_id,"status":"saved"}).to_string(),"Account credential saved."),
                Kind::Vault(kind) => {
                    let id = s(&v, "id");
                    if s(&v, "kind") != kind
                        || !(22..=64).contains(&id.len())
                        || !id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                    {
                        return Outcome::Failed;
                    }
                    Outcome::Receipt(json!({"type":"vault_intake_receipt","status":"saved","operation":"create","id":id,"kind":kind}).to_string(),"Saved to your encrypted Vault.")
                }
                Kind::Otp => {
                    if s(&v, "type") != "browser_vault_challenge_receipt"
                        || s(&v, "challenge_id") != r.request_id
                        || s(&v, "status") != "submitted"
                    {
                        return Outcome::Failed;
                    }
                    Outcome::Receipt(json!({"type":"browser_vault_challenge_receipt","challenge_id":r.request_id,"status":"submitted"}).to_string(),"Verification code submitted. The agent must verify the result.")
                }
                _ => {
                    let status = s(&v, "status");
                    if s(&v, "type") != "secure_input_receipt"
                        || s(&v, "request_id") != r.request_id
                        || !["filled", "submitted", "action_required", "outcome_unknown"]
                            .contains(&status)
                    {
                        return Outcome::Failed;
                    }
                    {
                        let mut receipt = json!({"type":"secure_input_receipt","request_id":r.request_id,"status":status});
                        let message = project_save(
                            &v,
                            &mut receipt,
                            "Private input delivered. The agent must verify the browser result.",
                        );
                        Outcome::Receipt(receipt.to_string(), message)
                    }
                }
            }
        }
        Operation::Finish => finish(c, r, false).await,
        Operation::RetryVaultSave => {
            let Some(v) = post(c, r, r.control("retry_vault_save")).await else {
                return Outcome::Failed;
            };
            if v.get("vault_save").is_none() {
                return Outcome::Failed;
            }
            let mut receipt =
                json!({"type":"private_vault_save_receipt", "request_id":r.request_id});
            let message = project_save(&v, &mut receipt, "Vault saving could not be confirmed.");
            Outcome::Receipt(receipt.to_string(), message)
        }
        Operation::Cancel => {
            if matches!(
                r.kind,
                Kind::Vault(_) | Kind::Credential(_) | Kind::Connector(_) | Kind::Otp
            ) {
                return Outcome::Receipt(json!({"type":"private_input_receipt","request_id":r.request_id,"status":"cancelled"}).to_string(),"Private input cancelled.");
            }
            if r.kind == Kind::Takeover {
                let Some(v) = post(c, r, r.control("cancel")).await else {
                    return Outcome::Failed;
                };
                if s(&v, "status") != "cancelled" {
                    return Outcome::Failed;
                }
                return Outcome::Receipt(json!({"type":"browser_vault_takeover_receipt","challenge_id":r.request_id,"status":"cancelled"}).to_string(),"Private browser input cancelled.");
            }
            let Some(v) = post(c, r, r.control("cancel")).await else {
                return Outcome::Failed;
            };
            let typ = if r.kind == Kind::Login {
                "browser_login_receipt"
            } else {
                "secure_input_receipt"
            };
            if s(&v, "type") != typ
                || s(&v, "request_id") != r.request_id
                || s(&v, "status") != "cancelled"
            {
                return Outcome::Failed;
            }
            Outcome::Receipt(
                json!({"type":typ,"request_id":r.request_id,"status":"cancelled"}).to_string(),
                "Private input cancelled.",
            )
        }
    }
}
async fn finish(c: &ManagedClient, r: &Request, stale: bool) -> Outcome {
    let Some(v) = post(c, r, r.control("finish")).await else {
        return Outcome::Failed;
    };
    if s(&v, "status") != "finished"
        || (r.kind == Kind::Login
            && (s(&v, "request_id") != r.request_id || s(&v, "type") != "browser_login_receipt"))
    {
        return Outcome::Failed;
    }
    let mut receipt = if r.kind == Kind::Login {
        json!({"type":"browser_login_receipt","request_id":r.request_id,"status":"finished"})
    } else {
        json!({"type":"browser_vault_takeover_receipt","challenge_id":r.request_id,"status":"finished"})
    };
    if stale {
        receipt["input_outcome"] = json!("page_changed");
    }
    let message = project_save(
        &v,
        &mut receipt,
        if stale {
            "The page changed. No input applied. Handed back for a fresh request."
        } else {
            "Private browser handed back. The agent must verify the result."
        },
    );
    Outcome::Receipt(receipt.to_string(), message)
}

// Project only bounded metadata. Never forward backend strings or raw bodies to chat.
fn project_save(v: &Value, receipt: &mut Value, default: &'static str) -> &'static str {
    let Some(save) = v.get("vault_save") else {
        return default;
    };
    let status = s(save, "status");
    if !matches!(status, "saved" | "not_saved" | "failed") {
        receipt["vault_save"] = json!({"status":"unknown"});
        return "Input delivered. Vault saving could not be confirmed. Check Vault before retrying.";
    }
    let mut safe = json!({"status":status});
    if save.get("retryable") == Some(&Value::Bool(true)) {
        safe["retryable"] = json!(true);
    }
    if let Some(items) = save.get("items").and_then(Value::as_array) {
        let projected: Vec<_> = items
            .iter()
            .take(5)
            .filter_map(|item| {
                let id = s(item, "id");
                let kind = s(item, "kind");
                if !(22..=64).contains(&id.len())
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                    || !matches!(kind, "login" | "api_key" | "card" | "address" | "phone")
                {
                    return None;
                }
                Some(json!({"id":id,"kind":kind}))
            })
            .collect();
        safe["items"] = json!(projected);
    }
    if status == "saved"
        && !safe
            .get("items")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
    {
        receipt["vault_save"] = json!({"status":"unknown"});
        return "Input delivered. Vault saving could not be confirmed. Check Vault before retrying.";
    }
    receipt["vault_save"] = safe;
    match status {
        "saved" => "Input delivered and saved to Vault. The agent must verify the browser result.",
        "not_saved" => {
            "Input delivered. No complete reusable item was saved; a later login step may complete it."
        }
        _ => {
            "Input delivered, but Vault saving failed. Do not repeat browser input to retry saving."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    const ID: &str = "11111111-1111-4111-8111-111111111111";
    const FIELD: &str = "22222222-2222-4222-8222-222222222222";
    fn request(kind: Kind) -> Request {
        Request {
            request_id: ID.into(),
            agent_id: "agent".into(),
            origin: "https://example.test".into(),
            expires_at: Some(9_000_000_000_000),
            kind,
            allowed_origins: vec!["https://example.test".into()],
            name: "Fixture".into(),
        }
    }
    fn native(entries: Value) -> Form {
        parse_form(
            &request(Kind::Login),
            &json!({"status":"active","origin":"https://example.test",
            "native_form":{"document_id":ID,"fields":entries}}),
        )
        .unwrap()
    }
    fn flow(form: Form) -> Flow {
        let mut flow = Flow::loading(request(Kind::Login), 1, super::super::pane::PaneId::Main);
        flow.finish(Outcome::Form(form));
        flow.visible = true;
        flow.ready = true;
        flow.drain = false;
        flow
    }
    fn key(flow: &mut Flow, code: KeyCode, modifiers: KeyModifiers) -> Action {
        flow.intercept(Event::Key(KeyEvent::new(code, modifiers)))
    }
    async fn capture(body: PrivateInputBody, r: &Request) -> Value {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut data = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).await.unwrap();
                assert_ne!(n, 0);
                data.extend_from_slice(&chunk[..n]);
                if let Some(end) = data.windows(4).position(|s| s == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&data[..end]).unwrap();
                    assert!(headers.contains("authorization: Bearer ncx_live_"));
                    let len: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if data.len() >= end + 4 + len {
                        let v = serde_json::from_slice(&data[end + 4..end + 4 + len]).unwrap();
                        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.unwrap();
                        return v;
                    }
                }
            }
        });
        let key = nanocodex_managed::ManagedApiKey::parse(format!(
            "ncx_live_{}_{}",
            "a".repeat(12),
            "b".repeat(43)
        ))
        .unwrap();
        let client = ManagedClient::new(format!("http://{address}"), key).unwrap();
        client.private_input_post(r, body).await.unwrap();
        server.await.unwrap()
    }
    #[tokio::test]
    async fn empty_native_notes_remain_in_private_payload() {
        let mut inputs = Inputs::new(native(json!([
            {"ref":FIELD,"type":"text","label":"Optional notes","multiline":true},
            {"ref":ID,"type":"checkbox","label":"Preference","checked":false}
        ])));
        assert!(inputs.form.fields[0].optional);
        let body = capture(
            inputs.body(&request(Kind::Login)).unwrap(),
            &request(Kind::Login),
        )
        .await;
        assert_eq!(
            body["fields"],
            json!([{"ref":FIELD,"value":""},{"ref":ID,"value":"false"}])
        );
        inputs.values[0].push("line one\nline two", true, 4096);
        assert!(inputs.body(&request(Kind::Login)).is_some());
        let vault = Inputs::new(local_form(&request(Kind::Vault("login".into()))).unwrap());
        assert!(vault.body(&request(Kind::Vault("login".into()))).is_none());
        let otp = Inputs::new(local_form(&request(Kind::Otp)).unwrap());
        assert!(otp.body(&request(Kind::Otp)).is_none());
    }
    #[test]
    fn select_navigation_uses_current_field_and_preserves_sparse_indices() {
        let mut flow = flow(native(json!([
            {"ref":FIELD,"type":"text","label":"Notes"},
            {"ref":ID,"type":"select","label":"Country","options":[{"index":2,"label":"A"},{"index":7,"label":"B"}]}
        ])));
        key(&mut flow, KeyCode::Tab, KeyModifiers::NONE);
        key(&mut flow, KeyCode::Right, KeyModifiers::NONE);
        let Phase::Fields(ref inputs) = flow.phase else {
            panic!()
        };
        assert_eq!(inputs.values[1].value(), "7");
        key(&mut flow, KeyCode::Left, KeyModifiers::NONE);
        let Phase::Fields(inputs) = flow.phase else {
            panic!()
        };
        assert_eq!(inputs.values[1].value(), "2");
    }
    #[test]
    fn saving_defaults_on_and_classification_preserves_explicit_optout() {
        let mut flow = flow(native(
            json!([{ "ref":FIELD,"type":"password","label":"Password","autocomplete":"current-password" }]),
        ));
        let Phase::Fields(ref inputs) = flow.phase else {
            panic!()
        };
        assert!(inputs.save);
        key(&mut flow, KeyCode::F(2), KeyModifiers::NONE);
        key(&mut flow, KeyCode::F(4), KeyModifiers::NONE);
        let Phase::Fields(inputs) = flow.phase else {
            panic!()
        };
        assert!(!inputs.save);
    }
    #[tokio::test]
    async fn all_five_vault_kinds_send_only_safe_mappings() {
        for (kind, role, input, hint) in [
            ("login", "password", "password", "current-password"),
            ("api_key", "api_key", "text", ""),
            ("card", "card_number", "text", "cc-number"),
            ("address", "city", "text", "address-level2"),
            ("phone", "phone_number", "tel", "tel"),
        ] {
            let mut inputs = Inputs::new(native(
                json!([{ "ref":FIELD,"type":input,"label":"Fixture","autocomplete":hint }]),
            ));
            assert!(vault_roles(kind).contains(&role));
            assert!(compatible(&inputs.form.fields[0], role));
            inputs.mappings[0] = Some(Mapping {
                item: PrivateVaultItem {
                    id: "fixture_safe_id_1234567890".into(),
                    name: "Fixture".into(),
                    kind: kind.into(),
                },
                role,
            });
            let body = capture(
                inputs.body(&request(Kind::Login)).unwrap(),
                &request(Kind::Login),
            )
            .await;
            assert_eq!(body["action"], "fill_vault_fields");
            assert_eq!(
                body["fields"],
                json!([{"ref":FIELD,"vault_id":"fixture_safe_id_1234567890","field":role}])
            );
            assert!(body.get("save_to_vault").is_none());
            inputs.values[0].set("manual-private-canary");
            assert!(inputs.body(&request(Kind::Login)).is_none());
        }
    }
    #[test]
    fn otp_cvc_and_new_password_do_not_offer_vault_reuse() {
        for hint in ["one-time-code", "cc-csc", "new-password"] {
            let form = native(
                json!([{ "ref":FIELD,"type":"password","label":"Fixture","autocomplete":hint }]),
            );
            assert!(!compatible(&form.fields[0], "password"));
        }
    }
    #[test]
    fn pasted_token_and_queued_secret_never_unlock_or_echo() {
        let mut flow = flow(native(
            json!([{ "ref":FIELD,"type":"text","label":"Notes" }]),
        ));
        flow.reset();
        flow.visible = true;
        flow.intercept(Event::Paste(flow.token.clone()));
        assert!(!flow.ready);
        flow.intercept(Event::Paste("private-canary".into()));
        let Phase::Fields(ref inputs) = flow.phase else {
            panic!()
        };
        assert!(inputs.values[0].value().is_empty());
        for c in flow.token.clone().chars() {
            key(&mut flow, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(flow.ready);
        assert!(!flow.visible);
        assert!(flow.take_drain());
        flow.intercept(Event::Paste("queued-private-canary".into()));
        let Phase::Fields(ref inputs) = flow.phase else {
            panic!()
        };
        assert!(inputs.values[0].value().is_empty());
        flow.visible = true;
        flow.intercept(Event::Paste("entered-private-canary".into()));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 50)).unwrap();
        terminal.draw(|frame| flow.render(frame)).unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!screen.contains("private-canary"));
        assert!(screen.contains("********"));
        // Focus changes keep the masked form; input is ignored while unfocused.
        flow.intercept(Event::FocusLost);
        assert!(matches!(flow.phase, Phase::Fields(_)));
        flow.intercept(Event::Paste("unfocused-canary".into()));
        flow.intercept(Event::FocusGained);
        assert!(flow.take_drain());
        let Phase::Fields(ref inputs) = flow.phase else {
            panic!()
        };
        assert_eq!(inputs.values[0].value(), "entered-private-canary");
        assert!(flow.ready);
    }
    #[test]
    fn saved_status_requires_safe_item_evidence_and_drops_arbitrary_strings() {
        let mut receipt = json!({});
        let message = project_save(
            &json!({"vault_save":{"status":"saved","items":[{"id":"bad","kind":"login","name":"private-canary"}]}}),
            &mut receipt,
            "default",
        );
        assert_eq!(receipt["vault_save"]["status"], "unknown");
        assert!(!message.contains("and saved"));
        project_save(
            &json!({"vault_save":{"status":"saved","items":[{"id":"fixture_safe_id_1234567890","kind":"login","name":"private-canary"}],"reason":"private-canary"}}),
            &mut receipt,
            "default",
        );
        assert_eq!(receipt["vault_save"]["status"], "saved");
        assert!(!receipt.to_string().contains("private-canary"));
    }
}

/// Short per-flow token: blocks pasted or prequeued keystrokes from arming the
/// panel without making the user retype a 32-character UUID at every step.
fn safety_token() -> String {
    let n = uuid::Uuid::new_v4().as_u128();
    (0..4)
        .map(|i| char::from(b'0' + ((n >> (i * 8)) % 10) as u8))
        .collect()
}
fn current_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
async fn connector_display(c: &ManagedClient, r: &Request, start: bool) -> Outcome {
    let Ok(mut value) = c.private_connector_input(r, start).await else {
        return Outcome::Failed;
    };
    let outcome = match &r.kind {
        Kind::Connector(kind) if kind == "whatsapp" => {
            let code = s(&value, "code");
            let expiry = value["expires_at"].as_u64().unwrap_or(0);
            if s(&value, "operation_id") != r.name
                || expiry <= current_millis()
                || !(4..=32).contains(&code.len())
                || !code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                Outcome::Failed
            } else {
                Outcome::Display(
                    Zeroizing::new(format!(
                        "WhatsApp pairing code: {code}\nIn WhatsApp: Settings → Linked devices → Link with phone number.\nAfter linking, close this panel and use /connectors list to verify."
                    )),
                    expiry.min(r.expires_at.unwrap_or(expiry)),
                )
            }
        }
        Kind::Connector(kind) if kind == "chatgpt" => {
            if s(&value, "state") == "authenticated" {
                Outcome::Receipt(
                    "{\"status\":\"connected\",\"provider\":\"chatgpt\"}".into(),
                    "ChatGPT connected.",
                )
            } else if s(&value, "state") == "pending" {
                let code = s(&value, "user_code");
                let expiry = value["expires_at"].as_u64().unwrap_or(0);
                let link = s(&value, "verification_url");
                let valid_link = reqwest::Url::parse(link).is_ok_and(|u| {
                    u.scheme() == "https"
                        && u.host_str() == Some("auth.openai.com")
                        && u.username().is_empty()
                        && u.password().is_none()
                });
                if expiry <= current_millis()
                    || !valid_link
                    || !(4..=32).contains(&code.len())
                    || !code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    Outcome::Failed
                } else {
                    Outcome::Display(
                        Zeroizing::new(format!(
                            "Open {link}\nDevice code: {code}\nAuthorize the device, then press F5 to verify."
                        )),
                        expiry.min(r.expires_at.unwrap_or(expiry)),
                    )
                }
            } else {
                Outcome::Failed
            }
        }
        _ => Outcome::Failed,
    };
    wipe_value(&mut value);
    outcome
}

#[cfg(test)]
mod account_management_journeys {
    use super::*;
    use axum::{Json, Router, extract::Request as HttpRequest, response::IntoResponse};
    use std::sync::{Arc, Mutex};
    #[tokio::test]
    async fn private_account_forms_and_provider_panels_use_owner_http_without_transcript_codes() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let observed = calls.clone();
        let expiry = current_millis() + 120_000;
        let app = Router::new().fallback(move |request: HttpRequest| {
            let calls = observed.clone();
            async move {
                assert!(request.headers()["authorization"].to_str().unwrap().starts_with("Bearer ncx_live_"));
                let path = request.uri().path().to_owned();
                let method = request.method().to_string();
                let query = request.uri().query().unwrap_or_default().to_owned();
                let body = axum::body::to_bytes(request.into_body(), 32768).await.unwrap();
                calls.lock().unwrap().push((method.clone(), path.clone(), query, body.to_vec()));
                if path == "/v1/credentials/openai" || path.starts_with("/v1/credentials/ssh/") {
                    return axum::http::StatusCode::NO_CONTENT.into_response();
                }
                if path == "/v1/connectors/whatsapp/pairing" {
                    return Json(json!({"operation_id":"11111111-1111-4111-8111-111111111111","code":"TEST-PAIR","expires_at":expiry})).into_response();
                }
                if method == "POST" { Json(json!({"state":"pending","user_code":"TEST-DEVICE","verification_url":"https://auth.openai.com/codex/device","expires_at":expiry,"poll_after_ms":1000})).into_response() }
                else { Json(json!({"state":"authenticated"})).into_response() }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let key = nanocodex_managed::ManagedApiKey::parse(format!(
            "ncx_live_{}_{}",
            "a".repeat(12),
            "b".repeat(43)
        ))
        .unwrap();
        let client = ManagedClient::new(origin, key).unwrap();
        let make = |kind, name: &str| Request {
            request_id: uuid::Uuid::new_v4().to_string(),
            agent_id: "synthetic-agent".into(),
            origin: String::new(),
            expires_at: Some(expiry),
            kind,
            allowed_origins: vec![],
            name: name.into(),
        };
        let openai = make(Kind::Credential("openai".into()), "");
        let mut inputs = Inputs::new(local_form(&openai).unwrap());
        inputs.values[0].set("synthetic-private-key");
        let saved = run(
            &client,
            &openai,
            Operation::Submit(inputs.body(&openai).unwrap()),
        )
        .await;
        assert!(matches!(saved,Outcome::Receipt(ref r,_) if !r.contains("synthetic-private-key")));
        let ssh = make(Kind::Credential("ssh".into()), "synthetic-server");
        let mut inputs = Inputs::new(local_form(&ssh).unwrap());
        for (i, value) in ["server.example", "22", "user", "SHA256:synthetic", ""]
            .iter()
            .enumerate()
        {
            inputs.values[i].set(value);
        }
        assert!(matches!(
            run(&client, &ssh, Operation::Submit(inputs.body(&ssh).unwrap())).await,
            Outcome::Receipt(_, _)
        ));
        let pairing = make(
            Kind::Connector("whatsapp".into()),
            "11111111-1111-4111-8111-111111111111",
        );
        let mut panel = Flow::loading(pairing.clone(), 1, super::super::pane::PaneId::Main);
        panel.finish(describe(&client, &pairing).await);
        assert!(matches!(&panel.phase,Phase::Display(code,_) if code.contains("TEST-PAIR")));
        panel.intercept(Event::FocusLost);
        assert!(matches!(panel.phase, Phase::Status(_)));
        let chat = make(Kind::Connector("chatgpt".into()), "start");
        assert!(
            matches!(describe(&client,&chat).await,Outcome::Display(code,_) if code.contains("TEST-DEVICE"))
        );
        assert!(
            matches!(run(&client,&chat,Operation::Open).await,Outcome::Receipt(ref r,_) if r.contains("connected") && !r.contains("TEST-DEVICE"))
        );
        let requests = calls.lock().unwrap();
        assert_eq!(requests.len(), 5);
        assert_eq!(requests[0].0, "PUT");
        assert_eq!(requests[1].1, "/v1/credentials/ssh/synthetic-server");
        let ssh_body: Value = serde_json::from_slice(&requests[1].3).unwrap();
        assert_eq!(ssh_body["generate"], true);
        assert_eq!(ssh_body["port"], 22);
        assert_eq!(
            requests[2].2,
            "operation_id=11111111-1111-4111-8111-111111111111"
        );
        assert_eq!(requests[3].0, "POST");
        assert_eq!(requests[4].0, "GET");
        server.abort();
    }
}
