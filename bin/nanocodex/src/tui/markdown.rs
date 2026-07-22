use std::{borrow::Cow, sync::OnceLock};

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, Theme, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) fn render_agent_markdown(source: &str, width: u16) -> Text<'static> {
    let mut writer = MarkdownWriter::new(width);
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    for event in Parser::new_ext(source, options) {
        writer.event(event);
    }
    writer.finish()
}

pub(super) fn heal_streaming_markdown(source: &str) -> Cow<'_, str> {
    let mut suffix = String::new();
    if let Some(fence) = open_fence(source) {
        if !source.ends_with('\n') {
            suffix.push('\n');
        }
        suffix.extend(std::iter::repeat_n(fence.marker, fence.length));
    } else {
        heal_inline_markers(source, &mut suffix);
        heal_link(source, &mut suffix);
    }
    if suffix.is_empty() {
        Cow::Borrowed(source)
    } else {
        Cow::Owned(format!("{source}{suffix}"))
    }
}

pub(super) fn highlighted_code_lines(language: Option<&str>, source: &str) -> Vec<Line<'static>> {
    let Some(language) = language.and_then(normalize_language) else {
        return plain_code_lines(source);
    };
    let assets = highlight_assets();
    let Some(syntax) = assets
        .syntaxes
        .find_syntax_by_token(language)
        .or_else(|| assets.syntaxes.find_syntax_by_extension(language))
    else {
        return plain_code_lines(source);
    };
    if syntax.name == "Plain Text" && !matches!(language, "text" | "txt" | "plain" | "plaintext") {
        return plain_code_lines(source);
    }
    let mut highlighter = HighlightLines::new(syntax, &assets.theme);
    let mut output = Vec::new();
    for line in LinesWithEndings::from(source) {
        let Ok(regions) = highlighter.highlight_line(line, &assets.syntaxes) else {
            return plain_code_lines(source);
        };
        let spans = regions
            .into_iter()
            .filter_map(|(style, text)| {
                let text = text.trim_end_matches(['\r', '\n']);
                (!text.is_empty()).then(|| {
                    Span::styled(
                        text.to_owned(),
                        Style::default()
                            .fg(Color::Rgb(
                                style.foreground.r,
                                style.foreground.g,
                                style.foreground.b,
                            ))
                            .add_modifier(font_modifiers(style.font_style)),
                    )
                })
            })
            .collect::<Vec<_>>();
        output.push(Line::from(spans));
    }
    if output.is_empty() {
        output.push(Line::raw(""));
    }
    output
}

struct HighlightAssets {
    syntaxes: SyntaxSet,
    theme: Theme,
}

#[derive(Clone, Copy)]
struct OpenFence {
    marker: char,
    length: usize,
}

fn highlight_assets() -> &'static HighlightAssets {
    static ASSETS: OnceLock<HighlightAssets> = OnceLock::new();
    ASSETS.get_or_init(|| {
        let themes = ThemeSet::load_defaults();
        let theme = themes
            .themes
            .get("base16-ocean.dark")
            .cloned()
            .or_else(|| themes.themes.values().next().cloned())
            .unwrap_or_default();
        HighlightAssets {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            theme,
        }
    })
}

fn normalize_language(language: &str) -> Option<&str> {
    language
        .split(|character: char| character.is_whitespace() || character == ',')
        .next()
        .map(|language| language.trim_matches(['{', '}', '.']))
        .filter(|language| !language.is_empty())
}

fn font_modifiers(style: FontStyle) -> Modifier {
    let mut modifiers = Modifier::empty();
    if style.contains(FontStyle::BOLD) {
        modifiers.insert(Modifier::BOLD);
    }
    if style.contains(FontStyle::ITALIC) {
        modifiers.insert(Modifier::ITALIC);
    }
    if style.contains(FontStyle::UNDERLINE) {
        modifiers.insert(Modifier::UNDERLINED);
    }
    modifiers
}

fn plain_code_lines(source: &str) -> Vec<Line<'static>> {
    let mut lines = source
        .trim_end_matches('\n')
        .split('\n')
        .map(|line| {
            Line::from(Span::styled(
                line.to_owned(),
                Style::default().fg(Color::Yellow),
            ))
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(Line::raw(""));
    }
    lines
}

fn open_fence(source: &str) -> Option<OpenFence> {
    let mut open = None;
    for line in source.lines() {
        let trimmed = line.trim_start_matches(' ');
        if line.len().saturating_sub(trimmed.len()) > 3 {
            continue;
        }
        let Some(marker) = trimmed
            .chars()
            .next()
            .filter(|marker| matches!(marker, '`' | '~'))
        else {
            continue;
        };
        let length = trimmed
            .chars()
            .take_while(|character| *character == marker)
            .count();
        if length < 3 {
            continue;
        }
        match open {
            Some(OpenFence {
                marker: open_marker,
                length: open_length,
            }) if marker == open_marker
                && length >= open_length
                && trimmed[length..].trim().is_empty() =>
            {
                open = None;
            }
            None => open = Some(OpenFence { marker, length }),
            _ => {}
        }
    }
    open
}

fn heal_link(source: &str, suffix: &mut String) {
    let tail = source.rsplit_once('\n').map_or(source, |(_, tail)| tail);
    if let Some(open) = tail.rfind("](")
        && !tail[open + 2..].contains(')')
    {
        suffix.push(')');
        return;
    }
    if let Some(open) = tail.rfind('[')
        && !tail[open + 1..].contains(']')
        && !tail[..open].ends_with('!')
    {
        suffix.push_str("](streaming:incomplete)");
    }
}

fn heal_inline_markers(source: &str, suffix: &mut String) {
    let markers = marker_counts(source);
    if markers.inline_code % 2 == 1 {
        suffix.push('`');
    }
    if markers.bold_asterisk % 2 == 1 {
        suffix.push_str("**");
    }
    if markers.bold_underscore % 2 == 1 {
        suffix.push_str("__");
    }
    if markers.italic_asterisk % 2 == 1 {
        suffix.push('*');
    }
    if markers.italic_underscore % 2 == 1 {
        suffix.push('_');
    }
    if markers.strikethrough % 2 == 1 {
        suffix.push_str("~~");
    }
}

#[derive(Default)]
struct MarkerCounts {
    inline_code: usize,
    bold_asterisk: usize,
    bold_underscore: usize,
    italic_asterisk: usize,
    italic_underscore: usize,
    strikethrough: usize,
}

fn marker_counts(source: &str) -> MarkerCounts {
    let mut counts = MarkerCounts::default();
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut fence: Option<(u8, usize)> = None;
    let mut inline_code = false;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index = index.saturating_add(2);
            continue;
        }
        if let Some((marker, length)) = fence_marker_at_line_start(bytes, index) {
            match fence {
                Some((open_marker, open_length))
                    if marker == open_marker && length >= open_length =>
                {
                    fence = None;
                }
                None => fence = Some((marker, length)),
                _ => {}
            }
            index = index.saturating_add(length);
            continue;
        }
        if fence.is_some() {
            index = index.saturating_add(1);
            continue;
        }
        let run = bytes[index..]
            .iter()
            .take_while(|byte| **byte == bytes[index])
            .count();
        if bytes[index] == b'`' && run < 3 {
            counts.inline_code = counts.inline_code.saturating_add(1);
            inline_code = !inline_code;
            index = index.saturating_add(run);
            continue;
        }
        if inline_code {
            index = index.saturating_add(run.max(1));
            continue;
        }
        match bytes[index] {
            b'*' if run >= 2 && marker_is_delimiter(bytes, index, run) => {
                counts.bold_asterisk = counts.bold_asterisk.saturating_add(run / 2);
                counts.italic_asterisk = counts.italic_asterisk.saturating_add(run % 2);
            }
            b'*' if marker_is_delimiter(bytes, index, run) => {
                counts.italic_asterisk = counts.italic_asterisk.saturating_add(1);
            }
            b'_' if run >= 2 && marker_is_delimiter(bytes, index, run) => {
                counts.bold_underscore = counts.bold_underscore.saturating_add(run / 2);
                counts.italic_underscore = counts.italic_underscore.saturating_add(run % 2);
            }
            b'_' if marker_is_delimiter(bytes, index, run) => {
                counts.italic_underscore = counts.italic_underscore.saturating_add(1);
            }
            b'~' if run >= 2 => {
                counts.strikethrough = counts.strikethrough.saturating_add(run / 2);
            }
            _ => {}
        }
        index = index.saturating_add(run.max(1));
    }
    counts
}

fn fence_marker_at_line_start(bytes: &[u8], index: usize) -> Option<(u8, usize)> {
    if !matches!(bytes.get(index), Some(b'`' | b'~')) {
        return None;
    }
    let line_start = bytes[..index]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position + 1);
    if index.saturating_sub(line_start) > 3
        || bytes[line_start..index].iter().any(|byte| *byte != b' ')
    {
        return None;
    }
    let marker = bytes[index];
    let length = bytes[index..]
        .iter()
        .take_while(|byte| **byte == marker)
        .count();
    (length >= 3).then_some((marker, length))
}

fn marker_is_delimiter(bytes: &[u8], index: usize, run: usize) -> bool {
    let before = index.checked_sub(1).and_then(|index| bytes.get(index));
    let after = bytes.get(index.saturating_add(run));
    if before.is_some_and(u8::is_ascii_alphanumeric) && after.is_some_and(u8::is_ascii_alphanumeric)
    {
        return false;
    }
    let before_flanking = before.is_some_and(|byte| !byte.is_ascii_whitespace());
    let after_flanking = after.is_some_and(|byte| !byte.is_ascii_whitespace());
    before_flanking || after_flanking
}

struct MarkdownWriter {
    width: u16,
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    styles: Vec<Style>,
    lists: Vec<ListState>,
    pending_item_prefix: Option<String>,
    quote_depth: usize,
    code_block: Option<CodeBlock>,
    table: Option<TableState>,
}

struct ListState {
    next: Option<u64>,
}

struct CodeBlock {
    language: Option<String>,
    source: String,
}

#[derive(Default)]
struct TableState {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    in_header: bool,
}

impl MarkdownWriter {
    fn new(width: u16) -> Self {
        Self {
            width,
            lines: vec![Line::styled(
                "● Nanocodex",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )],
            current: Vec::new(),
            styles: vec![Style::default().fg(Color::White)],
            lists: Vec::new(),
            pending_item_prefix: None,
            quote_depth: 0,
            code_block: None,
            table: None,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn event(&mut self, event: Event<'_>) {
        if self.table.is_some() && self.table_event(&event) {
            return;
        }
        if let Some(code) = &mut self.code_block {
            match event {
                Event::Text(text) | Event::Code(text) => code.source.push_str(&text),
                Event::SoftBreak | Event::HardBreak => code.source.push('\n'),
                Event::End(TagEnd::CodeBlock) => self.end_code_block(),
                _ => {}
            }
            return;
        }

        match event {
            Event::End(TagEnd::Paragraph) => {
                self.flush_current();
                self.blank_line();
            }
            Event::Start(Tag::Heading { .. }) => {
                self.flush_current();
                self.push_style(
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                );
            }
            Event::End(TagEnd::Heading(_)) => {
                self.flush_current();
                self.pop_style();
                self.blank_line();
            }
            Event::Start(Tag::BlockQuote) => self.quote_depth = self.quote_depth.saturating_add(1),
            Event::End(TagEnd::BlockQuote) => {
                self.flush_current();
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.blank_line();
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                self.flush_current();
                let language = match kind {
                    CodeBlockKind::Fenced(language) if !language.is_empty() => {
                        Some(language.into_string())
                    }
                    CodeBlockKind::Fenced(_) | CodeBlockKind::Indented => None,
                };
                self.code_block = Some(CodeBlock {
                    language,
                    source: String::new(),
                });
            }
            Event::Start(Tag::List(next)) => self.lists.push(ListState { next }),
            Event::End(TagEnd::List(_)) => {
                self.flush_current();
                let _ = self.lists.pop();
                self.blank_line();
            }
            Event::Start(Tag::Item) => {
                self.flush_current();
                let prefix = self.lists.last_mut().map_or_else(
                    || "• ".to_owned(),
                    |list| match &mut list.next {
                        Some(next) => {
                            let prefix = format!("{next}. ");
                            *next = next.saturating_add(1);
                            prefix
                        }
                        None => "• ".to_owned(),
                    },
                );
                self.pending_item_prefix = Some(prefix);
            }
            Event::End(TagEnd::Item) | Event::SoftBreak | Event::HardBreak => {
                self.flush_current();
            }
            Event::Start(Tag::Emphasis) => {
                self.push_style(Style::default().add_modifier(Modifier::ITALIC));
            }
            Event::End(
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link,
            ) => self.pop_style(),
            Event::Start(Tag::Strong) => {
                self.push_style(Style::default().add_modifier(Modifier::BOLD));
            }
            Event::Start(Tag::Strikethrough) => {
                self.push_style(Style::default().add_modifier(Modifier::CROSSED_OUT));
            }
            Event::Start(Tag::Link { .. }) => self.push_style(
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::UNDERLINED),
            ),
            Event::Start(Tag::Image { .. }) => self.append_text("image: "),
            Event::Text(text) => self.append_text(&text),
            Event::Code(code) => {
                let style = self.current_style().patch(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM),
                );
                self.ensure_prefix();
                self.current.push(Span::styled(code.into_string(), style));
            }
            Event::Rule => {
                self.flush_current();
                self.lines.push(Line::styled(
                    format!(
                        "  {}",
                        "─".repeat(usize::from(self.width.saturating_sub(4).min(36)))
                    ),
                    Style::default().fg(Color::DarkGray),
                ));
                self.blank_line();
            }
            Event::TaskListMarker(checked) => {
                self.append_text(if checked { "[✓] " } else { "[ ] " });
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                let plain = strip_html(&html);
                if !plain.is_empty() {
                    self.append_text(&plain);
                }
            }
            Event::FootnoteReference(label) => self.append_text(&format!("[{label}]")),
            Event::Start(Tag::Table(_)) => self.table = Some(TableState::default()),
            Event::Start(
                Tag::Paragraph
                | Tag::HtmlBlock
                | Tag::FootnoteDefinition(_)
                | Tag::MetadataBlock(_)
                | Tag::TableHead
                | Tag::TableRow
                | Tag::TableCell,
            )
            | Event::End(
                TagEnd::Image
                | TagEnd::CodeBlock
                | TagEnd::HtmlBlock
                | TagEnd::FootnoteDefinition
                | TagEnd::MetadataBlock(_)
                | TagEnd::Table
                | TagEnd::TableHead
                | TagEnd::TableRow
                | TagEnd::TableCell,
            ) => {}
        }
    }

    fn table_event(&mut self, event: &Event<'_>) -> bool {
        let Some(table) = &mut self.table else {
            return false;
        };
        match event {
            Event::Start(Tag::TableHead) => table.in_header = true,
            Event::End(TagEnd::TableHead) => {
                finish_table_row(table);
                table.in_header = false;
            }
            Event::Start(Tag::TableRow) => table.current_row.clear(),
            Event::End(TagEnd::TableRow) => finish_table_row(table),
            Event::Start(Tag::TableCell) => table.current_cell.clear(),
            Event::End(TagEnd::TableCell) => {
                table.current_row.push(compact_cell(&table.current_cell));
                table.current_cell.clear();
            }
            Event::Text(text) | Event::Code(text) => table.current_cell.push_str(text),
            Event::SoftBreak | Event::HardBreak => table.current_cell.push(' '),
            Event::End(TagEnd::Table) => {
                let mut table = self.table.take().unwrap_or_default();
                finish_table_row(&mut table);
                self.render_table(table);
            }
            _ => {}
        }
        true
    }

    fn append_text(&mut self, text: &str) {
        let mut parts = text.split('\n').peekable();
        while let Some(part) = parts.next() {
            if !part.is_empty() {
                self.ensure_prefix();
                self.current
                    .push(Span::styled(part.to_owned(), self.current_style()));
            }
            if parts.peek().is_some() {
                self.flush_current();
            }
        }
    }

    fn ensure_prefix(&mut self) {
        if !self.current.is_empty() {
            return;
        }
        self.current.push(Span::raw("  "));
        if self.quote_depth > 0 {
            self.current.push(Span::styled(
                "│ ".repeat(self.quote_depth),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if let Some(prefix) = self.pending_item_prefix.take() {
            let indent = "  ".repeat(self.lists.len().saturating_sub(1));
            self.current.push(Span::raw(indent));
            self.current
                .push(Span::styled(prefix, Style::default().fg(Color::Green)));
        }
    }

    fn flush_current(&mut self) {
        if self.current.is_empty() {
            return;
        }
        self.lines
            .push(Line::from(std::mem::take(&mut self.current)));
    }

    fn blank_line(&mut self) {
        if self
            .lines
            .last()
            .is_some_and(|line| line.spans.iter().all(|span| span.content.is_empty()))
        {
            return;
        }
        self.lines.push(Line::raw(""));
    }

    fn push_style(&mut self, style: Style) {
        let next = self.current_style().patch(style);
        self.styles.push(next);
    }

    fn pop_style(&mut self) {
        if self.styles.len() > 1 {
            let _ = self.styles.pop();
        }
    }

    fn current_style(&self) -> Style {
        self.styles.last().copied().unwrap_or_default()
    }

    fn end_code_block(&mut self) {
        let Some(code) = self.code_block.take() else {
            return;
        };
        let title = code.language.as_deref().unwrap_or("code");
        self.lines.push(Line::styled(
            format!("  ┌─ {title}"),
            Style::default().fg(Color::DarkGray),
        ));
        for line in highlighted_code_lines(code.language.as_deref(), &code.source) {
            let mut spans = vec![Span::styled("  │ ", Style::default().fg(Color::DarkGray))];
            spans.extend(line.spans);
            self.lines.push(Line::from(spans));
        }
        self.lines
            .push(Line::styled("  └─", Style::default().fg(Color::DarkGray)));
        self.blank_line();
    }

    fn render_table(&mut self, mut table: TableState) {
        self.flush_current();
        let columns = table
            .rows
            .iter()
            .map(Vec::len)
            .chain(std::iter::once(table.header.len()))
            .max()
            .unwrap_or(0);
        if columns == 0 {
            return;
        }
        table.header.resize(columns, String::new());
        for row in &mut table.rows {
            row.resize(columns, String::new());
        }
        let widths = (0..columns)
            .map(|column| {
                table
                    .rows
                    .iter()
                    .map(|row| UnicodeWidthStr::width(row[column].as_str()))
                    .chain(std::iter::once(UnicodeWidthStr::width(
                        table.header[column].as_str(),
                    )))
                    .max()
                    .unwrap_or(0)
            })
            .collect::<Vec<_>>();
        let required = widths
            .iter()
            .sum::<usize>()
            .saturating_add(columns.saturating_sub(1) * 3)
            .saturating_add(2);
        if required <= usize::from(self.width.max(1)) {
            self.render_wide_table(&table, &widths);
        } else {
            self.render_table_cards(&table);
        }
        self.blank_line();
    }

    fn render_wide_table(&mut self, table: &TableState, widths: &[usize]) {
        self.lines.push(table_line(
            &table.header,
            widths,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        let divider = widths
            .iter()
            .map(|width| "─".repeat(*width))
            .collect::<Vec<_>>()
            .join("─┼─");
        self.lines.push(Line::styled(
            format!("  {divider}"),
            Style::default().fg(Color::DarkGray),
        ));
        for row in &table.rows {
            self.lines
                .push(table_line(row, widths, Style::default().fg(Color::White)));
        }
    }

    fn render_table_cards(&mut self, table: &TableState) {
        for (row_index, row) in table.rows.iter().enumerate() {
            self.lines.push(Line::styled(
                format!("  ┌─ row {}", row_index + 1),
                Style::default().fg(Color::DarkGray),
            ));
            for (column, value) in row.iter().enumerate() {
                let label = table.header.get(column).map_or("", String::as_str);
                let label = if label.is_empty() {
                    format!("column {}", column + 1)
                } else {
                    label.to_owned()
                };
                let label_width = UnicodeWidthStr::width(label.as_str()).saturating_add(6);
                if label_width.saturating_add(UnicodeWidthStr::width(value.as_str()))
                    <= usize::from(self.width)
                {
                    self.lines.push(table_card_field(&label, value));
                } else {
                    self.lines.push(Line::from(vec![
                        Span::styled("  │ ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            format!("{label}:"),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    let value_width = usize::from(self.width.saturating_sub(6).max(1));
                    for line in hard_wrap(value, value_width) {
                        self.lines.push(Line::from(vec![
                            Span::styled("  │   ", Style::default().fg(Color::DarkGray)),
                            Span::styled(line, Style::default().fg(Color::White)),
                        ]));
                    }
                }
            }
            self.lines
                .push(Line::styled("  └─", Style::default().fg(Color::DarkGray)));
        }
    }

    fn finish(mut self) -> Text<'static> {
        self.flush_current();
        while self
            .lines
            .last()
            .is_some_and(|line| line.spans.iter().all(|span| span.content.is_empty()))
        {
            let _ = self.lines.pop();
        }
        self.lines.push(Line::raw(""));
        Text::from(self.lines)
    }
}

fn table_card_field(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("  │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{label}: "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value.to_owned(), Style::default().fg(Color::White)),
    ])
}

fn hard_wrap(value: &str, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut width = 0_usize;
    for grapheme in UnicodeSegmentation::graphemes(value, true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if !current.is_empty() && width.saturating_add(grapheme_width) > max_width {
            lines.push(std::mem::take(&mut current));
            width = 0;
        }
        current.push_str(grapheme);
        width = width.saturating_add(grapheme_width);
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

fn finish_table_row(table: &mut TableState) {
    if table.current_row.is_empty() {
        return;
    }
    let row = std::mem::take(&mut table.current_row);
    if table.in_header && table.header.is_empty() {
        table.header = row;
    } else {
        table.rows.push(row);
    }
}

fn table_line(cells: &[String], widths: &[usize], style: Style) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
        }
        let padding = widths[index].saturating_sub(UnicodeWidthStr::width(cell.as_str()));
        spans.push(Span::styled(
            format!("{cell}{}", " ".repeat(padding)),
            style,
        ));
    }
    Line::from(spans)
}

fn compact_cell(cell: &str) -> String {
    cell.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_html(html: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    for character in html.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use ratatui::{Terminal, backend::TestBackend, widgets::Paragraph};

    use super::{heal_streaming_markdown, highlighted_code_lines, render_agent_markdown};

    fn render(markdown: &str, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new(render_agent_markdown(markdown, width)),
                    frame.area(),
                );
            })
            .unwrap();
        terminal.backend().to_string()
    }

    #[test]
    fn renders_common_markdown_without_losing_content() {
        let rendered = render(
            "## Result\n\nUse **bold**, *emphasis*, and `cargo test`.\n\n- first\n- second\n\n```rust\nfn main() {}\n```",
            60,
            14,
        );
        assert!(rendered.contains("Result"));
        assert!(rendered.contains("Use bold, emphasis, and cargo test."));
        assert!(rendered.contains("• first"));
        assert!(rendered.contains("┌─ rust"));
        assert!(rendered.contains("fn main() {}"));
    }

    #[test]
    fn wide_tables_keep_columns_and_narrow_tables_become_cards() {
        let markdown = "| Name | Result |\n| --- | --- |\n| alpha | passed |\n| beta | failed |";
        let wide = render(markdown, 50, 8);
        assert!(wide.contains("Name  │ Result"));
        assert!(wide.contains("alpha │ passed"));

        let narrow = render(markdown, 15, 20);
        assert!(narrow.contains("┌─ row 1"));
        assert!(narrow.contains("Name: alpha"));
        assert!(narrow.contains("Result:"));
        assert!(narrow.contains("passed"), "{narrow}");
    }

    #[test]
    fn heals_incomplete_streaming_constructs_without_changing_plain_text() {
        assert_eq!(
            heal_streaming_markdown("plain snake_case 20 * 30"),
            "plain snake_case 20 * 30"
        );
        assert_eq!(
            heal_streaming_markdown("This is **bold"),
            "This is **bold**"
        );
        assert_eq!(
            heal_streaming_markdown("This is *italic"),
            "This is *italic*"
        );
        assert_eq!(
            heal_streaming_markdown("This is __bold"),
            "This is __bold__"
        );
        assert_eq!(
            heal_streaming_markdown("This is ~~gone"),
            "This is ~~gone~~"
        );
        assert_eq!(
            heal_streaming_markdown("Use `cargo test"),
            "Use `cargo test`"
        );
        assert_eq!(
            heal_streaming_markdown("[Read **this"),
            "[Read **this**](streaming:incomplete)"
        );
        assert_eq!(
            heal_streaming_markdown("```rust\nfn main() {}"),
            "```rust\nfn main() {}\n```"
        );
        assert_eq!(
            heal_streaming_markdown("```text\nliteral ** marker\n```"),
            "```text\nliteral ** marker\n```"
        );
    }

    #[test]
    fn highlights_known_fences_and_falls_back_for_unknown_languages() {
        let highlighted = highlighted_code_lines(Some("rust"), "fn main() { let value = 42; }");
        let source = highlighted
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let colors = highlighted
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter_map(|span| span.style.fg)
            .collect::<HashSet<_>>();
        assert_eq!(source, "fn main() { let value = 42; }");
        assert!(
            colors.len() > 1,
            "Rust source should use multiple syntax colors"
        );

        let fallback = highlighted_code_lines(Some("not-a-real-language"), "opaque code");
        assert_eq!(fallback[0].spans[0].content, "opaque code");
        assert_eq!(
            fallback[0].spans[0].style.fg,
            Some(ratatui::style::Color::Yellow)
        );
    }
}
