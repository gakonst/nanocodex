use super::{Presentation, count_label, generic};
use crate::tui::{
    theme::Theme,
    transcript::{ToolEntry, ToolState},
};
use ratatui::style::{Modifier, Style};
use serde_json::Value;

pub(super) fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let reading = tool.family() == "read_session";
    let title = if reading {
        "Read session"
    } else {
        "Find sessions"
    };
    let subject = tool
        .arguments
        .get(if reading { "session_id" } else { "query" })
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut presentation = Presentation::new(title, subject).truncate_summary();
    let Some(raw) = &tool.result else {
        return presentation;
    };
    // Some transports serialize the complete result as a string.
    let decoded;
    let result = if let Some(text) = raw.as_str() {
        decoded = serde_json::from_str::<Value>(text).unwrap_or_else(|_| raw.clone());
        &decoded
    } else {
        raw
    };
    let Some(items) = result
        .get(if reading { "turns" } else { "sessions" })
        .and_then(Value::as_array)
    else {
        return generic(tool, width, theme, expanded);
    };
    if tool.state == ToolState::Failed || !items.iter().all(|item| item.is_object()) {
        return generic(tool, width, theme, expanded);
    }
    let count = count_label(
        items.len(),
        if reading { "turn" } else { "match" },
        if reading { "turns" } else { "matches" },
    );
    if reading
        && let Some(title) = items
            .iter()
            .find_map(|item| item.get("title").and_then(Value::as_str))
    {
        presentation = Presentation::new("Read session", title).truncate_summary();
    }
    presentation = presentation.outcome(&count);
    if !expanded {
        return presentation;
    }
    let normal = Style::default().fg(theme.text());
    let muted = Style::default().fg(theme.muted());
    let heading = Style::default()
        .fg(theme.accent())
        .add_modifier(Modifier::BOLD);
    if items.is_empty() {
        return presentation
            .selectable_plain(
                if reading {
                    "No turns returned."
                } else {
                    "No matching sessions."
                },
                width,
                muted,
            )
            .footer(count);
    }
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            presentation = presentation.selectable_plain("", width, normal);
        }
        if reading {
            for (key, label) in [("user", "You"), ("assistant", "Assistant")] {
                if let Some(text) = item
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    presentation = presentation
                        .selectable_plain(label, width, heading)
                        .selectable_plain(text, width, normal);
                }
            }
        } else {
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("Untitled session");
            presentation =
                presentation.selectable_plain(format!("{}. {title}", index + 1), width, heading);
            if let Some(preview) = item.get("preview").and_then(Value::as_str) {
                presentation = presentation.selectable_plain(preview, width, normal);
            }
        }
        // Retain copyable references without the JSON transport fields.
        let mut references = Vec::new();
        if !reading && let Some(id) = item.get("session_id").and_then(Value::as_str) {
            references.push(format!("Session {id}"));
        }
        if let Some(id) = item.get("turn_id").and_then(Value::as_str) {
            references.push(format!("Turn {id}"));
        }
        if !references.is_empty() {
            presentation = presentation.selectable_plain(references.join(" · "), width, muted);
        }
    }
    presentation.footer(if reading {
        format!("{count} · {subject}")
    } else {
        count
    })
}
