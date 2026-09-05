// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use super::{Presentation, format_bytes};
use crate::tui::{theme::Theme, transcript::ToolEntry};
use ratatui::style::Style;
use serde_json::Value;

const OPERATIONS: [&str; 9] = [
    "search_query",
    "image_query",
    "open",
    "click",
    "find",
    "finance",
    "weather",
    "sports",
    "time",
];

pub(super) fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let subject = summary(&tool.arguments);
    let presentation = Presentation::new("Web", subject);
    if !expanded {
        return presentation;
    }

    let details = operation_details(&tool.arguments, width, theme);
    let mut presentation = presentation.unselectable_details(details);
    let result_size = tool
        .result
        .as_ref()
        .map_or(0, |result| result.to_string().len());
    if let Some(result) = tool.result.as_ref().and_then(Value::as_str) {
        let result = clean_result(result);
        presentation =
            presentation.selectable_plain(result, width, Style::default().fg(theme.text()));
    } else if let Some(result) = &tool.result {
        let (source, details) = super::selectable_result(result, width, theme);
        presentation = presentation.selectable_details(source, details);
    }
    presentation.footer(format!("web result · {}", format_bytes(result_size)))
}

fn summary(arguments: &Value) -> String {
    let counts = OPERATIONS
        .iter()
        .filter_map(|operation| {
            let count = arguments
                .get(operation)
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            (count > 0).then_some((*operation, count))
        })
        .collect::<Vec<_>>();
    if counts.len() > 1 {
        return counts
            .into_iter()
            .map(|(operation, count)| format!("{} {count}", operation_label(operation)))
            .collect::<Vec<_>>()
            .join(" · ");
    }
    if let Some(queries) = arguments.get("search_query").and_then(Value::as_array) {
        if queries.len() == 1
            && let Some(query) = queries[0].get("q").and_then(Value::as_str)
        {
            return format!("search {query:?}");
        }
        return format!("{} searches", queries.len());
    }
    if let Some(queries) = arguments.get("image_query").and_then(Value::as_array) {
        return format!("{} image queries", queries.len());
    }
    if let Some(open) = first_string(arguments, "open", "ref_id") {
        return format!("open {open}");
    }
    if let Some(pattern) = first_string(arguments, "find", "pattern") {
        return format!("find {pattern:?}");
    }
    if let Some(ticker) = first_string(arguments, "finance", "ticker") {
        return format!("finance {ticker}");
    }
    if let Some(location) = first_string(arguments, "weather", "location") {
        return format!("weather {location}");
    }
    if let Some(league) = first_string(arguments, "sports", "league") {
        return format!("sports {league}");
    }
    if let Some(offset) = first_string(arguments, "time", "utc_offset") {
        return format!("time {offset}");
    }
    let count = OPERATIONS
        .iter()
        .map(|key| {
            arguments
                .get(key)
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        })
        .sum::<usize>();
    format!("{count} operations")
}

fn operation_label(operation: &str) -> &str {
    match operation {
        "search_query" => "search",
        "image_query" => "images",
        operation => operation,
    }
}

fn operation_details(
    arguments: &Value,
    width: u16,
    theme: &Theme,
) -> Vec<ratatui::text::Line<'static>> {
    let mut details = Vec::new();
    for operation in OPERATIONS {
        let Some(values) = arguments.get(operation).and_then(Value::as_array) else {
            continue;
        };
        for value in values {
            let rendered = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
            details.extend(super::super::markdown::wrap_plain(
                &format!("{} {rendered}", operation.replace('_', " ")),
                width,
                Style::default().fg(theme.accent()),
            ));
        }
    }
    if let Some(length) = arguments.get("response_length").and_then(Value::as_str) {
        details.extend(super::super::markdown::wrap_plain(
            &format!("response length {length}"),
            width,
            Style::default().fg(theme.muted()),
        ));
    }
    details
}

fn first_string<'a>(arguments: &'a Value, operation: &str, field: &str) -> Option<&'a str> {
    arguments
        .get(operation)
        .and_then(Value::as_array)?
        .first()?
        .get(field)
        .and_then(Value::as_str)
}

fn clean_result(result: &str) -> String {
    let mut clean = String::with_capacity(result.len());
    let mut rest = result;
    while let Some(start) = rest.find("cite") {
        clean.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('') else {
            rest = &rest[start..];
            break;
        };
        rest = &rest[start + end + ''.len_utf8()..];
    }
    clean.push_str(rest);
    clean
        .lines()
        .map(|line| line.split("[wordlim:").next().unwrap_or(line).trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}
