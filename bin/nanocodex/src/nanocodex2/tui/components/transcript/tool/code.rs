// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use super::{Presentation, format_bytes};
use crate::tui::{
    theme::Theme,
    transcript::{ToolEntry, code_mode_output_text},
};
use ratatui::style::Style;
use serde_json::Value;

pub(super) fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    if tool.family() == "wait" {
        return wait(tool, width, theme, expanded);
    }
    let source = tool.arguments.as_str().unwrap_or_else(|| {
        tool.arguments
            .get("input")
            .and_then(Value::as_str)
            .unwrap_or("<source unavailable>")
    });
    let emitted = tool.result.as_ref().map_or(0, emitted_count);
    let child_count = tool.child_count;
    let subject = if child_count > 1 {
        format!("{child_count} tools")
    } else if emitted == 1 {
        "1 emitted item".to_owned()
    } else {
        format!("{emitted} emitted items")
    };
    let title = if child_count > 1 { "Batch" } else { "Code" };
    let presentation = Presentation::new(title, subject);
    if !expanded {
        return presentation;
    }
    let details =
        super::super::markdown::render(&format!("```javascript\n{source}\n```"), width, theme)
            .lines;
    let mut presentation = presentation.unselectable_details(details);
    if let Some(result) = &tool.result {
        if let Some(items) = result.as_array() {
            for item in items {
                presentation = with_emitted_item(presentation, item, width, theme);
            }
        } else {
            presentation = with_emitted_item(presentation, result, width, theme);
        }
    }
    let size = tool
        .result
        .as_ref()
        .map_or(0, |result| result.to_string().len());
    presentation.footer(format!("{emitted} outputs · {}", format_bytes(size)))
}

fn emitted_count(result: &Value) -> usize {
    match result {
        Value::Array(items) => items.iter().filter(|item| emitted_item(item)).count(),
        Value::String(text) => usize::from(!code_mode_output_text(text).trim().is_empty()),
        Value::Null => 0,
        _ => 1,
    }
}

fn emitted_item(item: &Value) -> bool {
    match item {
        Value::String(text) => !code_mode_output_text(text).trim().is_empty(),
        Value::Object(fields) if fields.get("text").and_then(Value::as_str).is_some() => fields
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| !code_mode_output_text(text).trim().is_empty()),
        Value::Null => false,
        _ => true,
    }
}

fn with_emitted_item(
    presentation: Presentation,
    item: &Value,
    width: u16,
    theme: &Theme,
) -> Presentation {
    if !emitted_item(item) {
        return presentation;
    }
    if let Some(text) = item
        .as_str()
        .or_else(|| item.get("text").and_then(Value::as_str))
    {
        return presentation.selectable_plain(
            code_mode_output_text(text),
            width,
            Style::default().fg(theme.text()),
        );
    }
    if let Some(summary) = media_summary(item) {
        return presentation.selectable_plain(&summary, width, Style::default().fg(theme.accent()));
    }
    let (source, details) = super::selectable_result(item, width, theme);
    presentation.selectable_details(source, details)
}

fn media_summary(item: &Value) -> Option<String> {
    let fields = item.as_object()?;
    match fields.get("type").and_then(Value::as_str)? {
        "input_image" if fields.get("image_url").and_then(Value::as_str).is_some() => {
            let detail = fields
                .get("detail")
                .and_then(Value::as_str)
                .map(|detail| format!(" · {detail}"))
                .unwrap_or_default();
            Some(format!("image output{detail} · binary data hidden"))
        }
        "input_audio" if fields.get("audio_url").and_then(Value::as_str).is_some() => {
            Some("audio output · binary data hidden".to_owned())
        }
        _ => None,
    }
}

fn wait(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let presentation = Presentation::new("Wait", "background work");
    if !expanded {
        return presentation;
    }
    let details = super::pretty_value(&tool.arguments, width, theme);
    let mut presentation = presentation.unselectable_details(details);
    if let Some(result) = &tool.result {
        let (source, details) = super::selectable_result(result, width, theme);
        presentation = presentation.selectable_details(source, details);
    }
    presentation.footer("wait diagnostics")
}
