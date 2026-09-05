// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use super::Presentation;
use crate::tui::{theme::Theme, transcript::ToolEntry};
use serde_json::Value;

pub(super) fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let (title, subject) = summary(tool);
    let mut presentation = Presentation::new(title, subject);
    if let Some(outcome) = outcome(tool) {
        presentation = presentation.outcome(outcome);
    }
    if expanded {
        super::with_generic_details(presentation, tool, width, theme)
    } else {
        presentation
    }
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
        "wait_agent" => ("Waiting on", agent_targets(&tool.arguments)),
        "send_agent_message" => ("Messaged", agent_target(&tool.arguments)),
        "interrupt_agent" => ("Interrupted", agent_target(&tool.arguments)),
        "close_agent" => ("Closed", agent_target(&tool.arguments)),
        "list_agents" => ("Listed", "subagents".to_owned()),
        "submit_result" => ("Submitted", "subagent result".to_owned()),
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
    let agents = result.get("agents").and_then(Value::as_array)?;
    let states = agents
        .iter()
        .filter_map(|agent| {
            agent
                .pointer("/status/state")
                .and_then(Value::as_str)
                .or_else(|| agent.get("status").and_then(Value::as_str))
        })
        .collect::<Vec<_>>();
    (!states.is_empty()).then(|| states.join(", "))
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
