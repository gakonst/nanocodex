// SPDX-License-Identifier: Apache-2.0

//! Cards for Claude-native tools: files, search, plans, tasks, web and skills.
//!
//! Results arrive in several envelopes: a plain string, `{text}`, a Code Mode
//! `{content, isError, structuredContent}` receipt or provider
//! `{content_blocks}`. Collapsed rows stay one terse line derived from typed
//! arguments; expanded cards keep a bounded full review. A Write shows the new
//! contents only: without the previous contents it is never drawn as a diff.

use super::{MAX_EXPANDED_TEXT_BYTES, Presentation, count_label, format_bytes, plan};
use crate::nanocodex2::tui::{
    format::shorten_home,
    theme::Theme,
    transcript::{ToolEntry, ToolState},
};
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use serde_json::Value;
use std::{borrow::Cow, path::Path};

/// Separator before project guidance appended to file results.
const WORKSPACE_CONTEXT: &str =
    "\nWorkspace context (guidance only; does not expand tool authority):\n";
/// Source lines rendered per expanded file body or edit hunk.
const MAX_SOURCE_LINES: usize = 80;
/// Bytes scanned for line, match and file counts; larger payloads get lower bounds.
const COUNT_SCAN_BYTES: usize = 64 * 1024;
/// Largest result text parsed as task-board or job JSON.
const MAX_JSON_BYTES: usize = 64 * 1024;

pub(super) fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    match tool.family() {
        "Read" => read(tool, width, theme, expanded),
        "Write" => write(tool, width, theme, expanded),
        "Edit" => edit(tool, width, theme, expanded),
        "NotebookEdit" => notebook(tool, width, theme, expanded),
        "Glob" | "Grep" => search(tool, width, theme, expanded),
        "TodoWrite" => todos(tool, width, theme, expanded),
        "WebSearch" | "WebFetch" => web(tool, width, theme, expanded),
        "Skill" => skill(tool, width, theme, expanded),
        _ => task(tool, width, theme, expanded),
    }
}

fn read(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let path = display_path(argument(tool, "file_path"));
    let requested = requested_range(tool);
    let succeeded = completed(tool);
    let text = succeeded
        .then(|| result_text(tool.result.as_ref()))
        .flatten();
    let (media, media_type, media_bytes) = media_blocks(tool.result.as_ref());
    let mut presentation = Presentation::new("Read", path).truncate_summary();
    let outcome = if succeeded && media > 0 {
        Some(media_type.map_or_else(
            || count_label(media, "attachment", "attachments"),
            |kind| {
                format!(
                    "{} · {kind}",
                    count_label(media, "attachment", "attachments")
                )
            },
        ))
    } else if let Some(text) = text {
        Some(returned_range(text))
    } else {
        requested.clone()
    };
    if let Some(outcome) = outcome {
        presentation = presentation.outcome(outcome);
    }
    if !expanded {
        return presentation;
    }
    if let Some(requested) = requested {
        presentation = presentation.unselectable_details(muted(
            &format!("requested {requested}"),
            width,
            theme,
        ));
    }
    if succeeded && media > 0 {
        let returned = count_label(media, "attachment", "attachments");
        let summary = if media_bytes > 0 {
            format!("{returned} returned · ≈ {}", format_bytes(media_bytes))
        } else {
            format!("{returned} returned")
        };
        presentation = presentation.unselectable_details(super::super::markdown::wrap_plain(
            &summary,
            width,
            Style::default().fg(theme.accent()),
        ));
        if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
            presentation =
                presentation.selectable_plain(text, width, Style::default().fg(theme.text()));
        }
        return presentation.footer("binary data hidden");
    }
    let Some(text) = text else {
        let footer = match tool.state {
            ToolState::Failed => "read failed",
            ToolState::Unknown => "read outcome unknown",
            // Resumed nested calls keep their outcome but not their output.
            ToolState::Succeeded => "read completed · output not retained",
            _ => "read pending",
        };
        return with_error(presentation, tool, width, theme).footer(footer);
    };
    if !text.is_empty() {
        presentation =
            presentation.selectable_plain(text, width, Style::default().fg(theme.text()));
    }
    presentation.footer(format!(
        "{} · {}",
        line_count(text).label("line", "lines"),
        format_bytes(text.len())
    ))
}

fn write(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let path = argument(tool, "file_path");
    let content = argument(tool, "content");
    let mut subject = display_path(path);
    if let Some(content) = content {
        subject.push_str(&format!(
            " · {} · {}",
            line_count(content).label("line", "lines"),
            format_bytes(content.len())
        ));
    }
    let mut presentation = Presentation::new("Write", subject).truncate_summary();
    if !expanded {
        return presentation;
    }
    if let Some(content) = content.filter(|content| !content.is_empty()) {
        presentation =
            presentation.unselectable_details(source_lines(content, language(path), width, theme));
    }
    let footer = match tool.state {
        ToolState::Succeeded => "full contents written · previous contents not shown",
        ToolState::Failed => "write failed · previous contents not shown",
        ToolState::Unknown => "write outcome unknown · previous contents not shown",
        ToolState::Running | ToolState::Yielded => {
            "requested contents · previous contents not shown"
        }
    };
    with_error(presentation, tool, width, theme).footer(footer)
}

fn edit(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let path = argument(tool, "file_path");
    let old = argument(tool, "old_string").unwrap_or_default();
    let new = argument(tool, "new_string").unwrap_or_default();
    let replace_all = tool.arguments.get("replace_all").and_then(Value::as_bool) == Some(true);
    let subject = vec![
        Span::styled(
            format!("{} · ", display_path(path)),
            Style::default().fg(theme.text()),
        ),
        Span::styled(
            format!("+{}", line_count(new).number()),
            Style::default().fg(Color::Green),
        ),
        Span::raw(" "),
        Span::styled(
            format!("−{}", line_count(old).number()),
            Style::default().fg(Color::Red),
        ),
    ];
    let mut presentation = Presentation::styled_subject("Edit", subject).truncate_summary();
    let replacements = completed(tool)
        .then(|| result_text(tool.result.as_ref()).and_then(replacement_count))
        .flatten();
    match (replacements, replace_all) {
        (Some(count), true) => {
            presentation = presentation.outcome(count_label(count, "replacement", "replacements"));
        }
        (None, true) => presentation = presentation.outcome("every occurrence"),
        _ => {}
    }
    if !expanded {
        return presentation;
    }
    if !old.is_empty() || !new.is_empty() {
        presentation =
            presentation.unselectable_details(edit_diff(path, old, new, replace_all, width, theme));
    }
    let footer = match tool.state {
        ToolState::Succeeded => "replacement applied · surrounding file not shown",
        ToolState::Failed => "replacement not applied · surrounding file not shown",
        ToolState::Unknown => "replacement outcome unknown · surrounding file not shown",
        ToolState::Running | ToolState::Yielded => {
            "requested replacement · surrounding file not shown"
        }
    };
    with_error(presentation, tool, width, theme).footer(footer)
}

fn notebook(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let path = display_path(argument(tool, "notebook_path"));
    let mode = argument(tool, "edit_mode").unwrap_or("replace");
    let target = match (mode, argument(tool, "cell_id")) {
        ("insert", Some(cell)) => format!("insert after cell {cell}"),
        ("insert", None) => "insert at start".to_owned(),
        (mode, Some(cell)) => format!("{mode} cell {cell}"),
        (mode, None) => mode.to_owned(),
    };
    let mut presentation =
        Presentation::new("Notebook", format!("{path} · {target}")).truncate_summary();
    if !expanded {
        return presentation;
    }
    if mode != "delete"
        && let Some(source) = argument(tool, "new_source").filter(|source| !source.is_empty())
    {
        let language = if argument(tool, "cell_type") == Some("markdown") {
            "markdown"
        } else {
            ""
        };
        presentation =
            presentation.unselectable_details(source_lines(source, language, width, theme));
    }
    let footer = match mode {
        "delete" if completed(tool) => "cell deleted",
        "delete" if failed(tool) => "delete failed",
        "delete" if tool.state == ToolState::Unknown => "delete outcome unknown",
        "delete" => "delete requested",
        "insert" => "new cell source",
        _ => "new cell source · previous source not shown",
    };
    with_error(presentation, tool, width, theme).footer(footer)
}

fn search(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let grep = tool.family() == "Grep";
    let pattern = argument(tool, "pattern").unwrap_or("<pattern unavailable>");
    let mut subject = if grep {
        format!("\"{pattern}\"")
    } else {
        pattern.to_owned()
    };
    if let Some(path) = argument(tool, "path") {
        subject.push_str(&format!(" in {}", display_path(Some(path))));
    }
    for key in ["glob", "type"] {
        if let Some(filter) = argument(tool, key) {
            subject.push_str(&format!(" · {filter}"));
        }
    }
    let mode = if grep {
        argument(tool, "output_mode").unwrap_or("files_with_matches")
    } else {
        "files_with_matches"
    };
    let context = ["-A", "-B", "-C", "context"].into_iter().any(|key| {
        tool.arguments
            .get(key)
            .and_then(Value::as_u64)
            .is_some_and(|lines| lines > 0)
    });
    let only_matching = tool.arguments.get("-o").and_then(Value::as_bool) == Some(true);
    let text = completed(tool)
        .then(|| result_text(tool.result.as_ref()))
        .flatten();
    let mut presentation =
        Presentation::new(if grep { "Grep" } else { "Glob" }, subject).truncate_summary();
    if let Some(text) = text {
        presentation = presentation.outcome(search_outcome(mode, text, context, only_matching));
    }
    if !expanded {
        return presentation;
    }
    if grep {
        let options = grep_options(&tool.arguments, mode);
        presentation = presentation.unselectable_details(muted(&options, width, theme));
    }
    let Some(text) = text else {
        return with_error(presentation, tool, width, theme).footer("search");
    };
    if !text.is_empty() {
        presentation =
            presentation.selectable_plain(text, width, Style::default().fg(theme.text()));
    }
    presentation.footer(format!(
        "{} · {}",
        line_count(text).label("line", "lines"),
        format_bytes(text.len())
    ))
}

fn todos(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let items = tool
        .arguments
        .get("todos")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let status = todo_status;
    let total = items.len();
    let completed = items
        .iter()
        .filter(|item| status(item) == "completed")
        .count();
    let current = items.iter().find_map(|item| {
        (status(item) == "in_progress")
            .then(|| {
                item.get("activeForm")
                    .or_else(|| item.get("content"))
                    .and_then(Value::as_str)
            })
            .flatten()
    });
    let subject = match current {
        _ if total == 0 => "list cleared".to_owned(),
        Some(current) => format!("{completed}/{total} complete · {current}"),
        None => format!("{completed}/{total} complete"),
    };
    let mut presentation = Presentation::new("Plan", subject).truncate_summary();
    if !expanded {
        return presentation;
    }
    let mut details = Vec::new();
    for item in items {
        let text = item
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        details.extend(plan::checklist_line(status(item), text, width, theme));
    }
    presentation = presentation.unselectable_details(details);
    with_error(presentation, tool, width, theme).footer(format!("{completed}/{total} complete"))
}

fn web(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let search = tool.family() == "WebSearch";
    let subject = if search {
        format!(
            "search \"{}\"",
            argument(tool, "query").unwrap_or("<query unavailable>")
        )
    } else {
        format!(
            "fetch {}",
            argument(tool, "url").unwrap_or("<url unavailable>")
        )
    };
    let mut presentation = Presentation::new("Web", subject).truncate_summary();
    if !expanded {
        return presentation;
    }
    if search {
        for (key, label) in [
            ("allowed_domains", "only"),
            ("blocked_domains", "excluding"),
        ] {
            let domains = tool
                .arguments
                .get(key)
                .and_then(Value::as_array)
                .map(|domains| {
                    domains
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|domains| !domains.is_empty());
            if let Some(domains) = domains {
                presentation = presentation.unselectable_details(muted(
                    &format!("{label} {domains}"),
                    width,
                    theme,
                ));
            }
        }
    } else if let Some(prompt) = argument(tool, "prompt") {
        presentation = presentation.unselectable_details(muted(prompt, width, theme));
    }
    let Some(text) = completed(tool)
        .then(|| result_text(tool.result.as_ref()))
        .flatten()
    else {
        return with_error(presentation, tool, width, theme).footer("web result");
    };
    presentation = presentation.selectable_plain(text, width, Style::default().fg(theme.text()));
    presentation.footer(format!("web result · {}", format_bytes(text.len())))
}

fn skill(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let mut subject = argument(tool, "skill")
        .unwrap_or("<skill unavailable>")
        .to_owned();
    if let Some(arguments) = argument(tool, "args").filter(|args| !args.trim().is_empty()) {
        subject.push_str(&format!(" · {}", arguments.trim()));
    }
    let text = completed(tool)
        .then(|| result_text(tool.result.as_ref()))
        .flatten();
    let mut presentation = Presentation::new("Skill", subject).truncate_summary();
    if let Some(text) = text {
        presentation = presentation.outcome(format!("{} loaded", format_bytes(text.len())));
    }
    if !expanded {
        return presentation;
    }
    let Some(text) = text else {
        return with_error(presentation, tool, width, theme).footer("skill");
    };
    presentation = presentation.selectable_plain(text, width, Style::default().fg(theme.text()));
    presentation.footer(format!("skill guidance · {}", format_bytes(text.len())))
}

fn task(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let family = tool.family();
    let id = argument(tool, "taskId").or_else(|| argument(tool, "task_id"));
    let task_label = id.map_or_else(|| "<task unavailable>".to_owned(), |id| format!("#{id}"));
    let value = completed(tool)
        .then(|| result_json(tool.result.as_ref()))
        .flatten();
    let field = |pointer: &str| {
        value
            .as_ref()
            .and_then(|value| value.pointer(pointer))
            .and_then(scalar)
    };
    let (title, subject, outcome) = match family {
        "TaskCreate" => (
            "Task",
            format!("create · {}", argument(tool, "subject").unwrap_or_default()),
            field("/task/id").map(|id| format!("#{id}")),
        ),
        "TaskGet" => (
            "Task",
            task_label,
            value.as_ref().map(|value| match value.get("task") {
                Some(Value::Object(task)) => [
                    task.get("subject").and_then(Value::as_str),
                    task.get("status").and_then(Value::as_str),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · "),
                _ => "not found".to_owned(),
            }),
        ),
        "TaskUpdate" => (
            "Task",
            update_subject(tool, &task_label),
            match (field("/statusChange/from"), field("/statusChange/to")) {
                (Some(from), Some(to)) => Some(format!("{from} → {to}")),
                _ => field("/success").map(|_| "updated".to_owned()),
            },
        ),
        "TaskList" => (
            "Tasks",
            "list".to_owned(),
            value
                .as_ref()
                .and_then(|value| value.get("tasks"))
                .and_then(Value::as_array)
                .map(|tasks| {
                    let completed = tasks
                        .iter()
                        .filter(|task| {
                            task.get("status").and_then(Value::as_str) == Some("completed")
                        })
                        .count();
                    format!(
                        "{} · {completed} completed",
                        count_label(tasks.len(), "task", "tasks")
                    )
                }),
        ),
        "TaskOutput" => (
            "Task output",
            id.unwrap_or("<task unavailable>").to_owned(),
            field("/status"),
        ),
        "TaskStop" => (
            "Task stop",
            id.unwrap_or("<task unavailable>").to_owned(),
            field("/status")
                .or_else(|| (tool.state == ToolState::Succeeded).then(|| "stopped".to_owned())),
        ),
        family => (family, task_label, field("/status")),
    };
    let mut presentation = Presentation::new(title, subject).truncate_summary();
    if let Some(outcome) = outcome.filter(|outcome| !outcome.is_empty()) {
        presentation = presentation.outcome(outcome);
    }
    if !expanded {
        return presentation;
    }
    if family == "TaskList"
        && let Some(tasks) = value
            .as_ref()
            .and_then(|value| value.get("tasks"))
            .and_then(Value::as_array)
    {
        let mut details = Vec::new();
        for task in tasks {
            let status = task
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            let id = task.get("id").and_then(scalar).unwrap_or_default();
            let subject = task
                .get("subject")
                .and_then(Value::as_str)
                .unwrap_or_default();
            details.extend(plan::checklist_line(
                status,
                &format!("#{id} {subject}"),
                width,
                theme,
            ));
        }
        return presentation
            .unselectable_details(details)
            .footer(count_label(tasks.len(), "task", "tasks"));
    }
    if family == "TaskOutput"
        && let Some(value) = &value
    {
        for key in ["stdout", "stderr", "output"] {
            if let Some(text) = value
                .get(key)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                presentation = presentation
                    .unselectable_details(muted(key, width, theme))
                    .selectable_plain(text, width, Style::default().fg(theme.text()));
            }
        }
    }
    super::with_generic_details(presentation, tool, width, theme)
}

fn todo_status(item: &Value) -> &str {
    item.get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending")
}

fn update_subject(tool: &ToolEntry, task: &str) -> String {
    if let Some(status) = argument(tool, "status") {
        return format!("{task} → {status}");
    }
    let fields = [
        "subject",
        "description",
        "activeForm",
        "owner",
        "addBlocks",
        "addBlockedBy",
        "metadata",
    ]
    .into_iter()
    .filter(|key| tool.arguments.get(key).is_some())
    .collect::<Vec<_>>();
    if fields.is_empty() {
        task.to_owned()
    } else {
        format!("{task} · {}", fields.join(", "))
    }
}

fn argument<'a>(tool: &'a ToolEntry, key: &str) -> Option<&'a str> {
    tool.arguments.get(key).and_then(Value::as_str)
}

fn failed(tool: &ToolEntry) -> bool {
    tool.state == ToolState::Failed
}

/// Results are read as outcomes only once the call actually succeeded.
fn completed(tool: &ToolEntry) -> bool {
    tool.state == ToolState::Succeeded
}

fn display_path(path: Option<&str>) -> String {
    path.map_or_else(
        || "<path unavailable>".to_owned(),
        |path| shorten_home(Path::new(path)),
    )
}

fn language(path: Option<&str>) -> &str {
    path.and_then(|path| Path::new(path).extension())
        .and_then(|extension| extension.to_str())
        .filter(|extension| extension.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("")
}

fn muted(text: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    super::super::markdown::wrap_plain(text, width, Style::default().fg(theme.muted()))
}

/// A line or item count; inexact counts are lower bounds over a bounded scan.
#[derive(Clone, Copy)]
struct Count {
    count: usize,
    exact: bool,
}

impl Count {
    fn number(self) -> String {
        if self.exact {
            self.count.to_string()
        } else {
            format!("{}+", self.count)
        }
    }

    fn label(self, singular: &str, plural: &str) -> String {
        if self.exact {
            count_label(self.count, singular, plural)
        } else {
            format!("{}+ {plural}", self.count)
        }
    }
}

/// The scanned prefix of a payload and whether it is the whole payload.
fn scan_prefix(text: &str) -> (&str, bool) {
    if text.len() <= COUNT_SCAN_BYTES {
        return (text, true);
    }
    let mut end = COUNT_SCAN_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], false)
}

/// Lines in a payload, scanning at most [`COUNT_SCAN_BYTES`].
fn line_count(text: &str) -> Count {
    let (scanned, exact) = scan_prefix(text);
    let newlines = scanned.bytes().filter(|byte| *byte == b'\n').count();
    // A truncated scan always has at least one more line after its prefix.
    let trailing = !exact || (!scanned.is_empty() && !scanned.ends_with('\n'));
    Count {
        count: newlines + usize::from(trailing),
        exact,
    }
}

fn scalar(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_bool().map(|value| value.to_string()))
}

/// Failed or unknown results stay visible in full when a card is expanded.
fn with_error(
    presentation: Presentation,
    tool: &ToolEntry,
    width: u16,
    theme: &Theme,
) -> Presentation {
    if !matches!(tool.state, ToolState::Failed | ToolState::Unknown) {
        return presentation;
    }
    match result_text(tool.result.as_ref()).filter(|text| !text.trim().is_empty()) {
        Some(text) => {
            presentation.selectable_plain(text, width, Style::default().fg(theme.thinking_xhigh()))
        }
        None => presentation,
    }
}

/// Model-visible text of a result, without its appended project guidance.
fn result_text(result: Option<&Value>) -> Option<&str> {
    let text = envelope_text(result?)?;
    Some(
        text.split_once(WORKSPACE_CONTEXT)
            .map_or(text, |(body, _)| body),
    )
}

fn envelope_text(value: &Value) -> Option<&str> {
    if let Some(text) = value.as_str() {
        return Some(text);
    }
    let fields = value.as_object()?;
    if let Some(text) = fields.get("structuredContent").and_then(Value::as_str) {
        return Some(text);
    }
    for key in ["content", "content_blocks"] {
        let Some(content) = fields.get(key) else {
            continue;
        };
        if let Some(text) = content.as_str() {
            return Some(text);
        }
        if let Some(text) = content.as_array().and_then(|blocks| {
            blocks.iter().find_map(|block| {
                (block.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| block.get("text").and_then(Value::as_str))
                    .flatten()
            })
        }) {
            return Some(text);
        }
    }
    ["text", "message", "error"]
        .into_iter()
        .find_map(|key| fields.get(key).and_then(Value::as_str))
}

/// Structured JSON of a task-board or job result in any envelope.
fn result_json(result: Option<&Value>) -> Option<Cow<'_, Value>> {
    let result = result?;
    if let Some(structured) = result
        .get("structuredContent")
        .filter(|value| value.is_object() || value.is_array())
    {
        return Some(Cow::Borrowed(structured));
    }
    let envelope = [
        "content",
        "content_blocks",
        "structuredContent",
        "text",
        "isError",
    ]
    .into_iter()
    .any(|key| result.get(key).is_some());
    if result.is_object() && !envelope {
        return Some(Cow::Borrowed(result));
    }
    let text = result_text(Some(result))?.trim();
    if text.len() > MAX_JSON_BYTES {
        return None;
    }
    serde_json::from_str(text).ok().map(Cow::Owned)
}

/// Count, first media type and approximate decoded size of image/document
/// blocks, from their base64 lengths; their data is never shown or serialized.
fn media_blocks(result: Option<&Value>) -> (usize, Option<&str>, usize) {
    let Some(fields) = result.and_then(Value::as_object) else {
        return (0, None, 0);
    };
    let blocks = ["content", "content_blocks"]
        .into_iter()
        .filter_map(|key| fields.get(key).and_then(Value::as_array))
        .flatten()
        .filter(|block| {
            matches!(
                block.get("type").and_then(Value::as_str),
                Some("image" | "document")
            )
        })
        .collect::<Vec<_>>();
    let kind = blocks.iter().find_map(|block| {
        block
            .get("mimeType")
            .or_else(|| block.pointer("/source/media_type"))
            .and_then(Value::as_str)
            .or_else(|| block.get("type").and_then(Value::as_str))
    });
    let bytes = blocks
        .iter()
        .filter_map(|block| {
            block
                .get("data")
                .or_else(|| block.pointer("/source/data"))
                .and_then(Value::as_str)
        })
        .fold(0_usize, |total, data| {
            total.saturating_add(data.len() / 4 * 3)
        });
    (blocks.len(), kind, bytes)
}

fn requested_range(tool: &ToolEntry) -> Option<String> {
    if let Some(pages) = argument(tool, "pages") {
        return Some(format!("pages {pages}"));
    }
    let offset = tool.arguments.get("offset").and_then(Value::as_u64);
    let limit = tool.arguments.get("limit").and_then(Value::as_u64);
    match (offset, limit) {
        (Some(offset), Some(limit)) => Some(format!(
            "lines {offset}–{}",
            offset.saturating_add(limit.saturating_sub(1))
        )),
        (Some(offset), None) => Some(format!("from line {offset}")),
        (None, Some(limit)) => Some(format!("lines 1–{limit}")),
        (None, None) => None,
    }
}

/// Actual numbered range returned by Read, or its line count when unnumbered.
fn returned_range(text: &str) -> String {
    let body = text.trim_end_matches('\n');
    if body.is_empty() {
        return "no lines".to_owned();
    }
    let number = |line: &str| {
        line.split_once('\t')
            .and_then(|(number, _)| number.trim().parse::<u64>().ok())
    };
    let first = body.lines().next().and_then(number);
    let last = body.rsplit('\n').next().and_then(number);
    match (first, last) {
        (Some(first), Some(last)) if first == last => format!("line {first}"),
        (Some(first), Some(last)) if first < last => format!("lines {first}–{last}"),
        _ => line_count(body).label("line", "lines"),
    }
}

fn replacement_count(text: &str) -> Option<usize> {
    text.trim_end()
        .strip_suffix(" replacement(s))")?
        .rsplit_once('(')?
        .1
        .parse()
        .ok()
}

fn search_outcome(mode: &str, text: &str, context: bool, only_matching: bool) -> String {
    let (scanned, exact) = scan_prefix(text);
    let lines = Count {
        count: scanned
            .lines()
            .filter(|line| !line.is_empty() && *line != "--")
            .count(),
        exact,
    };
    match mode {
        "count" => {
            let (files, total) = scanned
                .lines()
                .filter_map(|line| line.rsplit_once(':')?.1.trim().parse::<usize>().ok())
                .fold((0_usize, 0_usize), |(files, total), count| {
                    (files + 1, total.saturating_add(count))
                });
            let total = Count {
                count: total,
                exact,
            };
            let files = Count {
                count: files,
                exact,
            };
            format!(
                "{} in {}",
                total.label("match", "matches"),
                files.label("file", "files")
            )
        }
        "content" if only_matching => lines.label("match", "matches"),
        "content" if context => lines.label("line", "lines"),
        "content" => lines.label("matching line", "matching lines"),
        _ if exact && lines.count == 0 => "no files".to_owned(),
        _ => lines.label("file", "files"),
    }
}

fn grep_options(arguments: &Value, mode: &str) -> String {
    let mut options = vec![mode.replace('_', " ")];
    for (key, label) in [
        ("-i", "case-insensitive"),
        ("-o", "only matching"),
        ("multiline", "multiline"),
    ] {
        if arguments.get(key).and_then(Value::as_bool) == Some(true) {
            options.push(label.to_owned());
        }
    }
    for key in ["-A", "-B", "-C", "context", "head_limit", "offset"] {
        if let Some(value) = arguments.get(key).and_then(Value::as_u64) {
            options.push(format!("{key} {value}"));
        }
    }
    options.join(" · ")
}

/// Leading lines within the source budget and the count of omitted lines.
fn leading_lines(text: &str, max_lines: usize) -> (&str, Count) {
    let mut end = 0;
    let mut shown = 0;
    for line in text.split_inclusive('\n') {
        if shown == max_lines || end + line.len() > MAX_EXPANDED_TEXT_BYTES {
            break;
        }
        end += line.len();
        shown += 1;
    }
    if shown == 0 && !text.is_empty() {
        end = text.len().min(MAX_EXPANDED_TEXT_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        shown = 1;
    }
    let total = line_count(text);
    let hidden = Count {
        count: total.count.saturating_sub(shown),
        exact: total.exact,
    };
    (text[..end].trim_end_matches('\n'), hidden)
}

fn omitted(hidden: Count, noun: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!("… {} more {noun} not shown", hidden.number()),
        Style::default().fg(theme.muted()),
    ))
}

/// A highlighted, bounded file body inside a fence its contents cannot close.
fn source_lines(source: &str, language: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let (shown, hidden) = leading_lines(source, MAX_SOURCE_LINES);
    let mut longest = 0;
    let mut run = 0;
    for character in shown.chars() {
        run = if character == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    let mut lines = super::super::markdown::render(
        &format!("{fence}{language}\n{shown}\n{fence}"),
        width,
        theme,
    )
    .lines;
    if hidden.count > 0 {
        let noun = if hidden.exact && hidden.count == 1 {
            "line"
        } else {
            "lines"
        };
        lines.push(omitted(hidden, noun, theme));
    }
    lines
}

/// The exact replacement as one hunk; surrounding file lines are unknown here.
fn edit_diff(
    path: Option<&str>,
    old: &str,
    new: &str,
    replace_all: bool,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let budget = MAX_SOURCE_LINES / 2;
    let (old_shown, old_hidden) = leading_lines(old, budget);
    let (new_shown, new_hidden) = leading_lines(new, budget);
    let path = path.unwrap_or("file").replace(['\n', '\r'], " ");
    let heading = if replace_all { " every occurrence" } else { "" };
    let mut patch = format!("*** Begin Patch\n*** Update File: {path}\n@@{heading}\n");
    for (prefix, shown) in [('-', old_shown), ('+', new_shown)] {
        if shown.is_empty() {
            continue;
        }
        for line in shown.split('\n') {
            patch.push(prefix);
            patch.push_str(line);
            patch.push('\n');
        }
    }
    patch.push_str("*** End Patch");
    let mut lines =
        super::super::markdown::render(&format!("```diff\n{patch}\n```"), width, theme).lines;
    if old_hidden.count > 0 {
        lines.push(omitted(old_hidden, "removed lines", theme));
    }
    if new_hidden.count > 0 {
        lines.push(omitted(new_hidden, "added lines", theme));
    }
    lines
}
