// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use super::{Presentation, format_bytes};
use crate::tui::{theme::Theme, transcript::ToolEntry};
use ratatui::style::Style;
use serde_json::Value;

pub(super) fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    match tool.family() {
        "sandbox_exec" => exec(tool, width, theme, expanded),
        "sandbox_get_process" => get_process(tool, width, theme, expanded),
        "sandbox_kill_process" => process_control(tool, "Stop process", width, theme, expanded),
        "sandbox_list_files" => file_operation(tool, "List files", width, theme, expanded),
        "sandbox_start_process" => process(tool, width, theme, expanded),
        "preview" => preview(tool, width, theme, expanded),
        "sandbox_read_file" => file_operation(tool, "Read file", width, theme, expanded),
        "sandbox_write_file" => file_operation(tool, "Write file", width, theme, expanded),
        _ => super::generic(tool, width, theme, expanded),
    }
}

fn process_control(
    tool: &ToolEntry,
    title: &str,
    width: u16,
    theme: &Theme,
    expanded: bool,
) -> Presentation {
    let process_id = tool
        .arguments
        .get("process_id")
        .and_then(Value::as_str)
        .unwrap_or("<process unavailable>");
    let mut presentation = Presentation::new(title, process_id).truncate_summary();
    if let Some(outcome) = process_status(tool.result.as_ref()) {
        presentation = presentation.outcome(outcome);
    }
    if expanded {
        super::with_generic_details(presentation, tool, width, theme)
    } else {
        presentation
    }
}

fn file_operation(
    tool: &ToolEntry,
    title: &str,
    width: u16,
    theme: &Theme,
    expanded: bool,
) -> Presentation {
    let path = tool
        .arguments
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("/workspace");
    let presentation = Presentation::new(title, path).truncate_summary();
    if expanded {
        super::with_generic_details(presentation, tool, width, theme)
    } else {
        presentation
    }
}

fn get_process(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let process_id = tool
        .arguments
        .get("process_id")
        .and_then(Value::as_str)
        .unwrap_or("<process unavailable>");
    let mut presentation = Presentation::new("Check process", process_id).truncate_summary();
    if let Some(outcome) = process_status(tool.result.as_ref()) {
        presentation = presentation.outcome(outcome);
    }
    if !expanded {
        return presentation;
    }
    let command = tool
        .result
        .as_ref()
        .and_then(|result| result.get("command"))
        .and_then(Value::as_str);
    execution_details(presentation, tool, command, width, theme)
}

fn exec(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let command = tool
        .arguments
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("<command unavailable>");
    let mut presentation =
        Presentation::new("Run command", format!("$ {command}")).truncate_summary();
    if let Some(outcome) = exec_outcome(tool.result.as_ref()) {
        presentation = presentation.outcome(outcome);
    }
    if !expanded {
        return presentation;
    }

    if let Some(cwd) = tool.arguments.get("cwd").and_then(Value::as_str) {
        presentation = presentation.unselectable_details(super::super::markdown::wrap_plain(
            &format!("cwd {cwd}"),
            width,
            Style::default().fg(theme.muted()),
        ));
    }
    execution_details(presentation, tool, Some(command), width, theme)
}

fn execution_details(
    mut presentation: Presentation,
    tool: &ToolEntry,
    command: Option<&str>,
    width: u16,
    theme: &Theme,
) -> Presentation {
    if let Some(command) = command {
        presentation = labeled_text(presentation, "command", command, width, theme);
    }
    let stdout = result_string(tool, "stdout");
    let stderr = result_string(tool, "stderr");
    if let Some(stdout) = stdout {
        presentation = labeled_text(presentation, "stdout", nonempty(stdout), width, theme);
    }
    if let Some(stderr) = stderr {
        presentation = labeled_text(presentation, "stderr", nonempty(stderr), width, theme);
    }
    let bytes = stdout.map_or(0, str::len) + stderr.map_or(0, str::len);
    presentation.footer(format!("stdout/stderr · {}", format_bytes(bytes)))
}

fn process(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let command = process_command(&tool.arguments);
    let mut presentation = Presentation::new("Start process", command).truncate_summary();
    if let Some(outcome) = process_outcome(tool.result.as_ref()) {
        presentation = presentation.outcome(outcome);
    }
    if expanded {
        super::with_generic_details(presentation, tool, width, theme)
    } else {
        presentation
    }
}

fn preview(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let subject = tool
        .arguments
        .get("port")
        .and_then(Value::as_u64)
        .map_or_else(
            || "port unavailable".to_owned(),
            |port| format!("port {port}"),
        );
    let mut presentation = Presentation::new("Open preview", subject);
    if tool
        .result
        .as_ref()
        .and_then(|result| result.get("url"))
        .and_then(Value::as_str)
        .is_some_and(safe_http_url)
    {
        presentation = presentation.outcome("preview ready");
    }
    if expanded {
        super::with_generic_details(presentation, tool, width, theme)
    } else {
        presentation
    }
}

fn labeled_text(
    mut presentation: Presentation,
    label: &str,
    text: &str,
    width: u16,
    theme: &Theme,
) -> Presentation {
    presentation = presentation.unselectable_details(super::super::markdown::wrap_plain(
        label,
        width,
        Style::default().fg(theme.muted()),
    ));
    presentation.selectable_plain(text, width, Style::default().fg(theme.text()))
}

fn exec_outcome(result: Option<&Value>) -> Option<String> {
    let result = result?;
    if let Some(code) = result.get("exit_code").and_then(Value::as_i64) {
        return Some(format!("exit {code}"));
    }
    if let Some(status) = result.get("status").and_then(Value::as_str) {
        return Some(status.replace('_', " "));
    }
    result
        .get("success")
        .and_then(Value::as_bool)
        .map(|success| if success { "succeeded" } else { "failed" }.to_owned())
}

fn process_outcome(result: Option<&Value>) -> Option<String> {
    let result = result?;
    let mut parts = Vec::new();
    if let Some(pid) = result.get("pid").and_then(value_label) {
        parts.push(format!("PID {pid}"));
    } else if let Some(process_id) = result.get("process_id").and_then(value_label) {
        parts.push(format!("process {process_id}"));
    }
    if let Some(status) = result.get("status").and_then(Value::as_str) {
        parts.push(status.replace('_', " "));
    }
    if let Some(port) = result.get("ready_port").and_then(Value::as_u64) {
        parts.push(format!("port {port} ready"));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn process_status(result: Option<&Value>) -> Option<String> {
    let result = result?;
    if result.get("found").and_then(Value::as_bool) == Some(false) {
        return Some("not found".to_owned());
    }
    let mut parts = Vec::new();
    if let Some(status) = result.get("status").and_then(Value::as_str) {
        parts.push(status.replace('_', " "));
    }
    if let Some(code) = result.get("exit_code").and_then(Value::as_i64) {
        parts.push(format!("exit {code}"));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn process_command(arguments: &Value) -> String {
    let mut command = arguments
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("<command unavailable>")
        .to_owned();
    if let Some(args) = arguments.get("args").and_then(Value::as_array) {
        for argument in args.iter().filter_map(Value::as_str) {
            command.push(' ');
            command.push_str(argument);
        }
    }
    command
}

fn result_string<'a>(tool: &'a ToolEntry, field: &str) -> Option<&'a str> {
    tool.result.as_ref()?.get(field)?.as_str()
}

fn value_label(value: &Value) -> Option<String> {
    value
        .as_i64()
        .map(|value| value.to_string())
        .or_else(|| value.as_str().map(str::to_owned))
}

fn nonempty(text: &str) -> &str {
    if text.is_empty() { "(empty)" } else { text }
}

fn safe_http_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}
