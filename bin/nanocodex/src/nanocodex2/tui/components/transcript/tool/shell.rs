// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use super::{Presentation, format_bytes};
use crate::tui::{format::shorten_home, theme::Theme, transcript::ToolEntry};
use ratatui::{
    style::{Color, Style},
    text::Span,
};
use serde_json::Value;
use std::path::Path;
use syntect::easy::HighlightLines;

pub(super) fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    if tool.family() == "write_stdin" {
        return stdin(tool, width, theme, expanded);
    }
    let command = tool
        .arguments
        .get("cmd")
        .and_then(Value::as_str)
        .unwrap_or("<command unavailable>");
    let mut presentation =
        Presentation::styled_subject("Shell", command_spans(command)).truncate_summary();
    if let Some(outcome) = shell_outcome(tool.result.as_ref()) {
        presentation = presentation.outcome(outcome);
    }
    if !expanded {
        return presentation;
    }

    if let Some(workdir) = tool.arguments.get("workdir").and_then(Value::as_str) {
        presentation = presentation.unselectable_details(super::super::markdown::wrap_plain(
            &format!("cwd {}", shorten_home(Path::new(workdir))),
            width,
            Style::default().fg(theme.muted()),
        ));
    }
    let rendered_command = command_spans(command)
        .into_iter()
        .map(|mut span| {
            span.style = span.style.bg(theme.code_background());
            span
        })
        .collect::<Vec<_>>();
    let prompt_exclusions = [std::iter::once(0..2).collect::<Vec<_>>()];
    presentation = presentation.selectable_details_excluding(
        command,
        super::super::markdown::wrap_spans(&rendered_command, width, true),
        &prompt_exclusions,
    );
    for substep in &tool.substeps {
        presentation = presentation.unselectable_details(super::super::markdown::wrap_plain(
            &format!("↳ {substep}"),
            width,
            Style::default().fg(theme.muted()),
        ));
    }
    with_shell_output(presentation, tool, width, theme)
}

fn command_spans(command: &str) -> Vec<Span<'static>> {
    let command = super::super::markdown::sanitize(command);
    let assets = super::super::highlight::assets();
    let syntax = super::super::highlight::syntax_for_token(&assets.syntaxes, "sh");
    let syntax_theme = super::super::highlight::theme();
    let mut highlighter = HighlightLines::new(syntax, &syntax_theme);
    let mut spans = vec![Span::styled("$ ", Style::default().fg(Color::Yellow))];

    for (index, line) in command.split('\n').enumerate() {
        if index > 0 {
            spans.push(Span::raw("\n"));
        }
        spans.extend(super::super::highlight::line(
            &mut highlighter,
            line,
            &assets.syntaxes,
        ));
    }
    spans
}

fn stdin(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let session = tool
        .arguments
        .get("session_id")
        .and_then(Value::as_i64)
        .map(|id| id.to_string())
        .unwrap_or_else(|| "<session unavailable>".to_owned());
    let subject = tool
        .arguments
        .get("chars")
        .and_then(Value::as_str)
        .filter(|chars| !chars.is_empty())
        .map_or_else(
            || format!("poll {session}"),
            |chars| format!("send {chars:?} to {session}"),
        );
    let mut presentation = Presentation::new("Process", subject).truncate_summary();
    if let Some(outcome) = shell_outcome(tool.result.as_ref()) {
        presentation = presentation.outcome(outcome);
    }
    if !expanded {
        return presentation;
    }
    with_shell_output(presentation, tool, width, theme)
}

fn with_shell_output(
    mut presentation: Presentation,
    tool: &ToolEntry,
    width: u16,
    theme: &Theme,
) -> Presentation {
    let output = tool
        .result
        .as_ref()
        .and_then(|result| result.get("output"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !output.is_empty() {
        presentation =
            presentation.selectable_plain(output, width, Style::default().fg(theme.text()));
    }
    let line_count = output.lines().count();
    let line_label = if line_count == 1 { "line" } else { "lines" };
    presentation.footer(format!(
        "{line_count} {line_label} · {}",
        format_bytes(output.len())
    ))
}

fn shell_outcome(result: Option<&Value>) -> Option<String> {
    let result = result?;
    if let Some(code) = result.get("exit_code").and_then(Value::as_i64) {
        return Some(format!("exit {code}"));
    }
    if let Some(error) = result.get("error").and_then(Value::as_str) {
        return Some(error.lines().next().unwrap_or(error).to_owned());
    }
    if let Some(id) = result.get("session_id").and_then(Value::as_i64) {
        return Some(format!("session {id} running"));
    }
    Some("terminated".to_owned())
}
