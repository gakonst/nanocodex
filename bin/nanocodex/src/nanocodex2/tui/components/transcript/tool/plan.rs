// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use super::Presentation;
use crate::tui::{theme::Theme, transcript::ToolEntry};
use ratatui::style::Style;
use serde_json::Value;

pub(super) fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let steps = tool.arguments.get("plan").and_then(Value::as_array);
    let total = steps.map_or(0, Vec::len);
    let completed = steps.map_or(0, |steps| {
        steps
            .iter()
            .filter(|step| step.get("status").and_then(Value::as_str) == Some("completed"))
            .count()
    });
    let current = steps.and_then(|steps| {
        steps.iter().find_map(|step| {
            (step.get("status").and_then(Value::as_str) == Some("in_progress"))
                .then(|| step.get("step").and_then(Value::as_str))
                .flatten()
        })
    });
    let subject = current.map_or_else(
        || format!("{completed}/{total} complete"),
        |step| format!("{completed}/{total} complete · {step}"),
    );
    let presentation = Presentation::new("Plan", subject);
    if !expanded {
        return presentation;
    }

    let mut details = Vec::new();
    if let Some(explanation) = tool.arguments.get("explanation").and_then(Value::as_str) {
        details.extend(super::super::markdown::wrap_plain(
            explanation,
            width,
            Style::default().fg(theme.muted()),
        ));
    }
    if let Some(steps) = steps {
        for step in steps {
            let status = step
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            let (marker, color) = match status {
                "completed" => ("●", theme.thinking_medium()),
                "in_progress" => ("◐", theme.accent()),
                _ => ("○", theme.muted()),
            };
            let text = step.get("step").and_then(Value::as_str).unwrap_or_default();
            details.extend(super::super::markdown::wrap_plain(
                &format!("{marker} {text}"),
                width,
                Style::default().fg(color),
            ));
        }
    }
    presentation
        .unselectable_details(details)
        .footer(format!("{completed}/{total} complete"))
}
