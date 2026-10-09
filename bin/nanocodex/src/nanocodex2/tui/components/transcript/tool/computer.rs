//! Compact computer activity; provider observations remain in disclosed details.
use super::super::markdown::wrap_plain_preserving_whitespace;
use super::{Presentation, sanitize, status_style, truncate, wrap_plain};
use crate::nanocodex2::tui::{
    theme::Theme,
    transcript::{ToolEntry, ToolState},
};
use ratatui::{style::Style, text::Line};
use serde_json::{Value, json};

fn title(tool: &ToolEntry) -> &str {
    tool.arguments
        .get("title")
        .and_then(Value::as_str)
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("Computer action")
}

fn has_image(tool: &ToolEntry) -> bool {
    tool.result
        .as_ref()
        .and_then(|result| result.get("content"))
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block["type"] == "image"
                    && block["data"].as_str().is_some_and(|data| !data.is_empty())
            })
        })
}

fn error(tool: &ToolEntry) -> Option<&str> {
    let result = tool.result.as_ref()?;
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .find(|text| text.starts_with("Script error:"))
        })
        .or_else(|| super::error_text(result))?;
    text.strip_prefix("Script error:")
        .unwrap_or(text)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
}

fn preview(tool: &ToolEntry, width: u16) -> String {
    let title = title(tool).split_whitespace().collect::<Vec<_>>().join(" ");
    if tool.state == ToolState::Failed {
        return error(tool).map_or_else(
            || format!("Failed: {title}"),
            |error| {
                if width < 60 {
                    format!("Failed: {error}")
                } else {
                    format!("Failed: {} — {error}", truncate(&title, width / 3))
                }
            },
        );
    }
    if has_image(tool) {
        format!("Captured screenshot · {title}")
    } else {
        title
    }
}

pub(super) fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let presentation = Presentation::new("Computer", preview(tool, width)).truncate_summary();
    if expanded {
        super::with_generic_details(presentation, tool, width, theme)
    } else {
        presentation
    }
}

pub(in super::super) fn previews(calls: &[&ToolEntry], width: u16) -> Value {
    let mut selected = if let Some(index) = calls
        .iter()
        .rposition(|tool| tool.state == ToolState::Running)
    {
        vec![index]
    } else {
        let mut indices: Vec<_> = (0..calls.len()).collect();
        indices.sort_by_key(|&index| {
            std::cmp::Reverse((
                calls[index].state == ToolState::Failed,
                has_image(calls[index]),
                index,
            ))
        });
        indices.truncate(if calls.len() > 3 { 2 } else { 3 });
        indices
    };
    selected.sort_unstable();
    Value::Array(selected.into_iter().map(|index| json!({"text": preview(calls[index], width), "failed": calls[index].state == ToolState::Failed})).collect())
}

pub(super) fn group_lines(
    tool: &ToolEntry,
    total: usize,
    previews: &[Value],
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let active = tool.state == ToolState::Running;
    let label = if active {
        "Using computer"
    } else {
        "Used computer"
    };
    let noun = if total == 1 { "action" } else { "actions" };
    let failed = tool.arguments["counts"][2].as_u64().unwrap_or(0);
    let mut header = format!(
        "  ▶ {} {label} · {total} {noun}",
        super::status_symbol(tool.state)
    );
    if failed > 0 {
        header.push_str(&format!(" · {failed} failed"));
    }
    if tool.arguments["failed"] == true {
        header.push_str(" · execution failed");
    }
    let mut lines =
        wrap_plain_preserving_whitespace(&header, width, status_style(tool.state, theme));
    for item in previews {
        let text = item["text"].as_str().unwrap_or_default();
        let style = if item["failed"] == true {
            status_style(ToolState::Failed, theme)
        } else {
            Style::default().fg(theme.muted())
        };
        lines.push(Line::styled(
            truncate(&sanitize(&format!("    {text}")), width),
            style,
        ));
    }
    if total > previews.len() && !active {
        lines.extend(wrap_plain(
            &format!("    {} more · expand for details", total - previews.len()),
            width,
            Style::default().fg(theme.muted()),
        ));
    }
    lines
}
