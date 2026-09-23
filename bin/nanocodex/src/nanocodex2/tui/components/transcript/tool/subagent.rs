// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use super::Presentation;
use crate::tui::{theme::Theme, transcript::ToolEntry};
use ratatui::style::{Modifier, Style};
use serde_json::Value;

pub(super) fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let (title, subject) = summary(tool);
    let mut presentation = Presentation::new(title, subject);
    if let Some(outcome) = outcome(tool) {
        presentation = presentation.outcome(outcome);
    }
    if !expanded {
        return presentation;
    }
    let mut sections = Vec::new();
    match tool.family() {
        "spawn_agent" => {
            for (key, label) in [
                ("task", "Task"),
                ("model", "Model"),
                ("thinking", "Thinking"),
            ] {
                if let Some(value) = tool.arguments.get(key) {
                    sections.push((label.to_owned(), value.clone()));
                }
            }
        }
        "send_agent_message" => {
            for (key, label) in [
                ("message", "Message"),
                ("purpose", "Purpose"),
                ("priority", "Priority"),
            ] {
                if let Some(value) = tool.arguments.get(key) {
                    sections.push((label.to_owned(), value.clone()));
                }
            }
        }
        "submit_result" => {
            if let Some(output) = tool.arguments.get("output") {
                sections.push(("Result".to_owned(), output.clone()));
            }
        }
        "wait_agent" if tool.result.is_none() => {
            sections.push((
                "Waiting for".into(),
                Value::String(agent_targets(&tool.arguments)),
            ));
        }
        _ => {}
    }
    if let Some(result) = &tool.result {
        if let Some(agents) = result.get("agents").and_then(Value::as_array) {
            if agents.is_empty() {
                sections.push(("Agents".into(), Value::String("No agents returned".into())));
            }
            for agent in agents.iter().take(64) {
                let id = agent.get("agent_id").and_then(Value::as_u64);
                let role = string(agent, "role").unwrap_or("agent");
                let heading =
                    id.map_or_else(|| role.to_owned(), |id| format!("Agent {id} · {role}"));
                sections.push((
                    heading,
                    Value::String(state(agent).unwrap_or("unknown").replace('_', " ")),
                ));
                if let Some(task) = agent.get("task") {
                    sections.push(("Task".into(), task.clone()));
                }
                if let Some(output) = agent
                    .pointer("/status/output")
                    .or_else(|| agent.get("last_output"))
                {
                    sections.push(("Result".into(), output.clone()));
                }
                if let Some(error) = agent.pointer("/status/error") {
                    sections.push(("Error".into(), error.clone()));
                }
            }
            if agents.len() > 64 {
                sections.push((
                    "More".into(),
                    Value::String(format!("{} more agents", agents.len() - 64)),
                ));
            }
        }
        if result.get("agents").is_none()
            && tool.family() != "submit_result"
            && let Some(status) = state(result)
        {
            sections.push(("Status".into(), Value::String(status.replace('_', " "))));
        }
        for key in ["error", "message", "reason"] {
            if let Some(value) = result.get(key) {
                sections.push((label(key), value.clone()));
            }
        }
        // Preserve unexpected error payloads without falling back to protocol JSON.
        if tool.state == crate::tui::transcript::ToolState::Failed && sections.is_empty() {
            sections.push(("Error".into(), result.clone()));
        }
    }
    for (heading, value) in sections {
        presentation = presentation.unselectable_details(super::wrap_plain(
            &heading,
            width,
            Style::default()
                .fg(theme.accent())
                .add_modifier(Modifier::BOLD),
        ));
        let mut text = String::new();
        readable_value(&value, 0, &mut text);
        presentation = presentation.selectable_plain(
            text.trim_end(),
            width,
            Style::default().fg(theme.text()),
        );
    }
    presentation
}

fn summary(tool: &ToolEntry) -> (&'static str, String) {
    match tool.family() {
        "spawn_agent" => {
            let role = string(&tool.arguments, "role").unwrap_or("agent");
            let task = string(&tool.arguments, "task")
                .and_then(|task| task.lines().find(|line| !line.trim().is_empty()))
                .unwrap_or_default();
            let task = compact(task, 56);
            let subject = if task.is_empty() {
                role.to_owned()
            } else {
                format!("{role} · {task}")
            };
            ("Spawned", subject)
        }
        "wait_agent" => (
            if tool.result.is_some() {
                "Waited on"
            } else {
                "Waiting on"
            },
            agent_targets(&tool.arguments),
        ),
        "send_agent_message" => {
            let target = agent_target(&tool.arguments);
            let preview = string(&tool.arguments, "message").unwrap_or_default();
            let preview = compact(
                &preview.split_whitespace().collect::<Vec<_>>().join(" "),
                64,
            );
            (
                "Messaged",
                if preview.is_empty() {
                    target
                } else {
                    format!("{target} · {preview}")
                },
            )
        }
        "interrupt_agent" => ("Interrupted", agent_target(&tool.arguments)),
        "close_agent" => ("Closed", agent_target(&tool.arguments)),
        "list_agents" => ("Listed", "subagents".to_owned()),
        "submit_result" => {
            let title = match tool
                .result
                .as_ref()
                .and_then(|result| string(result, "status"))
            {
                Some("accepted") => "Accepted",
                Some("superseded") => "Superseded",
                _ if tool
                    .result
                    .as_ref()
                    .and_then(|result| result.get("accepted"))
                    .and_then(Value::as_bool)
                    == Some(true) =>
                {
                    "Accepted"
                }
                _ => "Submit",
            };
            (title, "subagent result".to_owned())
        }
        _ => ("Subagent", String::new()),
    }
}

fn outcome(tool: &ToolEntry) -> Option<String> {
    let result = tool.result.as_ref()?;
    if tool.family() == "send_agent_message" {
        return ["disposition", "status", "state"]
            .into_iter()
            .find_map(|key| result.get(key).and_then(Value::as_str))
            .map(str::to_owned);
    }
    if tool.family() == "spawn_agent" {
        let id = result.get("agent_id").and_then(Value::as_u64);
        let state = result
            .pointer("/status/state")
            .and_then(Value::as_str)
            .or_else(|| result.get("status").and_then(Value::as_str));
        return match (id, state) {
            (Some(id), Some(state)) => Some(format!("agent {id} · {state}")),
            (Some(id), None) => Some(format!("agent {id}")),
            (None, Some(state)) => Some(state.to_owned()),
            (None, None) => None,
        };
    }
    if let Some(agents) = result.get("agents").and_then(Value::as_array) {
        let mut counts = std::collections::BTreeMap::<&str, usize>::new();
        for agent in agents {
            *counts.entry(state(agent).unwrap_or("unknown")).or_default() += 1;
        }
        return Some(if agents.is_empty() {
            "no agents".to_owned()
        } else {
            counts
                .into_iter()
                .map(|(state, count)| format!("{count} {state}"))
                .collect::<Vec<_>>()
                .join(" · ")
        });
    }
    if tool.family() == "submit_result" {
        return None;
    }
    state(result).map(str::to_owned)
}

fn state(value: &Value) -> Option<&str> {
    value
        .pointer("/status/state")
        .and_then(Value::as_str)
        .or_else(|| string(value, "status"))
}

fn label(key: &str) -> String {
    let text = key.replace('_', " ");
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

/// Bound traversal before wrapping, including deeply nested or very large results.
fn readable_value(value: &Value, depth: usize, text: &mut String) {
    if text.len() >= 24 * 1024 {
        return;
    }
    if depth > 8 {
        text.push_str("… nested content omitted …\n");
        return;
    }
    let indent = "  ".repeat(depth);
    match value {
        Value::Object(fields) if !fields.is_empty() => {
            for (key, value) in fields.iter().take(64) {
                if text.len() >= 24 * 1024 {
                    break;
                }
                text.push_str(&format!("{indent}{}: ", label(key)));
                if value.is_object() || value.is_array() {
                    text.push('\n');
                    readable_value(value, depth + 1, text);
                } else {
                    readable_value(value, depth, text);
                }
            }
            if fields.len() > 64 {
                text.push_str("… more fields omitted …\n");
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for value in items.iter().take(64) {
                if text.len() >= 24 * 1024 {
                    break;
                }
                text.push_str(&format!("{indent}• "));
                readable_value(value, depth + 1, text);
            }
            if items.len() > 64 {
                text.push_str("… more items omitted …\n");
            }
        }
        Value::String(value) => {
            text.push_str(&super::bounded_text(value));
            text.push('\n');
        }
        Value::Null => text.push_str("None\n"),
        Value::Object(_) | Value::Array(_) => text.push_str("Empty\n"),
        _ => {
            text.push_str(&value.to_string());
            text.push('\n');
        }
    }
}

fn agent_target(arguments: &Value) -> String {
    arguments
        .get("agent_id")
        .and_then(Value::as_u64)
        .map_or_else(|| "agent".to_owned(), |id| format!("agent {id}"))
}

fn agent_targets(arguments: &Value) -> String {
    let Some(ids) = arguments.get("agent_ids").and_then(Value::as_array) else {
        return "agents".to_owned();
    };
    let ids = ids
        .iter()
        .filter_map(Value::as_u64)
        .map(|id| id.to_string())
        .collect::<Vec<_>>();
    if ids.len() == 1 {
        format!("agent {}", ids[0])
    } else {
        format!("agents {}", ids.join(", "))
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn compact(text: &str, max_chars: usize) -> String {
    let mut chars = text.trim().chars();
    let compact = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{compact}…")
    } else {
        compact
    }
}
