//! Caller-local Vault approval. Only safe metadata and receipts enter chat.
use nanocodex_managed::VaultLogin;
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    Latest,
    Review { id: String, origin: String },
    Open,
    Help,
}
impl Command {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let words: Vec<_> = text.split_whitespace().collect();
        if words.first().copied() != Some("/vault") {
            return None;
        }
        Some(match words.as_slice() {
            [_] | [_, "review"] => Self::Latest,
            [_, "open"] => Self::Open,
            [_, "review", id, origin] if valid_id(id) && valid_origin(origin) => Self::Review {
                id: (*id).into(),
                origin: (*origin).into(),
            },
            _ => Self::Help,
        })
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Review {
    pub(crate) login: VaultLogin,
    pub(crate) origin: String,
    pub(crate) agent_id: String,
    pub(crate) generation: u64,
    pub(crate) visible: bool,
}
impl Review {
    pub(crate) fn description(&self) -> String {
        format!(
            "Saved login: {}\nVault ID: {}\n\nApprove website:\n{}\n\nCurrent website: {}\nThis replaces the login’s approved website.\nYour password stays in Vault.\n\nPress Ctrl+Enter to approve, or Esc to cancel.",
            self.login.name,
            self.login.id,
            self.origin,
            self.login.browser_origin.as_deref().unwrap_or("None")
        )
    }
}
pub(crate) fn valid_id(id: &str) -> bool {
    (22..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
pub(crate) fn valid_origin(origin: &str) -> bool {
    origin.len() <= 2048
        && reqwest::Url::parse(origin).is_ok_and(|url| {
            url.scheme() == "https" && url.origin().ascii_serialization() == origin
        })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Intake {
    #[serde(rename = "type")]
    type_: String,
    status: String,
    #[serde(default = "create")]
    operation: String,
    vault_id: Option<String>,
    kind: String,
    name: Option<String>,
    origin: Option<String>,
}
fn create() -> String {
    "create".into()
}
fn safe_name(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= 120 && !name.chars().any(char::is_control)
}
pub(crate) fn intake_summary(value: &Value) -> Option<String> {
    let intake: Intake = serde_json::from_value(value.clone()).ok()?;
    if intake.type_ != "vault_intake"
        || intake.status != "input_required"
        || !["login", "api_key", "card", "address", "phone"].contains(&intake.kind.as_str())
        || intake.name.as_deref().is_some_and(|s| !safe_name(s))
        || intake
            .origin
            .as_deref()
            .is_some_and(|s| intake.kind != "login" || !valid_origin(s))
    {
        return None;
    }
    match intake.operation.as_str() {
        "authorize_origin" if intake.kind == "login" => {
            let _id = intake.vault_id.filter(|s| valid_id(s))?;
            let origin = intake.origin?;
            Some(format!(
                "Website approval requested for saved login\n{origin}\nReview opens automatically. Use /vault to reopen."
            ))
        }
        "create" if intake.vault_id.is_none() => Some(format!(
            "Add {} to Vault\nYour secure Vault opens automatically. Reopen with /vault open\nEnter credential values only in the Vault web form. After saving, return here and tell the agent to refresh Vault metadata.",
            intake.name.unwrap_or_else(|| intake.kind.replace('_', " "))
        )),
        _ => None,
    }
}
pub(crate) fn receipt(login: &VaultLogin) -> String {
    format!(
        "Vault website approval saved.\nLogin: {}\nVault ID: {}\nApproved website: {}\nPassword stayed in Vault.",
        login.name,
        login.id,
        login.browser_origin.as_deref().unwrap_or("None")
    )
}
pub(crate) fn receipt_summary(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("type")?.as_str()? != "vault_intake_receipt" {
        return None;
    }
    let invalid = || Some("Vault receipt could not be verified.".to_owned());
    let Some(name) = value
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| safe_name(s))
    else {
        return invalid();
    };
    if value.get("status").and_then(Value::as_str) != Some("saved")
        || !value
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(valid_id)
    {
        return invalid();
    }
    match value.get("operation").and_then(Value::as_str) {
        Some("authorize_origin") => {
            let Some(origin) = value
                .get("browser_origin")
                .and_then(Value::as_str)
                .filter(|s| valid_origin(s))
            else {
                return invalid();
            };
            Some(format!(
                "Website approved for {name}\n{origin}\nVault ID: {}\nSaved to Vault. Password stayed in Vault.",
                value.get("id")?.as_str()?
            ))
        }
        Some("create") => Some(format!("Saved {name} to Vault.")),
        _ => invalid(),
    }
}
pub(crate) enum Outcome {
    Review(Review),
    Saved(String),
}
pub(crate) type Completion = (super::pane::PaneId, String, u64, Result<Outcome, String>);

/// Hard-wrap the complete review; approval is disabled if any content is hidden.
pub(crate) fn review_lines(text: &str, width: u16) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    for logical in text.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        for character in logical.chars() {
            let columns = character.width().unwrap_or(0);
            if used + columns > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push(character);
            used += columns;
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn exact_origins_and_local_commands() {
        assert!(matches!(
            Command::parse("/vault review abcdefghijklmnopqrstuv https://example.com"),
            Some(Command::Review { .. })
        ));
        for origin in [
            "http://example.com",
            "https://example.com/",
            "https://user@example.com",
            "https://example.com?q=x",
            "https://example.com/#x",
        ] {
            assert!(!valid_origin(origin));
        }
        assert_eq!(
            Command::parse("/vault review bad https://example.com"),
            Some(Command::Help)
        );
        assert_eq!(Command::parse("ordinary prompt"), None);
    }
    #[test]
    fn intake_is_strict_and_approval_ignores_unverified_name() {
        let mut value = json!({"type":"vault_intake","status":"input_required","operation":"authorize_origin","vault_id":"abcdefghijklmnopqrstuv","kind":"login","name":"Unverified label","origin":"https://example.com"});
        let text = intake_summary(&value).unwrap();
        assert!(text.contains("/vault"));
        assert!(!text.contains("Unverified label"));
        value["password"] = "secret".into();
        assert!(intake_summary(&value).is_none());
    }
    #[test]
    fn receipts_project_only_safe_fields() {
        let value = json!({"type":"vault_intake_receipt","status":"saved","operation":"authorize_origin","id":"abcdefghijklmnopqrstuv","name":"Example","kind":"login","browser_origin":"https://example.com","password":"secret"});
        let text = receipt_summary(&value.to_string()).unwrap();
        assert!(text.contains("Website approved for Example"));
        assert!(!text.contains("secret"));
    }
    #[test]
    fn vault_scope_rejects_switch_away_and_back() {
        assert!(scope_matches("a", 1, "a", 1));
        assert!(!scope_matches("a", 1, "b", 2));
        assert!(!scope_matches("a", 1, "a", 3));
        assert!(!scope_matches("", 1, "", 1));
    }
    #[test]
    fn vault_nested_envelope_is_plain_text() {
        let request = serde_json::json!({"type":"vault_intake","status":"input_required","operation":"authorize_origin","kind":"login","vault_id":"abcdefghijklmnopqrstuv","origin":"https://example.com"});
        let wrapped = serde_json::json!({"content":[{"type":"text", "text":request.to_string()}]});
        let summary = payload_summary(&wrapped, 0).unwrap();
        assert!(summary.contains("/vault"));
        assert!(!summary.contains("input_required"));
        assert!(!summary.contains('{'));
        assert!(matches!(
            intake_command(&wrapped),
            Some(Command::Review { .. })
        ));
        let mixed =
            serde_json::json!([wrapped, {"type":"text","text":"Unrelated output stays visible"}]);
        assert!(
            payload_summary(&mixed, 0)
                .unwrap()
                .contains("Unrelated output stays visible")
        );
    }
    #[test]
    fn review_wraps_all_content() {
        assert_eq!(review_lines("123456\nabc", 3), ["123", "456", "abc"]);
    }
}

/// Recognize nested Code Mode/MCP result envelopes without exposing their JSON.
pub(crate) fn payload_summary(value: &Value, depth: usize) -> Option<String> {
    if depth > 10 {
        return None;
    }
    if value.get("type").and_then(Value::as_str) == Some("vault_intake") {
        return Some(intake_summary(value).unwrap_or_else(|| {
            "Secure Vault request could not be verified. Use /vault open.".into()
        }));
    }
    if value.get("type").and_then(Value::as_str) == Some("vault_intake_receipt") {
        return receipt_summary(&value.to_string());
    }
    if let Some(text) = value.as_str() {
        return serde_json::from_str::<Value>(super::transcript::code_mode_output_text(text))
            .ok()
            .and_then(|v| payload_summary(&v, depth + 1));
    }
    let items: Vec<_> = if let Some(items) = value.as_array() {
        items.iter().collect()
    } else if let Some(fields) = value.as_object() {
        ["text", "content", "structuredContent", "result", "output"]
            .iter()
            .filter_map(|key| fields.get(*key))
            .collect()
    } else {
        return None;
    };
    let summaries: Vec<_> = items
        .iter()
        .map(|item| payload_summary(item, depth + 1))
        .collect();
    if summaries.iter().all(Option::is_none) {
        return None;
    }
    Some(
        items
            .into_iter()
            .zip(summaries)
            .map(|(item, summary)| {
                summary.unwrap_or_else(|| {
                    item.as_str()
                        .or_else(|| item.get("text").and_then(Value::as_str))
                        .map(str::to_owned)
                        .unwrap_or_else(|| item.to_string())
                })
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

pub(crate) fn intake_command(value: &Value) -> Option<Command> {
    fn decode(value: &Value, depth: usize) -> Option<Command> {
        if depth > 10 {
            return None;
        }
        if intake_summary(value).is_some() {
            return if value.get("operation").and_then(Value::as_str) == Some("authorize_origin") {
                Some(Command::Review {
                    id: value.get("vault_id")?.as_str()?.into(),
                    origin: value.get("origin")?.as_str()?.into(),
                })
            } else {
                Some(Command::Open)
            };
        }
        if let Some(text) = value.as_str() {
            return serde_json::from_str::<Value>(super::transcript::code_mode_output_text(text))
                .ok()
                .and_then(|v| decode(&v, depth + 1));
        }
        if let Some(items) = value.as_array() {
            return items.iter().rev().find_map(|v| decode(v, depth + 1));
        }
        ["text", "content", "structuredContent", "result", "output"]
            .iter()
            .filter_map(|key| value.get(*key))
            .find_map(|v| decode(v, depth + 1))
    }
    decode(value, 0)
}

pub(crate) fn browser_summary(tool: &str, result: Option<&Value>, failed: bool) -> String {
    if failed {
        return "Private browser request could not be completed. Check the approved website and saved login.".into();
    }
    let status = result
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str);
    match (tool, status) {
        ("browser_vault_close", Some("closed")) => {
            "Private credential browser closed; login state discarded.".into()
        }
        (_, Some("submitted")) => {
            "Login form submitted privately. Sign-in has not been confirmed.".into()
        }
        (_, Some("filled")) => "Login fields filled privately. Form has not been submitted.".into(),
        (_, Some("login_form")) => "Supported username and password fields found.".into(),
        (_, Some("username_form")) => "Supported username field found.".into(),
        (_, Some("password_form")) => "Supported password field found.".into(),
        (_, Some("no_supported_login_form")) => {
            "No supported login form found. This does not confirm sign-in.".into()
        }
        (_, Some("destination_changed")) => {
            "Browser left the approved website. Review the destination before continuing.".into()
        }
        _ if result.is_none() => "Working privately with your saved login…".into(),
        _ => "Private browser request finished. Credential values stay in Vault.".into(),
    }
}

pub(crate) fn scope_matches(
    expected_agent: &str,
    expected_generation: u64,
    agent: &str,
    generation: u64,
) -> bool {
    !agent.is_empty() && expected_agent == agent && expected_generation == generation
}

/// Only completed live tool results can initiate a caller-local Vault flow.
pub(crate) fn request(record: &super::transcript::TranscriptRecord) -> Option<(String, Command)> {
    if record.kind() != "tool.result" {
        return None;
    }
    let value: Value = record.decode_payload().ok()?;
    if value.get("status")?.as_str()? != "completed"
        || !matches!(
            value.get("tool")?.as_str()?,
            "request_vault_intake" | "exec" | "wait"
        )
    {
        return None;
    }
    let command = value
        .get("structured_result")
        .and_then(intake_command)
        .or_else(|| value.get("result").and_then(intake_command))?;
    let turn = record
        .managed_turn_id()
        .or_else(|| record.agent_request_id());
    // The semantic command is shared by the direct result and its exec echo.
    let key = format!("{turn:?}:{command:?}");
    Some((key, command))
}
