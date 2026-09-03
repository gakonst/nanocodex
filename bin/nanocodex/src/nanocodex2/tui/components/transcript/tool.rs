// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

mod account;
mod browser;
mod code;
mod mcp;
mod media;
mod memory;
mod patch;
mod plan;
mod sandbox;
mod shell;
mod subagent;
mod web;

use super::markdown::{
    Layout, SourceSpan, plain_selection_spans_excluding, sanitize, wrap_plain, wrap_spans,
};
use crate::tui::{
    format::{format_duration, humanize_tool},
    theme::Theme,
    transcript::{ToolEntry, ToolState, is_subagent_tool},
};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde_json::Value;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_EXPANDED_DETAIL_LINES: usize = 128;
const MAX_DETAIL_SECTION_LINES: usize = MAX_EXPANDED_DETAIL_LINES / 2;
const MAX_EXPANDED_TEXT_BYTES: usize = 24 * 1024;

fn count_label(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

#[cfg(test)]
pub(super) fn render(tool: &ToolEntry, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    render_layout(tool, None, width, theme, false).lines
}

#[cfg(test)]
pub(super) fn render_expanded(tool: &ToolEntry, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    render_layout(tool, None, width, theme, true).lines
}

#[cfg(test)]
pub(super) fn render_live(
    tool: &ToolEntry,
    duration_ns: u64,
    width: u16,
    theme: &Theme,
    expanded: bool,
) -> Vec<Line<'static>> {
    render_layout(tool, Some(duration_ns), width, theme, expanded).lines
}

pub(super) fn render_layout(
    tool: &ToolEntry,
    live_duration_ns: Option<u64>,
    width: u16,
    theme: &Theme,
    expanded: bool,
) -> Layout {
    if width == 0 {
        return Layout {
            lines: Vec::new(),
            images: Vec::new(),
            links: Vec::new(),
            selections: Vec::new(),
            envelopes: Vec::new(),
            selection_source: None,
            image_state: super::markdown::ImageState::None,
        };
    }
    let detail_width = width.saturating_sub(6).max(1);
    let presentation = present(tool, detail_width, theme, expanded);
    let mut lines = summary_lines(
        tool,
        &presentation,
        live_duration_ns,
        width,
        theme,
        expanded,
    );
    if !expanded {
        return Layout {
            links: vec![Vec::new(); lines.len()],
            selections: vec![Vec::new(); lines.len()],
            lines,
            images: Vec::new(),
            envelopes: Vec::new(),
            selection_source: None,
            image_state: super::markdown::ImageState::None,
        };
    }
    let detail_start = lines.len();
    let Presentation {
        details,
        footer,
        selection_source,
        mut detail_selections,
        ..
    } = presentation;
    let selection_source = (!selection_source.is_empty()).then_some(selection_source);
    append_details(&mut lines, details, footer, width, theme);
    let mut selections = vec![Vec::new(); lines.len()];
    let prefix = if width < 7 { 0 } else { 6 };
    for (index, spans) in detail_selections.iter_mut().enumerate() {
        let row = detail_start + index;
        for span in &mut *spans {
            span.columns.start = span.columns.start.saturating_add(prefix);
            span.columns.end = span.columns.end.saturating_add(prefix).min(width);
        }
        spans.retain(|span| span.columns.start < span.columns.end);
        selections[row] = std::mem::take(spans);
    }
    Layout {
        links: vec![Vec::new(); lines.len()],
        lines,
        images: Vec::new(),
        selections,
        envelopes: Vec::new(),
        selection_source,
        image_state: super::markdown::ImageState::None,
    }
}

pub(super) fn render_live_summary(
    tool: &ToolEntry,
    duration_ns: u64,
    width: u16,
    theme: &Theme,
    expanded: bool,
) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let presentation = present(tool, width.saturating_sub(6).max(1), theme, false);
    summary_lines(
        tool,
        &presentation,
        Some(duration_ns),
        width,
        theme,
        expanded,
    )
}

fn present(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    if tool.has_mcp_origin() {
        return mcp::present(tool, width, theme, expanded);
    }
    let family = tool.family();
    if is_subagent_tool(family) {
        return subagent::present(tool, width, theme, expanded);
    }
    match family {
        "accountInfo" | "account_connectors" | "runtimeInfo" => {
            account::present(tool, width, theme, expanded)
        }
        family if family.starts_with("sandbox_") => sandbox::present(tool, width, theme, expanded),
        "exec_command" | "write_stdin" => shell::present(tool, width, theme, expanded),
        "preview" => sandbox::present(tool, width, theme, expanded),
        "update_plan" => plan::present(tool, width, theme, expanded),
        "apply_patch" => patch::present(tool, width, theme, expanded),
        "web__run" => web::present(tool, width, theme, expanded),
        "browser" | "browser_execute" => browser::present(tool, width, theme, expanded),
        "view_image" | "image_gen__imagegen" => media::present(tool, width, theme, expanded),
        "memory" => memory::present(tool, width, theme, expanded),
        "exec" | "wait" => code::present(tool, width, theme, expanded),
        _ => generic(tool, width, theme, expanded),
    }
}

pub(super) struct Presentation {
    title: String,
    subject: Subject,
    outcome: Option<String>,
    details: Vec<Line<'static>>,
    footer: Option<String>,
    summary_overflow: SummaryOverflow,
    selection_source: String,
    detail_selections: Vec<Vec<SourceSpan>>,
}

enum Subject {
    Plain(String),
    Styled(Vec<Span<'static>>),
}

enum SummaryOverflow {
    Wrap,
    Truncate,
}

const TRUNCATION_MARKER: &str = " …";
const TRUNCATION_MARKER_WIDTH: u16 = 2;

impl Presentation {
    pub(super) fn new(title: impl Into<String>, subject: impl Into<String>) -> Self {
        Self {
            title: capitalize_title(&title.into()),
            subject: Subject::Plain(subject.into()),
            outcome: None,
            details: Vec::new(),
            footer: None,
            summary_overflow: SummaryOverflow::Wrap,
            selection_source: String::new(),
            detail_selections: Vec::new(),
        }
    }

    pub(super) fn styled_subject(title: impl Into<String>, subject: Vec<Span<'static>>) -> Self {
        Self {
            title: capitalize_title(&title.into()),
            subject: Subject::Styled(subject),
            outcome: None,
            details: Vec::new(),
            footer: None,
            summary_overflow: SummaryOverflow::Wrap,
            selection_source: String::new(),
            detail_selections: Vec::new(),
        }
    }

    pub(super) fn outcome(mut self, outcome: impl Into<String>) -> Self {
        self.outcome = Some(outcome.into());
        self
    }

    pub(super) fn unselectable_details(mut self, details: Vec<Line<'static>>) -> Self {
        let details = self.bounded_details(details);
        self.detail_selections
            .resize_with(self.detail_selections.len() + details.len(), Vec::new);
        self.details.extend(details);
        self
    }

    pub(super) fn selectable_details(
        self,
        source: impl Into<String>,
        details: Vec<Line<'static>>,
    ) -> Self {
        self.selectable_details_excluding(source, details, &[])
    }

    pub(super) fn selectable_details_excluding(
        mut self,
        source: impl Into<String>,
        details: Vec<Line<'static>>,
        exclusions: &[Vec<Range<u16>>],
    ) -> Self {
        let source = bounded_text(&source.into());
        let details = self.bounded_details(details);
        let offset = if self.selection_source.is_empty() {
            0
        } else {
            self.selection_source.push('\n');
            self.selection_source.len()
        };
        let mut selections = plain_selection_spans_excluding(&source, &details, exclusions);
        for spans in &mut selections {
            for span in spans {
                span.source.start = span.source.start.saturating_add(offset);
                span.source.end = span.source.end.saturating_add(offset);
            }
        }
        self.selection_source.push_str(&source);
        self.details.extend(details);
        self.detail_selections.extend(selections);
        self
    }

    fn bounded_details(&self, mut details: Vec<Line<'static>>) -> Vec<Line<'static>> {
        let remaining = MAX_EXPANDED_DETAIL_LINES.saturating_sub(self.details.len());
        if details.len() <= remaining {
            return details;
        }
        if remaining == 0 {
            return Vec::new();
        }
        details.truncate(remaining.saturating_sub(1));
        details.push(Line::from("… expanded output truncated …"));
        details
    }

    pub(super) fn selectable_plain(
        self,
        source: impl Into<String>,
        width: u16,
        style: Style,
    ) -> Self {
        let source = bounded_text(&source.into());
        let lines = wrap_plain(&source, width, style);
        self.selectable_details(source, lines)
    }

    pub(super) fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    pub(super) fn truncate_summary(mut self) -> Self {
        self.summary_overflow = SummaryOverflow::Truncate;
        self
    }
}

fn capitalize_title(title: &str) -> String {
    let mut capitalize_next = true;
    let mut capitalized = String::with_capacity(title.len());
    for character in title.chars() {
        if capitalize_next && character.is_alphanumeric() {
            capitalized.extend(character.to_uppercase());
            capitalize_next = false;
        } else {
            capitalized.push(character);
        }
    }
    capitalized
}

fn summary_lines(
    tool: &ToolEntry,
    presentation: &Presentation,
    live_duration_ns: Option<u64>,
    width: u16,
    theme: &Theme,
    expanded: bool,
) -> Vec<Line<'static>> {
    let border = Style::default().fg(theme.border());
    let status = status_style(tool.state, theme);
    let prefix = vec![
        Span::raw("  "),
        Span::styled(if expanded { "▼ " } else { "▶ " }, border),
        Span::styled(format!("{} ", status_symbol(tool.state)), status),
    ];
    let mut content = Vec::new();
    append_span(
        &mut content,
        &presentation.title,
        Style::default()
            .fg(theme.text())
            .add_modifier(Modifier::BOLD),
    );
    push_subject(&mut content, &presentation.subject, theme);
    let mut outcome_spans = Vec::new();
    if let Some(outcome) = &presentation.outcome {
        append_span(
            &mut outcome_spans,
            &format!(" · {outcome}"),
            Style::default().fg(theme.muted()),
        );
    }
    let mut origin_spans = Vec::new();
    append_span(
        &mut origin_spans,
        &format!(" · {}", tool.execution_qualifier()),
        Style::default().fg(theme.muted()),
    );
    let mut error_spans = Vec::new();
    if tool.state == ToolState::Failed
        && let Some(error) = first_error_line(tool.result.as_ref())
    {
        append_span(
            &mut error_spans,
            &format!(" · {error}"),
            Style::default().fg(theme.thinking_xhigh()),
        );
    }
    let mut duration_spans = Vec::new();
    if let Some(duration) = live_duration_ns.or(tool.duration_ns) {
        append_span(
            &mut duration_spans,
            &format!(" · {}", format_duration(duration)),
            Style::default().fg(theme.muted()),
        );
    }

    if matches!(presentation.summary_overflow, SummaryOverflow::Truncate) {
        let title_span_count = prefix.len() + usize::from(!content.is_empty());
        let leading = prefix
            .into_iter()
            .chain(content)
            .chain(origin_spans)
            .collect::<Vec<_>>();
        let full_summary = leading
            .iter()
            .chain(&outcome_spans)
            .chain(&error_spans)
            .chain(&duration_spans)
            .cloned()
            .collect::<Vec<_>>();
        if !spans_need_truncation(&full_summary, width) {
            return vec![Line::from(full_summary)];
        }

        let suffix = outcome_spans
            .into_iter()
            .chain(duration_spans)
            .collect::<Vec<_>>();
        let suffix_width = spans_width(&suffix);
        let minimum_leading_width =
            spans_width(&leading[..title_span_count]).saturating_add(TRUNCATION_MARKER_WIDTH);
        if suffix_width >= width || width - suffix_width < minimum_leading_width {
            return vec![truncate_spans_with_ellipsis(
                &full_summary,
                width,
                Style::default().fg(theme.muted()),
            )];
        }

        let leading_width = width - suffix_width;
        let mut line = truncate_spans_with_ellipsis(
            &leading,
            leading_width,
            Style::default().fg(theme.muted()),
        );
        line.spans.extend(suffix);
        return vec![line];
    }

    content.extend(origin_spans);
    content.extend(outcome_spans);
    content.extend(error_spans);
    content.extend(duration_spans);

    const PREFIX_WIDTH: u16 = 6;
    if width <= PREFIX_WIDTH {
        let spans = prefix.into_iter().chain(content).collect::<Vec<_>>();
        return wrap_spans(&spans, width, true);
    }

    let mut lines = wrap_spans(&content, width - PREFIX_WIDTH, true);
    for (index, line) in lines.iter_mut().enumerate() {
        let line_prefix = if index == 0 {
            prefix.clone()
        } else {
            vec![Span::raw("      ")]
        };
        line.spans.splice(0..0, line_prefix);
    }
    lines
}

fn spans_need_truncation(spans: &[Span<'static>], width: u16) -> bool {
    spans.iter().any(|span| span.content.contains(['\n', '\r'])) || spans_width(spans) > width
}

fn spans_width(spans: &[Span<'static>]) -> u16 {
    spans.iter().fold(0_u16, |total, span| {
        let width =
            u16::try_from(UnicodeWidthStr::width(span.content.as_ref())).unwrap_or(u16::MAX);
        total.saturating_add(width)
    })
}

fn truncate_spans_with_ellipsis(
    spans: &[Span<'static>],
    width: u16,
    ellipsis_style: Style,
) -> Line<'static> {
    if width == 0 {
        return Line::default();
    }
    let marker = if width < TRUNCATION_MARKER_WIDTH {
        "…"
    } else {
        TRUNCATION_MARKER
    };
    let mut rendered = Vec::new();
    let marker_width =
        u16::try_from(UnicodeWidthStr::width(marker)).unwrap_or(TRUNCATION_MARKER_WIDTH);
    let mut remaining = width.saturating_sub(marker_width);
    for span in spans {
        let line_end = span.content.find(['\n', '\r']);
        let content = line_end.map_or(span.content.as_ref(), |end| &span.content[..end]);
        let shortened = truncate(content, remaining);
        let fully_rendered = shortened == content;
        let used = u16::try_from(UnicodeWidthStr::width(shortened.as_str())).unwrap_or(u16::MAX);
        remaining = remaining.saturating_sub(used);
        if !shortened.is_empty() {
            rendered.push(Span::styled(shortened, span.style));
        }
        if line_end.is_some() || !fully_rendered {
            break;
        }
    }
    rendered.push(Span::styled(marker, ellipsis_style));
    Line::from(rendered)
}

fn append_span(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    let text = sanitize(text);
    if !text.is_empty() {
        spans.push(Span::styled(text, style));
    }
}

fn push_subject(spans: &mut Vec<Span<'static>>, subject: &Subject, theme: &Theme) {
    match subject {
        Subject::Plain(subject) if !subject.is_empty() => {
            append_span(
                spans,
                &format!("  {subject}"),
                Style::default().fg(theme.text()),
            );
        }
        Subject::Styled(subject) if !subject.is_empty() => {
            append_span(spans, "  ", Style::default());
            for span in subject {
                append_span(spans, &span.content, span.style);
            }
        }
        Subject::Plain(_) | Subject::Styled(_) => {}
    }
}

fn append_details(
    lines: &mut Vec<Line<'static>>,
    details: Vec<Line<'static>>,
    footer: Option<String>,
    width: u16,
    theme: &Theme,
) {
    let rail = Style::default().fg(theme.border());
    if width < 7 {
        lines.extend(details.into_iter().map(|line| truncate_line(line, width)));
        if let Some(footer) = footer {
            lines.push(Line::from(Span::styled(
                truncate(&sanitize(&footer), width),
                Style::default().fg(theme.muted()),
            )));
        }
        return;
    }
    for detail in details {
        lines.push(Line::from(
            std::iter::once(Span::styled("    │ ", rail))
                .chain(detail.spans)
                .collect::<Vec<_>>(),
        ));
    }
    let footer = footer.unwrap_or_else(|| "details".to_owned());
    let footer = truncate(&sanitize(&footer), width.saturating_sub(6));
    lines.push(Line::from(vec![
        Span::styled("    └ ", rail),
        Span::styled(footer, Style::default().fg(theme.muted())),
    ]));
}

fn truncate_line(line: Line<'static>, width: u16) -> Line<'static> {
    let mut spans = Vec::new();
    let mut remaining = width;
    for span in line.spans {
        push_span(&mut spans, &mut remaining, &span.content, span.style);
        if remaining == 0 {
            break;
        }
    }
    Line::from(spans)
}

fn generic(tool: &ToolEntry, width: u16, theme: &Theme, expanded: bool) -> Presentation {
    let family = tool.family();
    let title = humanize_tool(family.strip_prefix("sandbox_").unwrap_or(family));
    let subject = meaningful_subject(&tool.arguments).unwrap_or_else(|| {
        let count = tool.arguments.as_object().map_or(0, serde_json::Map::len);
        format!("{count} arguments")
    });
    let mut presentation = Presentation::new(title, subject);
    if let Some(outcome) = generic_outcome(tool.result.as_ref()) {
        presentation = presentation.outcome(outcome);
    }
    if !expanded {
        return presentation;
    }
    with_generic_details(presentation, tool, width, theme)
}

fn with_generic_details(
    mut presentation: Presentation,
    tool: &ToolEntry,
    width: u16,
    theme: &Theme,
) -> Presentation {
    let details = bounded_section(pretty_value(&tool.arguments, width, theme));
    presentation = presentation.unselectable_details(details);
    if let Some(result) = &tool.result {
        let (source, details) = selectable_result(result, width, theme);
        presentation = presentation.selectable_details(source, bounded_section(details));
    }
    presentation.footer = Some("arguments and result".to_owned());
    presentation
}

pub(super) fn selectable_result(
    value: &Value,
    width: u16,
    theme: &Theme,
) -> (String, Vec<Line<'static>>) {
    if contains_image_data(value) {
        let source = "image data hidden".to_owned();
        let details = wrap_plain(&source, width, Style::default().fg(theme.muted()));
        return (source, details);
    }
    if let Some(text) = value.as_str() {
        let text = bounded_text(text);
        return (
            text.clone(),
            wrap_plain(&text, width, Style::default().fg(theme.text())),
        );
    }
    let source = bounded_json(value);
    let details = wrap_plain(
        &source,
        width,
        Style::default()
            .fg(theme.code_text())
            .bg(theme.code_background()),
    );
    (source, details)
}

pub(super) fn pretty_value(value: &Value, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let rendered = bounded_json(value);
    wrap_plain(
        &rendered,
        width,
        Style::default()
            .fg(theme.code_text())
            .bg(theme.code_background()),
    )
}

pub(super) fn format_bytes(bytes: usize) -> String {
    if bytes >= 1_048_576 {
        return format!("{:.1} MiB", bytes as f64 / 1_048_576.0);
    }
    if bytes >= 1024 {
        return format!("{:.1} KiB", bytes as f64 / 1024.0);
    }
    format!("{bytes} B")
}

fn meaningful_subject(arguments: &Value) -> Option<String> {
    [
        "path",
        "file_path",
        "query",
        "prompt",
        "url",
        "name",
        "command",
        "action",
        "operation",
        "process_id",
        "session_id",
        "port",
    ]
    .into_iter()
    .find_map(|key| arguments.get(key).and_then(scalar_summary))
    .map(|value| sanitize(value.lines().next().unwrap_or_default()))
}

fn scalar_summary(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

fn generic_outcome(result: Option<&Value>) -> Option<String> {
    let fields = result?.as_object()?;
    if let Some(status) = fields.get("status").and_then(Value::as_str) {
        return Some(status.replace('_', " "));
    }
    fields.get("ok").and_then(Value::as_bool).map(|ok| {
        if ok {
            "succeeded".to_owned()
        } else {
            "failed".to_owned()
        }
    })
}

fn first_error_line(result: Option<&Value>) -> Option<String> {
    let result = result?;
    let text = error_text(result)?;
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(sanitize)
}

fn error_text(value: &Value) -> Option<&str> {
    if let Some(text) = value.as_str() {
        return Some(text);
    }
    if let Some(items) = value.as_array() {
        return items.iter().find_map(error_text);
    }
    let fields = value.as_object()?;
    for key in ["error", "message", "stderr", "output", "text"] {
        if let Some(text) = fields
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
        {
            return Some(text);
        }
    }
    for key in ["errors", "content", "details", "cause"] {
        if let Some(text) = fields.get(key).and_then(error_text) {
            return Some(text);
        }
    }
    None
}

pub(super) fn bounded_text(text: &str) -> String {
    if text.len() <= MAX_EXPANDED_TEXT_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_EXPANDED_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… output truncated …", &text[..end])
}

fn bounded_json(value: &Value) -> String {
    let mut writer = CappedWriter::default();
    let _ = serde_json::to_writer_pretty(&mut writer, value);
    writer.finish()
}

#[derive(Default)]
struct CappedWriter {
    bytes: Vec<u8>,
    truncated: bool,
}

impl CappedWriter {
    fn finish(self) -> String {
        let mut rendered = String::from_utf8_lossy(&self.bytes).into_owned();
        if self.truncated {
            rendered.push_str("\n… output truncated …");
        }
        rendered
    }
}

impl std::io::Write for CappedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let remaining = MAX_EXPANDED_TEXT_BYTES.saturating_sub(self.bytes.len());
        if remaining == 0 {
            self.truncated = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "tool detail limit reached",
            ));
        }
        let accepted = remaining.min(bytes.len());
        self.bytes.extend_from_slice(&bytes[..accepted]);
        if accepted < bytes.len() {
            self.truncated = true;
        }
        Ok(accepted)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn bounded_section(mut details: Vec<Line<'static>>) -> Vec<Line<'static>> {
    if details.len() <= MAX_DETAIL_SECTION_LINES {
        return details;
    }
    details.truncate(MAX_DETAIL_SECTION_LINES.saturating_sub(1));
    details.push(Line::from(
        "… section truncated · expanded output truncated …",
    ));
    details
}

fn contains_image_data(value: &Value) -> bool {
    match value {
        Value::String(text) => text.starts_with("data:image/"),
        Value::Array(values) => values.iter().any(contains_image_data),
        Value::Object(values) => values.values().any(contains_image_data),
        _ => false,
    }
}

fn push_span(spans: &mut Vec<Span<'static>>, remaining: &mut u16, text: &str, style: Style) {
    if *remaining == 0 {
        return;
    }
    let rendered = truncate(text, *remaining);
    let used = u16::try_from(UnicodeWidthStr::width(rendered.as_str())).unwrap_or(u16::MAX);
    *remaining = remaining.saturating_sub(used);
    if !rendered.is_empty() {
        spans.push(Span::styled(rendered, style));
    }
}

fn truncate(text: &str, width: u16) -> String {
    let mut rendered = String::new();
    let mut used = 0_u16;
    for grapheme in text.graphemes(true) {
        let next = used
            .saturating_add(u16::try_from(UnicodeWidthStr::width(grapheme)).unwrap_or(u16::MAX));
        if next > width {
            break;
        }
        rendered.push_str(grapheme);
        used = next;
    }
    rendered
}

fn status_symbol(state: ToolState) -> &'static str {
    match state {
        ToolState::Running => "◌",
        ToolState::Succeeded => "✓",
        ToolState::Failed => "×",
    }
}

fn status_style(state: ToolState, theme: &Theme) -> Style {
    let color = match state {
        ToolState::Running => theme.accent(),
        ToolState::Succeeded => Color::Green,
        ToolState::Failed => theme.thinking_xhigh(),
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_EXPANDED_DETAIL_LINES, MAX_EXPANDED_TEXT_BYTES, bounded_json, render, render_expanded,
        render_layout, render_live,
    };
    use crate::tui::{
        theme::Theme,
        transcript::{ToolEntry, ToolState},
    };
    use ratatui::style::{Color, Modifier};
    use serde_json::json;

    fn tool(name: &str, arguments: serde_json::Value) -> ToolEntry {
        let execution = ToolEntry::inferred_execution(name, &arguments, None);
        ToolEntry {
            name: name.to_owned(),
            arguments,
            started_at_unix_ms: 0,
            state: ToolState::Succeeded,
            duration_ns: Some(1_200_000_000),
            result: None,
            metadata: None,
            execution,
            substeps: Vec::new(),
            child_count: 0,
        }
    }

    #[test]
    fn completed_shell_is_a_single_collapsed_summary() {
        let mut shell = tool(
            "exec_command",
            json!({"cmd": "cargo test", "workdir": "/work"}),
        );
        shell.result = Some(json!({
            "output": "all tests passed\nsecond line",
            "exit_code": 0,
            "wall_time_seconds": 1.2,
        }));

        let lines = render(&shell, 80, &Theme::default());

        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].to_string(),
            "  ▶ ✓ Shell  $ cargo test · Local · exit 0 · 1.2s"
        );
        let checkmark = lines[0]
            .spans
            .iter()
            .find(|span| span.content == "✓ ")
            .expect("successful tool should render a checkmark");
        assert_eq!(checkmark.style.fg, Some(Color::Green));
    }

    #[test]
    fn shell_commands_use_prompt_and_syntax_colors() {
        let shell = tool(
            "exec_command",
            json!({"cmd": "if test \"$HOME\"; then echo ok; fi"}),
        );
        let theme = Theme::default();

        for (lines, expected_commands) in [
            (render(&shell, 80, &theme), 1),
            (render_expanded(&shell, 80, &theme), 2),
        ] {
            let spans = lines
                .iter()
                .flat_map(|line| &line.spans)
                .collect::<Vec<_>>();
            let prompts = spans
                .iter()
                .filter(|span| span.content == "$ ")
                .collect::<Vec<_>>();
            let keywords = spans
                .iter()
                .filter(|span| span.content.contains("if"))
                .collect::<Vec<_>>();

            assert_eq!(prompts.len(), expected_commands);
            assert_eq!(keywords.len(), expected_commands);
            assert!(
                prompts
                    .iter()
                    .all(|prompt| prompt.style.fg == Some(Color::Yellow))
            );
            assert!(keywords.iter().all(|keyword| {
                keyword.style.add_modifier.contains(Modifier::BOLD)
                    && keyword.style.fg == Some(Color::Blue)
            }));
        }
    }

    #[test]
    fn code_workflow_uses_the_compact_workflow_label() {
        let mut workflow = tool(
            "exec",
            json!("await tools.exec_command({cmd: \"cargo test\"})"),
        );
        workflow.child_count = 2;

        let lines = render(&workflow, 80, &Theme::default());

        assert_eq!(lines[0].to_string(), "  ▶ ✓ Batch  2 tools · Local · 1.2s");
        assert!(lines.iter().all(|line| line.width() <= 80));
    }

    #[test]
    fn single_child_code_wrapper_is_compact_and_ignores_status_header() {
        let mut workflow = tool("exec", json!("text(await tools.accountInfo({}))"));
        workflow.child_count = 1;
        workflow.result = Some(json!([
            {
                "type": "input_text",
                "text": "Script completed\nWall time 0.1 seconds\nOutput:\n"
            },
            {"type": "input_text", "text": "summary"}
        ]));

        let collapsed = render(&workflow, 80, &Theme::default())[0].to_string();
        let expanded = render_expanded(&workflow, 80, &Theme::default())
            .iter()
            .map(ToString::to_string)
            .collect::<String>();

        assert!(collapsed.contains("Code  1 emitted item"), "{collapsed}");
        assert!(!collapsed.contains("Batch  1 tool"), "{collapsed}");
        assert!(expanded.contains("summary"), "{expanded}");
        assert!(!expanded.contains("Script completed"), "{expanded}");
    }

    #[test]
    fn code_renders_each_counted_image_and_audio_output() {
        let mut workflow = tool("exec", json!("emit media"));
        workflow.result = Some(json!([
            {
                "type": "input_text",
                "text": "Script completed\nWall time 0.1 seconds\nOutput:\n"
            },
            {
                "type": "input_image",
                "image_url": "data:image/png;base64,IMAGE_BYTES",
                "detail": "high"
            },
            {
                "type": "input_audio",
                "audio_url": "data:audio/wav;base64,AUDIO_BYTES"
            }
        ]));

        let collapsed = render(&workflow, 80, &Theme::default())[0].to_string();
        let expanded = render_expanded(&workflow, 80, &Theme::default())
            .iter()
            .map(ToString::to_string)
            .collect::<String>();

        assert!(collapsed.contains("Code  2 emitted items"), "{collapsed}");
        assert!(
            expanded.contains("image output · high · binary data hidden"),
            "{expanded}"
        );
        assert!(
            expanded.contains("audio output · binary data hidden"),
            "{expanded}"
        );
        assert!(!expanded.contains("Script completed"), "{expanded}");
        assert!(!expanded.contains("IMAGE_BYTES"), "{expanded}");
        assert!(!expanded.contains("AUDIO_BYTES"), "{expanded}");
    }

    #[test]
    fn account_info_uses_bounded_semantic_summary_and_details() {
        let mut account = tool("accountInfo", json!({}));
        account.result = Some(json!({
            "status": "ready",
            "authenticated": ["github"],
            "accounts": {"github": "octocat@example.test"},
            "connectorAccounts": {
                "github": [
                    {"id": "github-1", "label": "Work GitHub"},
                    {"id": "github-2", "label": "Personal GitHub"}
                ]
            },
            "machines": [
                {
                    "id": "sandbox",
                    "kind": "sandbox",
                    "name": "Account sandbox",
                    "workspace": "/workspace",
                    "capabilities": ["shell", "files"]
                },
                {
                    "id": "laptop",
                    "kind": "user",
                    "name": "Alice's Mac",
                    "workspace": "/Users/alice/project",
                    "capabilities": ["exec_command"]
                }
            ],
            "identity": {},
            "stablecoins": [],
            "authorizations": [],
            "vault": [
                {
                    "id": "vault-1",
                    "kind": "login",
                    "name": "GitHub",
                    "created_at": 1,
                    "username": "octocat"
                }
            ],
            "ignored_future_field": {"secret": "must not be dumped"}
        }));

        let collapsed = render(&account, 120, &Theme::default())[0].to_string();
        let expanded = render_expanded(&account, 120, &Theme::default())
            .iter()
            .map(ToString::to_string)
            .collect::<String>();

        for expected in [
            "Account info  ready",
            "Account",
            "2 connections",
            "2 machines",
            "1 Vault item",
        ] {
            assert!(
                collapsed.contains(expected),
                "missing {expected:?}: {collapsed}"
            );
        }
        for expected in [
            "github  Work GitHub, Personal GitHub",
            "Machine Account sandbox  sandbox · /workspace · shell, files",
            "Machine Alice's Mac  user · /Users/alice/project · exec_command",
            "Vault · GitHub  login · octocat",
        ] {
            assert!(
                expanded.contains(expected),
                "missing {expected:?}: {expanded}"
            );
        }
        assert!(!expanded.contains("ignored_future_field"), "{expanded}");
        assert!(!expanded.contains("must not be dumped"), "{expanded}");
        assert!(!expanded.contains("\"status\""), "{expanded}");
    }

    #[test]
    fn account_info_renders_account_label_when_connector_accounts_are_empty() {
        let mut account = tool("accountInfo", json!({}));
        account.result = Some(json!({
            "status": "ready",
            "authenticated": ["github"],
            "accounts": {"github": "octocat@example.test"},
            "connectorAccounts": {},
            "machines": []
        }));

        let summary = render(&account, 120, &Theme::default())[0].to_string();
        let expanded = render_expanded(&account, 120, &Theme::default())
            .iter()
            .map(ToString::to_string)
            .collect::<String>();

        assert!(summary.contains("1 connector"), "{summary}");
        assert!(
            expanded.contains("github  octocat@example.test"),
            "{expanded}"
        );
    }

    #[test]
    fn account_and_runtime_tools_have_semantic_origins() {
        let account = render(&tool("accountInfo", json!({})), 80, &Theme::default())[0].to_string();
        let runtime = render(&tool("runtimeInfo", json!({})), 80, &Theme::default())[0].to_string();

        assert!(account.contains(" · Account · "), "{account}");
        assert!(runtime.contains(" · Runtime · "), "{runtime}");
    }

    #[test]
    fn non_shell_tool_summary_wraps_instead_of_discarding_overflow() {
        let operation = tool(
            "custom_operation",
            json!({"prompt": "inspect every target without failing fast across the workspace"}),
        );

        let lines = render(&operation, 32, &Theme::default());
        let rendered = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        let rendered = rendered.split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(lines.len() > 1);
        assert!(lines.iter().all(|line| line.width() <= 32));
        assert!(
            lines
                .iter()
                .skip(1)
                .all(|line| line.to_string().starts_with("      "))
        );
        assert!(
            rendered.contains("inspect every target without failing fast across the workspace")
        );
        assert!(rendered.contains("1.2s"));
    }

    #[test]
    fn shell_summaries_truncate_subject_and_preserve_status_suffix() {
        let mut shell = tool(
            "exec_command",
            json!({"cmd": "cargo test --all-targets --no-fail-fast --workspace"}),
        );
        shell.result = Some(json!({"output": "", "exit_code": 0}));
        let stdin = tool(
            "write_stdin",
            json!({"chars": "send a long interaction to the running process"}),
        );

        let shell_line = render(&shell, 36, &Theme::default()).remove(0);
        let live_shell_line =
            render_live(&shell, 2_500_000_000, 36, &Theme::default(), false).remove(0);
        let stdin_line = render(&stdin, 32, &Theme::default()).remove(0);

        assert_eq!(shell_line.width(), 36);
        assert!(shell_line.to_string().contains("exit 0"));
        assert!(shell_line.to_string().ends_with("1.2s"));
        assert!(live_shell_line.to_string().contains("exit 0"));
        assert!(live_shell_line.to_string().ends_with("2.5s"));
        assert_eq!(stdin_line.width(), 32);
        assert!(stdin_line.to_string().ends_with(" … · 1.2s"));
    }

    #[test]
    fn shell_summary_truncates_at_the_first_explicit_newline() {
        let shell = tool("exec_command", json!({"cmd": "printf one\nprintf two"}));

        let lines = render(&shell, 80, &Theme::default());

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), "  ▶ ✓ Shell  $ printf one … · 1.2s");
    }

    #[test]
    fn collapsed_web_call_does_not_render_its_large_result() {
        let mut web = tool("web__run", json!({"search_query": [{"q": "rust ratatui"}]}));
        web.result = Some(json!("large result body\n".repeat(1_000)));

        let lines = render(&web, 80, &Theme::default());
        let rendered = lines.iter().map(ToString::to_string).collect::<String>();

        assert_eq!(lines.len(), 1);
        assert!(rendered.contains("search \"rust ratatui\""));
        assert!(!rendered.contains("large result body"));
    }

    #[test]
    fn collapsed_failure_includes_the_first_error_line() {
        let mut shell = tool("exec_command", json!({"cmd": "cargo test"}));
        shell.state = ToolState::Failed;
        shell.result = Some(json!({
            "output": "compilation failed\nmore diagnostics",
            "exit_code": 101,
        }));

        let lines = render(&shell, 80, &Theme::default());

        assert_eq!(lines.len(), 1);
        assert!(lines[0].to_string().contains("compilation failed"));
        assert!(!lines[0].to_string().contains("more diagnostics"));
    }

    #[test]
    fn killed_shell_renders_failure_without_a_checkmark() {
        let mut shell = tool("exec_command", json!({"cmd": "sleep 100"}));
        shell.state = ToolState::Failed;
        shell.result = Some(json!({"output": "", "exit_code": null}));

        let rendered = render(&shell, 80, &Theme::default())[0].to_string();

        assert!(rendered.contains("× Shell"));
        assert!(rendered.contains("terminated"));
        assert!(!rendered.contains('✓'));
    }

    #[test]
    fn expansion_reveals_shell_output() {
        let mut shell = tool("exec_command", json!({"cmd": "cargo test"}));
        shell.result = Some(json!({"output": "all tests passed", "exit_code": 0}));

        let rendered = render_expanded(&shell, 80, &Theme::default())
            .into_iter()
            .map(|line| line.to_string())
            .collect::<String>();

        assert!(rendered.contains("all tests passed"));
        assert!(rendered.contains("└ 1 line · 16 B"));
    }

    #[test]
    fn image_data_is_never_rendered_verbatim() {
        let mut image = tool("view_image", json!({"path": "image.png"}));
        image.result = Some(json!({"image_url": "data:image/png;base64,AAAA"}));

        let rendered = render_expanded(&image, 40, &Theme::default())
            .into_iter()
            .map(|line| line.to_string())
            .collect::<String>();

        assert!(!rendered.contains("base64"));
        assert!(rendered.contains("image returned"));
    }

    #[test]
    fn every_first_party_tool_has_a_semantic_summary() {
        let cases = [
            ("exec", json!("text(true)"), "Code  0 emitted items"),
            (
                "update_plan",
                json!({"plan": [{"step": "done", "status": "completed"}]}),
                "Plan  1/1 complete",
            ),
            (
                "apply_patch",
                json!("*** Begin Patch\n*** Update File: src/main.rs\n+new\n-old\n*** End Patch"),
                "Patch  1 file · +1 −1",
            ),
            (
                "view_image",
                json!({"path": "/tmp/image.png", "detail": "original"}),
                "Image  /tmp/image.png · original",
            ),
            (
                "image_gen__imagegen",
                json!({"prompt": "a compact terminal"}),
                "Image generation  a compact terminal",
            ),
            ("wait", json!({"cell_id": "12"}), "Wait  background work"),
            (
                "mcp__files__read",
                json!({"path": "/tmp/file"}),
                "Files · read  /tmp/file",
            ),
            (
                "spawn_agent",
                json!({"role": "reviewer"}),
                "Spawned  reviewer",
            ),
        ];

        for (name, arguments, expected) in cases {
            let rendered = render(&tool(name, arguments), 100, &Theme::default())[0].to_string();
            assert!(rendered.contains(expected), "{name}: {rendered}");
        }
    }

    #[test]
    fn patch_summary_colors_additions_green_and_deletions_red() {
        let patch = tool(
            "apply_patch",
            json!("*** Begin Patch\n*** Update File: src/main.rs\n+new\n-old\n*** End Patch"),
        );

        let lines = render(&patch, 100, &Theme::default());
        let additions = lines[0]
            .spans
            .iter()
            .find(|span| span.content == "+1")
            .expect("patch summary should include additions");
        let deletions = lines[0]
            .spans
            .iter()
            .find(|span| span.content == "−1")
            .expect("patch summary should include deletions");

        assert_eq!(additions.style.fg, Some(Color::Green));
        assert_eq!(deletions.style.fg, Some(Color::Red));
    }

    #[test]
    fn expanded_patch_colors_diff_lines() {
        let patch = tool(
            "apply_patch",
            json!("*** Begin Patch\n*** Update File: src/main.rs\n+new\n-old\n*** End Patch"),
        );

        let lines = render_expanded(&patch, 80, &Theme::default());
        let addition = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == "+ ")
            .expect("addition should be rendered");
        let deletion = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == "- ")
            .expect("deletion should be rendered");

        assert_eq!(addition.style.fg, Some(Color::Green));
        assert_eq!(deletion.style.fg, Some(Color::Red));
    }

    #[test]
    fn expanded_patch_renders_each_hunk_with_its_file_and_context() {
        let patch = tool(
            "apply_patch",
            json!(
                "*** Begin Patch\n*** Update File: src/main.rs\n@@ fn main()\n-old();\n+new();\n*** End Patch"
            ),
        );

        let rendered = render_expanded(&patch, 80, &Theme::default())
            .iter()
            .map(ToString::to_string)
            .collect::<String>();

        assert!(rendered.contains("src/main.rs"));
        assert!(rendered.contains("fn main()"));
        assert!(rendered.contains("+1 −1"));
    }

    #[test]
    fn mixed_web_operations_are_summarized_by_count() {
        let web = tool(
            "web__run",
            json!({
                "search_query": [{"q": "one"}, {"q": "two"}],
                "open": [{"ref_id": "turn0search0"}],
                "weather": [{"location": "Amsterdam"}],
            }),
        );

        let rendered = render(&web, 100, &Theme::default())[0].to_string();

        assert!(rendered.contains("search 2 · open 1 · weather 1"));
    }

    #[test]
    fn expanded_web_results_hide_protocol_annotations() {
        let mut web = tool("web__run", json!({"open": [{"ref_id": "turn0search0"}]}));
        web.result = Some(json!(
            "citeturn0view0 Useful content [wordlim: 200]\nSecond line"
        ));

        let rendered = render_expanded(&web, 80, &Theme::default())
            .into_iter()
            .map(|line| line.to_string())
            .collect::<String>();

        assert!(rendered.contains("Useful content"));
        assert!(rendered.contains("Second line"));
        assert!(!rendered.contains("cite"));
        assert!(!rendered.contains("wordlim"));

        let source = render_layout(&web, None, 80, &Theme::default(), true)
            .selection_source
            .expect("expanded web results should be selectable");
        assert_eq!(source, " Useful content\nSecond line");
    }

    #[test]
    fn tool_rendering_never_exceeds_narrow_widths() {
        let mut shell = tool(
            "exec_command",
            json!({"cmd": "cargo test --all-targets --no-fail-fast"}),
        );
        shell.result = Some(json!({
            "output": "a very long output line that must wrap safely",
            "exit_code": 0,
        }));

        for width in 1..=12 {
            let collapsed = render(&shell, width, &Theme::default());
            assert!(!collapsed.is_empty());
            assert!(
                collapsed
                    .iter()
                    .all(|line| line.width() <= usize::from(width))
            );

            let expanded = render_expanded(&shell, width, &Theme::default());
            assert!(!expanded.is_empty());
            assert!(
                expanded
                    .iter()
                    .all(|line| line.width() <= usize::from(width))
            );
        }
    }

    #[test]
    fn summaries_name_the_execution_origin() {
        let cases = [
            (
                tool("exec_command", json!({"cmd": "pwd", "workdir": "/work"})),
                "Local",
            ),
            (
                tool("sandbox_exec", json!({"command": "pwd", "cwd": "/work"})),
                "Sandbox · /work",
            ),
            (tool("custom_operation", json!({})), "Local"),
            (
                tool("web__run", json!({"time": [{"utc_offset": "+00:00"}]})),
                "Web client",
            ),
            (
                tool(
                    "browser",
                    json!({"action": "open", "url": "https://example.com"}),
                ),
                "Browser",
            ),
            (
                tool("mcp__files__read", json!({"path": "/tmp/a"})),
                "MCP · files",
            ),
        ];

        for (tool, expected) in cases {
            let rendered = render(&tool, 120, &Theme::default())[0].to_string();
            assert!(rendered.contains(expected), "{rendered}");
        }
    }

    #[test]
    fn result_metadata_can_promote_a_call_to_a_named_machine() {
        let mut remote = tool("exec_command", json!({"cmd": "pwd"}));
        remote.metadata = Some(json!({"executor": {"machine_name": "Alice's Mac"}}));
        remote.infer_execution();

        let rendered = render(&remote, 100, &Theme::default())[0].to_string();

        assert!(rendered.contains("Alice's Mac"), "{rendered}");
        assert!(rendered.contains("Machine Alice's Mac"), "{rendered}");
        assert!(!rendered.contains("Sandbox"), "{rendered}");
    }

    #[test]
    fn expanded_generic_output_is_bounded() {
        let mut generic = tool("custom_operation", json!({"query": "bounded"}));
        generic.result = Some(json!({"rows": vec!["x".repeat(1_000); 1_000]}));

        let lines = render_expanded(&generic, 40, &Theme::default());
        let rendered = lines.iter().map(ToString::to_string).collect::<String>();

        assert!(lines.len() <= MAX_EXPANDED_DETAIL_LINES + 4);
        assert!(rendered.contains("expanded output truncated"));
    }

    #[test]
    fn subagent_summaries_keep_schema_and_full_task_out_of_collapsed_view() {
        let mut spawn = tool(
            "spawn_agent",
            json!({
                "role": "reviewer",
                "task": "Inspect the entire repository carefully and return a detailed report with evidence.",
                "output_schema": {"type": "object", "properties": {"report": {"type": "string"}}}
            }),
        );
        spawn.result = Some(json!({
            "agent_id": 72,
            "role": "reviewer",
            "status": {"state": "running"}
        }));

        let collapsed = render(&spawn, 140, &Theme::default())[0].to_string();
        let expanded = render_expanded(&spawn, 140, &Theme::default())
            .iter()
            .map(ToString::to_string)
            .collect::<String>();

        assert!(collapsed.contains("Spawned  reviewer"));
        assert!(collapsed.contains("agent 72 · running"));
        assert!(!collapsed.contains("output_schema"));
        assert!(expanded.contains("output_schema"));
        assert!(expanded.contains("Inspect the entire repository carefully"));
    }

    #[test]
    fn sandbox_families_render_commands_processes_previews_and_streams() {
        let mut exec = tool(
            "sandbox_exec",
            json!({"command": "printf hello", "cwd": "/workspace"}),
        );
        exec.result = Some(json!({
            "success": true,
            "exit_code": 0,
            "stdout": "hello\n",
            "stderr": ""
        }));
        let mut process = tool(
            "sandbox_start_process",
            json!({"command": "node", "args": ["server.mjs"], "ready_port": 8000}),
        );
        process.result = Some(json!({
            "process_id": "proc-1",
            "pid": 42,
            "status": "running",
            "ready_port": 8000
        }));
        let mut process_status = tool("sandbox_get_process", json!({"process_id": "proc-1"}));
        process_status.result = Some(json!({
            "found": true,
            "process_id": "proc-1",
            "command": "cargo test",
            "status": "failed",
            "terminal": true,
            "exit_code": 101,
            "stdout": "compiled\n",
            "stderr": "test failed\n"
        }));
        let mut stopped = tool("sandbox_kill_process", json!({"process_id": "proc-2"}));
        stopped.result = Some(json!({
            "found": true,
            "process_id": "proc-2",
            "status": "killed",
            "terminal": true,
            "kill_requested": true
        }));
        let mut preview = tool("preview", json!({"environment": "sandbox", "port": 8000}));
        preview.result = Some(json!({
            "port": 8000,
            "url": "https://preview.example.test",
            "persistent": false
        }));

        let exec_summary = render(&exec, 120, &Theme::default())[0].to_string();
        let exec_details = render_expanded(&exec, 120, &Theme::default())
            .iter()
            .map(ToString::to_string)
            .collect::<String>();
        let process_summary = render(&process, 120, &Theme::default())[0].to_string();
        let process_status_summary = render(&process_status, 120, &Theme::default())[0].to_string();
        let process_status_details = render_expanded(&process_status, 120, &Theme::default())
            .iter()
            .map(ToString::to_string)
            .collect::<String>();
        let stopped_summary = render(&stopped, 120, &Theme::default())[0].to_string();
        let preview_summary = render(&preview, 120, &Theme::default())[0].to_string();

        assert!(
            exec_summary.contains("Run command  $ printf hello"),
            "{exec_summary}"
        );
        assert!(
            exec_summary.contains("Sandbox · /workspace"),
            "{exec_summary}"
        );
        assert!(exec_summary.contains("exit 0"), "{exec_summary}");
        for expected in [
            "cwd /workspace",
            "command",
            "printf hello",
            "stdout",
            "hello",
            "stderr",
            "(empty)",
        ] {
            assert!(
                exec_details.contains(expected),
                "missing {expected:?}: {exec_details}"
            );
        }
        assert!(
            process_summary.contains("Start process  node server.mjs"),
            "{process_summary}"
        );
        assert!(
            process_summary.contains("PID 42 · running · port 8000 ready"),
            "{process_summary}"
        );
        assert!(
            process_status_summary.contains("Check process  proc-1"),
            "{process_status_summary}"
        );
        assert!(
            process_status_summary.contains("failed · exit 101"),
            "{process_status_summary}"
        );
        assert!(
            stopped_summary.contains("Stop process  proc-2")
                && stopped_summary.contains("killed")
                && stopped_summary.contains("Sandbox"),
            "{stopped_summary}"
        );
        for expected in ["cargo test", "compiled", "test failed"] {
            assert!(
                process_status_details.contains(expected),
                "missing {expected:?}: {process_status_details}"
            );
        }
        assert!(
            preview_summary.contains("Open preview  port 8000"),
            "{preview_summary}"
        );
        assert!(
            preview_summary.contains("preview ready"),
            "{preview_summary}"
        );
    }

    #[test]
    fn direct_shell_only_becomes_sandbox_when_metadata_says_so() {
        let direct = tool("exec_command", json!({"cmd": "pwd", "workdir": "/repo"}));
        let mut sandbox = direct.clone();
        sandbox.metadata = Some(json!({"execution": "sandbox"}));
        sandbox.infer_execution();

        let direct = render(&direct, 120, &Theme::default())[0].to_string();
        let sandbox = render(&sandbox, 120, &Theme::default())[0].to_string();

        assert!(direct.contains(" · Local · "), "{direct}");
        assert!(sandbox.contains("Sandbox · /repo"), "{sandbox}");
    }

    #[test]
    fn environment_addressed_execution_names_the_selected_hand() {
        let sandbox = tool(
            "exec_command",
            json!({"environment": "sandbox", "cmd": "pwd", "workdir": "/workspace"}),
        );
        let machine = tool(
            "write_stdin",
            json!({"environment": "user:build-box", "session_id": 7}),
        );

        let sandbox = render(&sandbox, 120, &Theme::default())[0].to_string();
        let machine = render(&machine, 120, &Theme::default())[0].to_string();

        assert!(sandbox.contains("Sandbox · /workspace"), "{sandbox}");
        assert!(machine.contains("Machine build-box"), "{machine}");
    }

    #[test]
    fn machine_qualified_capabilities_keep_their_execution_origin() {
        let image = tool(
            "user_machine-a_view_image",
            json!({"path": "/repo/result.png", "detail": "original"}),
        );
        let mcp = tool(
            "user_machine-a_mcp__linear__search_issues",
            json!({"query": "renderer"}),
        );

        let image = render(&image, 120, &Theme::default())[0].to_string();
        let mcp = render(&mcp, 120, &Theme::default())[0].to_string();

        assert!(
            image.contains("Image  /repo/result.png · original"),
            "{image}"
        );
        assert!(image.contains("Machine machine-a"), "{image}");
        assert!(mcp.contains("Linear · search issues"), "{mcp}");
        assert!(mcp.contains("Machine machine-a"), "{mcp}");
    }

    #[test]
    fn mcp_namespace_precedes_shell_family_and_wrappers_keep_wire_operation() {
        let shell = tool(
            "mcp__remote_host__exec_command",
            json!({"cmd": "pwd", "workdir": "/repo"}),
        );
        let wrapper = tool(
            "mcp__centaur__call_read_tool",
            json!({"name": "search_issues", "arguments": {"query": "renderer"}}),
        );

        let shell = render(&shell, 120, &Theme::default())[0].to_string();
        let wrapper = render(&wrapper, 120, &Theme::default())[0].to_string();

        assert!(shell.contains("Remote host · exec command"), "{shell}");
        assert!(shell.contains("MCP · remote_host"), "{shell}");
        assert!(!shell.contains("Shell  $"), "{shell}");
        assert!(wrapper.contains("Centaur · call read tool"), "{wrapper}");
        assert!(wrapper.contains("search_issues · renderer"), "{wrapper}");
    }

    #[test]
    fn generic_sections_bound_serialization_and_reserve_result_space() {
        let huge = json!({"rows": vec!["x".repeat(1_000); 1_000]});
        let serialized = bounded_json(&huge);
        assert!(serialized.len() <= MAX_EXPANDED_TEXT_BYTES + 64);
        assert!(serialized.contains("output truncated"));

        let mut generic = tool("custom_operation", huge);
        generic.result = Some(json!({"proof": "RESULT_SENTINEL"}));
        let rendered = render_expanded(&generic, 40, &Theme::default())
            .iter()
            .map(ToString::to_string)
            .collect::<String>();

        assert!(rendered.contains("section truncated"), "{rendered}");
        assert!(rendered.contains("RESULT_SENTINEL"), "{rendered}");
    }

    #[test]
    fn browser_target_objects_have_concise_semantic_subjects() {
        let browser = tool(
            "browser",
            json!({
                "action": "click",
                "target": {
                    "by": "role",
                    "role": "button",
                    "name": "Submit",
                    "exact": true,
                    "index": {"kind": "nth", "index": 2}
                }
            }),
        );

        let rendered = render(&browser, 120, &Theme::default())[0].to_string();

        assert!(
            rendered.contains("role button \"Submit\" · nth 2"),
            "{rendered}"
        );
        assert!(!rendered.contains("exact"), "{rendered}");
    }
}
