// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use super::Presentation;
use crate::tui::{theme::Theme, transcript::ToolEntry};
use serde_json::Value;

pub(super) fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let action = tool
        .arguments
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("operation");
    let title = format!("Browser · {}", action.replace('_', " "));
    let subject = [
        "url",
        "url_contains",
        "query",
        "text",
        "request_id",
        "tab_id",
        "artifact_id",
    ]
    .into_iter()
    .find_map(|key| tool.arguments.get(key).and_then(Value::as_str))
    .map(compact)
    .or_else(|| tool.arguments.get("target").and_then(target_summary))
    .unwrap_or_default();
    let presentation = Presentation::new(title, subject);
    if expanded {
        super::with_generic_details(presentation, tool, width, theme)
    } else {
        presentation
    }
}

fn target_summary(target: &Value) -> Option<String> {
    if let Some(target) = target.as_str() {
        return Some(compact(target));
    }
    let fields = target.as_object()?;
    let by = fields.get("by").and_then(Value::as_str);
    let mut summary = match by {
        Some("ref") => scalar(fields.get("reference")).map(|value| format!("ref {value}")),
        Some("css") => scalar(fields.get("selector")),
        Some("role") => scalar(fields.get("role")).map(|role| {
            scalar(fields.get("name")).map_or_else(
                || format!("role {role}"),
                |name| format!("role {role} {name:?}"),
            )
        }),
        Some("text") | Some("alt_text") => scalar(fields.get("text")),
        Some("label") => scalar(fields.get("label")).map(|value| format!("label {value:?}")),
        Some("placeholder") => {
            scalar(fields.get("placeholder")).map(|value| format!("placeholder {value:?}"))
        }
        Some("title") => scalar(fields.get("title")).map(|value| format!("title {value:?}")),
        Some("test_id") => scalar(fields.get("id")).map(|value| format!("test id {value}")),
        _ => [
            "selector",
            "reference",
            "role",
            "name",
            "text",
            "label",
            "id",
        ]
        .into_iter()
        .find_map(|key| scalar(fields.get(key))),
    }?;
    if let Some(index) = fields.get("index").and_then(index_summary) {
        summary.push_str(" · ");
        summary.push_str(&index);
    }
    Some(compact(&summary))
}

fn index_summary(index: &Value) -> Option<String> {
    if let Some(index) = index.as_str() {
        return Some(index.replace('_', " "));
    }
    let fields = index.as_object()?;
    match fields.get("kind").and_then(Value::as_str) {
        Some("nth") => fields
            .get("index")
            .and_then(Value::as_u64)
            .map(|index| format!("nth {index}")),
        Some(kind) => Some(kind.replace('_', " ")),
        None => None,
    }
}

fn scalar(value: Option<&Value>) -> Option<String> {
    let value = value?;
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

fn compact(value: &str) -> String {
    let mut characters = value.chars();
    let compact = characters.by_ref().take(96).collect::<String>();
    if characters.next().is_some() {
        format!("{compact}…")
    } else {
        compact
    }
}
