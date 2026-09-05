// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use super::Presentation;
use crate::tui::{theme::Theme, transcript::ToolEntry};
use ratatui::{
    style::{Color, Style},
    text::Span,
};
use serde_json::Value;

pub(super) fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let patch = tool
        .arguments
        .as_str()
        .or_else(|| tool.arguments.get("input").and_then(Value::as_str))
        .unwrap_or_default();
    let files = patch.lines().filter_map(file_operation).collect::<Vec<_>>();
    let additions = patch
        .lines()
        .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
        .count();
    let deletions = patch
        .lines()
        .filter(|line| line.starts_with('-') && !line.starts_with("---"))
        .count();
    let file_label = if files.len() == 1 { "file" } else { "files" };
    let subject = vec![
        Span::styled(
            format!("{} {file_label} · ", files.len()),
            Style::default().fg(theme.text()),
        ),
        Span::styled(format!("+{additions}"), Style::default().fg(Color::Green)),
        Span::raw(" "),
        Span::styled(format!("−{deletions}"), Style::default().fg(Color::Red)),
    ];
    let presentation = Presentation::styled_subject("Patch", subject);
    if !expanded {
        return presentation;
    }

    let details = if patch.is_empty() {
        Vec::new()
    } else {
        super::super::markdown::render(&format!("```diff\n{patch}\n```"), width, theme).lines
    };
    presentation
        .unselectable_details(details)
        .footer("patch details")
}

fn file_operation(line: &str) -> Option<(&'static str, &str)> {
    if let Some(path) = line.strip_prefix("*** Add File: ") {
        return Some(("add", path));
    }
    if let Some(path) = line.strip_prefix("*** Update File: ") {
        return Some(("update", path));
    }
    line.strip_prefix("*** Delete File: ")
        .map(|path| ("delete", path))
}
