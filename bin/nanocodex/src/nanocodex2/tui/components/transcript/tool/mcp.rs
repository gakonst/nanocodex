// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use super::Presentation;
use crate::tui::{theme::Theme, transcript::ToolEntry};
use serde_json::Value;

pub(super) fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let server = tool.mcp_server().unwrap_or("MCP");
    let operation = tool.family();
    let title = format!(
        "{} · {}",
        server.replace('_', " "),
        operation.replace('_', " ")
    );
    let subject = wrapper_subject(operation, &tool.arguments)
        .unwrap_or_else(|| super::meaningful_subject(&tool.arguments).unwrap_or_default());
    let presentation = Presentation::new(title, subject);
    if expanded {
        super::with_generic_details(presentation, tool, width, theme)
    } else {
        presentation
    }
}

fn wrapper_subject(operation: &str, arguments: &Value) -> Option<String> {
    if !matches!(operation, "call_read_tool" | "call_write_tool") {
        return None;
    }
    let nested_name = arguments.get("name").and_then(Value::as_str)?;
    let nested_arguments = arguments.get("arguments");
    let nested_subject = nested_arguments
        .and_then(super::meaningful_subject)
        .or_else(|| {
            nested_arguments
                .and_then(Value::as_str)
                .map(first_line)
                .map(str::to_owned)
        });
    Some(nested_subject.map_or_else(
        || first_line(nested_name).to_owned(),
        |subject| format!("{} · {subject}", first_line(nested_name)),
    ))
}

fn first_line(value: &str) -> &str {
    value.lines().next().unwrap_or_default()
}
