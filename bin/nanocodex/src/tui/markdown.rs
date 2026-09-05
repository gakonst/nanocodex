use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatex::{Formula, FormulaState, Ratatex};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};
use std::{
    borrow::Cow,
    fmt::Write as _,
    ops::Range,
    sync::{Arc, OnceLock},
};
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, Theme, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_HIGHLIGHT_LINE_BYTES: usize = 4 * 1024;

#[cfg(test)]
pub(super) fn render_agent_markdown(source: &str, width: u16) -> Text<'static> {
    render_agent_markdown_layout(source, width).text
}

pub(super) fn render_agent_markdown_layout(source: &str, width: u16) -> RenderedAgentMarkdown {
    render_agent_markdown_inner(source, width, None, &[], MathFallback::Source)
}

#[cfg(test)]
pub(super) fn render_agent_markdown_with_math(
    source: &str,
    width: u16,
    renderer: &Ratatex,
) -> RenderedAgentMarkdown {
    render_agent_markdown_inner(source, width, Some(renderer), &[], MathFallback::Pending)
}

pub(super) fn render_finalized_agent_markdown_with_math(
    source: &str,
    width: u16,
    renderer: &Ratatex,
    previous: &[StreamingFormulaFrame],
) -> RenderedAgentMarkdown {
    render_agent_markdown_inner(
        source,
        width,
        Some(renderer),
        previous,
        MathFallback::Pending,
    )
}

pub(super) fn render_streaming_agent_markdown_with_math(
    source: &str,
    width: u16,
    renderer: &Ratatex,
    previous: &[StreamingFormulaFrame],
) -> RenderedAgentMarkdown {
    render_agent_markdown_inner(
        source,
        width,
        Some(renderer),
        previous,
        MathFallback::PendingOrFailed,
    )
}

fn render_agent_markdown_inner(
    source: &str,
    width: u16,
    renderer: Option<&Ratatex>,
    previous: &[StreamingFormulaFrame],
    math_fallback: MathFallback,
) -> RenderedAgentMarkdown {
    let (prepared, formulas) = renderer.map_or_else(
        || (Cow::Borrowed(source), Vec::new()),
        |_| prepare_math(source),
    );
    let mut writer = MarkdownWriter::new(width, &formulas, renderer, previous, math_fallback);
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    for event in Parser::new_ext(&prepared, options) {
        writer.event(event);
    }
    writer.finish()
}

#[derive(Clone, Copy)]
enum MathFallback {
    Source,
    Pending,
    PendingOrFailed,
}

impl MathFallback {
    const fn hides_pending(self) -> bool {
        !matches!(self, Self::Source)
    }

    const fn hides_failed(self) -> bool {
        matches!(self, Self::PendingOrFailed)
    }
}

pub(super) struct RenderedAgentMarkdown {
    pub(super) text: Text<'static>,
    pub(super) links: Vec<Vec<LinkSpan>>,
    pub(super) formulas: Vec<MarkdownFormula>,
    pub(super) formula_sources: Vec<Arc<str>>,
    pub(super) math_generation: Option<u64>,
}

#[derive(Clone)]
pub(super) struct LinkSpan {
    pub(super) destination: Arc<str>,
    pub(super) start: u16,
    pub(super) end: u16,
}

#[derive(Clone, Default)]
pub(super) struct StreamingFormulaFrame {
    pub(super) sources: Vec<Arc<str>>,
    pub(super) formula: Option<Arc<Formula>>,
}

#[derive(Clone)]
pub(super) struct MarkdownFormula {
    pub(super) index: usize,
    pub(super) formula: Arc<Formula>,
    pub(super) full_source: Arc<str>,
    pub(super) line: usize,
    pub(super) column: u16,
    pub(super) source_column: u16,
    pub(super) block: bool,
}

struct MarkdownMath {
    source: String,
    full_source: String,
    display: bool,
}

struct MathRegion<'a> {
    source: &'a str,
    full_source: &'a str,
    range: Range<usize>,
    display: bool,
}

fn prepare_math(source: &str) -> (Cow<'_, str>, Vec<MarkdownMath>) {
    let display = ratatex::display_math(source);
    let mut regions = display
        .iter()
        .map(|region| MathRegion {
            source: region.source(),
            full_source: region.full_source(),
            range: region.range(),
            display: true,
        })
        .collect::<Vec<_>>();
    regions.extend(inline_math(source, &display));
    regions.sort_unstable_by_key(|region| region.range.start);
    if regions.is_empty() {
        return (Cow::Borrowed(source), Vec::new());
    }
    let mut prepared = String::with_capacity(source.len());
    let mut formulas = Vec::with_capacity(regions.len());
    let mut cursor = 0;
    for region in regions {
        let range = region.range;
        prepared.push_str(&source[cursor..range.start]);
        let index = formulas.len();
        if region.display {
            let _ = write!(prepared, "\n\n<!--ratatex-display:{index}-->\n\n");
        } else {
            let _ = write!(prepared, "<!--ratatex-inline:{index}-->");
        }
        formulas.push(MarkdownMath {
            source: region.source.to_owned(),
            full_source: region.full_source.to_owned(),
            display: region.display,
        });
        cursor = range.end;
    }
    prepared.push_str(&source[cursor..]);
    (Cow::Owned(prepared), formulas)
}

fn inline_math<'a>(source: &'a str, display: &[ratatex::DisplayMath<'a>]) -> Vec<MathRegion<'a>> {
    let mut protected = display
        .iter()
        .map(ratatex::DisplayMath::range)
        .collect::<Vec<_>>();
    let mut code_block_start = None;
    for (event, range) in Parser::new(source).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => code_block_start = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = code_block_start.take() {
                    protected.push(start..range.end);
                }
            }
            Event::Code(_) => protected.push(range),
            _ => {}
        }
    }
    protected.sort_unstable_by_key(|range| range.start);

    let mut regions = Vec::new();
    let mut cursor = 0;
    let mut protected_index = 0;
    while cursor < source.len() {
        if let Some(range) = protected.get(protected_index) {
            if cursor >= range.end {
                protected_index += 1;
                continue;
            }
            if range.contains(&cursor) {
                cursor = range.end;
                continue;
            }
        }
        if source[cursor..].starts_with(r"\(")
            && !is_escaped_at(source, cursor)
            && let Some(end_start) = find_inline_math_end(source, cursor + 2)
        {
            let end = end_start + 2;
            let body = &source[cursor + 2..end_start];
            if !body.trim().is_empty() && !body.contains('\n') {
                regions.push(MathRegion {
                    source: body.trim(),
                    full_source: &source[cursor..end],
                    range: cursor..end,
                    display: false,
                });
                cursor = end;
                continue;
            }
        }
        cursor += source[cursor..].chars().next().map_or(1, char::len_utf8);
    }
    regions
}

fn find_inline_math_end(source: &str, mut cursor: usize) -> Option<usize> {
    while cursor < source.len() {
        if source[cursor..].starts_with(r"\)") && !is_escaped_at(source, cursor) {
            return Some(cursor);
        }
        cursor += source[cursor..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

fn is_escaped_at(source: &str, index: usize) -> bool {
    source[..index]
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count()
        % 2
        == 1
}

enum MathMarker {
    Display(usize),
    Inline(usize),
}

fn math_marker(html: &str) -> Option<MathMarker> {
    let marker = html.trim().strip_suffix("-->")?;
    if let Some(index) = marker.strip_prefix("<!--ratatex-display:") {
        return index.parse().ok().map(MathMarker::Display);
    }
    marker
        .strip_prefix("<!--ratatex-inline:")?
        .parse()
        .ok()
        .map(MathMarker::Inline)
}

#[cfg(test)]
pub(super) fn restore_markdown_links(selected: String, source: &str) -> String {
    restore_markdown_links_from_sources(selected, std::iter::once(source))
}

pub(super) fn restore_markdown_links_from_sources<'a>(
    mut selected: String,
    sources: impl IntoIterator<Item = &'a str>,
) -> String {
    let sources = sources.into_iter().collect::<Vec<_>>();
    restore_fenced_code(&mut selected, &sources);
    let logical = LogicalMarkdown::from_sources(sources.iter().copied());
    logical.copy_range(&selected).unwrap_or(selected)
}

#[derive(Clone, Default)]
pub(super) struct LogicalMarkdown {
    text: String,
    links: Vec<LogicalLink>,
}

#[derive(Clone)]
struct LogicalLink {
    start: usize,
    end: usize,
    destination: String,
}

impl LogicalMarkdown {
    pub(super) fn from_sources<'a>(sources: impl IntoIterator<Item = &'a str>) -> Self {
        let mut logical = Self::default();
        for source in sources {
            logical.ensure_newline();
            logical.append_source(source);
        }
        logical
    }

    fn append_source(&mut self, source: &str) {
        let mut active_links = Vec::<(usize, String)>::new();
        for event in Parser::new_ext(source, Options::all()) {
            match event {
                Event::Start(Tag::Link { dest_url, .. }) => {
                    active_links.push((self.text.len(), dest_url.into_string()));
                }
                Event::End(TagEnd::Link) => {
                    if let Some((start, destination)) = active_links.pop() {
                        self.links.push(LogicalLink {
                            start,
                            end: self.text.len(),
                            destination,
                        });
                    }
                }
                Event::Text(text) | Event::Code(text) => self.text.push_str(&text),
                Event::SoftBreak
                | Event::HardBreak
                | Event::End(
                    TagEnd::Paragraph
                    | TagEnd::Heading(_)
                    | TagEnd::Item
                    | TagEnd::CodeBlock
                    | TagEnd::TableCell
                    | TagEnd::TableRow,
                ) => self.ensure_newline(),
                Event::Html(html) | Event::InlineHtml(html) => {
                    self.text.push_str(&strip_html(&html));
                }
                _ => {}
            }
        }
    }

    fn ensure_newline(&mut self) {
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.text.push('\n');
        }
    }

    pub(super) fn copy_range(&self, selected: &str) -> Option<String> {
        self.copy_range_exact(selected).or_else(|| {
            let stripped = strip_rendered_markdown_chrome(selected)?;
            self.copy_range_exact(&stripped)
        })
    }

    fn copy_range_exact(&self, selected: &str) -> Option<String> {
        let selected_key = compact_whitespace(selected);
        if selected_key.is_empty() {
            return None;
        }
        let (text_key, offsets) = compact_whitespace_with_offsets(&self.text);
        let mut matches = text_key.match_indices(&selected_key);
        let (normalized_start, _) = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        let normalized_end = normalized_start.saturating_add(selected_key.len());
        let start = offsets.iter().find_map(|(key_start, _, source_start, _)| {
            (*key_start == normalized_start).then_some(*source_start)
        })?;
        let end = offsets.iter().find_map(|(_, key_end, _, source_end)| {
            (*key_end == normalized_end).then_some(*source_end)
        })?;
        Some(self.markdown_range(start, end))
    }

    fn markdown_range(&self, start: usize, end: usize) -> String {
        let mut output = String::new();
        let mut cursor = start;
        for link in self
            .links
            .iter()
            .filter(|link| link.start < end && link.end > start)
        {
            let link_start = link.start.max(start);
            let link_end = link.end.min(end);
            output.push_str(&self.text[cursor..link_start]);
            let label = &self.text[link_start..link_end];
            if label == link.destination {
                output.push_str(label);
            } else {
                let _ = write!(output, "[{label}]({})", link.destination);
            }
            cursor = link_end;
        }
        output.push_str(&self.text[cursor..end]);
        output
    }
}

fn compact_whitespace(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn compact_whitespace_with_offsets(value: &str) -> (String, Vec<(usize, usize, usize, usize)>) {
    let mut compact = String::new();
    let mut offsets = Vec::new();
    for (source_start, character) in value.char_indices() {
        if character.is_whitespace() {
            continue;
        }
        let key_start = compact.len();
        compact.push(character);
        let key_end = compact.len();
        offsets.push((
            key_start,
            key_end,
            source_start,
            source_start.saturating_add(character.len_utf8()),
        ));
    }
    (compact, offsets)
}

fn restore_fenced_code(selected: &mut String, sources: &[&str]) {
    let blocks = sources
        .iter()
        .flat_map(|source| markdown_code_blocks(source))
        .collect::<Vec<_>>();
    for code in &blocks {
        while let Some(restored) = restore_framed_code(selected, code) {
            *selected = restored;
        }
    }
    let Some(without_gutters) = strip_code_gutters(selected) else {
        return;
    };
    for code in &blocks {
        let raw = code.source.trim_end_matches(['\r', '\n']);
        if raw.contains(&without_gutters) {
            *selected = without_gutters;
            return;
        }
        if raw.lines().collect::<String>() == without_gutters.lines().collect::<String>() {
            raw.clone_into(selected);
            return;
        }
    }
}

fn restore_framed_code(selected: &str, code: &CodeBlock) -> Option<String> {
    let title = code.language.as_deref().unwrap_or("code");
    let header = format!("┌─ {title} · {} LOC", code_line_count(&code.source));
    let lines = selected.split('\n').collect::<Vec<_>>();
    let start = lines.iter().position(|line| line.trim_start() == header)?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(index, line)| (line.trim_start() == "└─").then_some(index))?;
    let raw = code.source.trim_end_matches(['\r', '\n']);
    Some(
        lines[..start]
            .iter()
            .copied()
            .chain(std::iter::once(raw))
            .chain(lines[end + 1..].iter().copied())
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn strip_code_gutters(selected: &str) -> Option<String> {
    let mut stripped_any = false;
    let lines = selected
        .split('\n')
        .map(|line| {
            let trimmed = line.trim_start();
            let Some(body) = trimmed.strip_prefix('│') else {
                return line;
            };
            stripped_any = true;
            body.strip_prefix(' ').unwrap_or(body)
        })
        .collect::<Vec<_>>();
    stripped_any.then(|| lines.join("\n"))
}

fn strip_rendered_markdown_chrome(selected: &str) -> Option<String> {
    let mut stripped_any = false;
    let lines = selected
        .split('\n')
        .enumerate()
        .map(|(index, line)| {
            if index == 0 && line.trim_start_matches(' ') == "● Nanocodex" {
                stripped_any = true;
                return "";
            }
            let mut body = line.trim_start_matches(' ');
            let mut stripped_line = false;
            while let Some(rest) = body.strip_prefix('│') {
                stripped_any = true;
                stripped_line = true;
                body = rest.strip_prefix(' ').unwrap_or(rest);
            }
            if stripped_line { body } else { line }
        })
        .collect::<Vec<_>>();
    stripped_any.then(|| lines.join("\n"))
}

fn markdown_code_blocks(source: &str) -> Vec<CodeBlock> {
    let mut blocks = Vec::new();
    let mut active: Option<CodeBlock> = None;
    for event in Parser::new_ext(source, Options::all()) {
        if let Some(code) = &mut active {
            match event {
                Event::Text(text) | Event::Code(text) => code.source.push_str(&text),
                Event::SoftBreak | Event::HardBreak => code.source.push('\n'),
                Event::End(TagEnd::CodeBlock) => {
                    if let Some(code) = active.take() {
                        blocks.push(code);
                    }
                }
                _ => {}
            }
            continue;
        }
        if let Event::Start(Tag::CodeBlock(kind)) = event {
            let language = match kind {
                CodeBlockKind::Fenced(language) if !language.is_empty() => {
                    Some(language.into_string())
                }
                CodeBlockKind::Fenced(_) | CodeBlockKind::Indented => None,
            };
            active = Some(CodeBlock {
                language,
                source: String::new(),
            });
        }
    }
    blocks
}

pub(super) fn heal_streaming_markdown(source: &str) -> Cow<'_, str> {
    let source = ratatex::heal_streaming_display_math(source);
    let mut suffix = String::new();
    if let Some(fence) = open_fence(&source) {
        if !source.ends_with('\n') {
            suffix.push('\n');
        }
        suffix.extend(std::iter::repeat_n(fence.marker, fence.length));
    } else {
        heal_inline_markers(&source, &mut suffix);
        heal_link(&source, &mut suffix);
    }
    if suffix.is_empty() {
        source
    } else {
        Cow::Owned(format!("{source}{suffix}"))
    }
}

pub(super) fn highlighted_code_lines(language: Option<&str>, source: &str) -> Vec<Line<'static>> {
    if source
        .lines()
        .any(|line| line.len() > MAX_HIGHLIGHT_LINE_BYTES)
    {
        return plain_code_lines(source);
    }
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

pub(super) fn code_line_count(source: &str) -> usize {
    let source = source.trim_end_matches(['\r', '\n']);
    usize::from(!source.is_empty()) + source.bytes().filter(|byte| *byte == b'\n').count()
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
            syntaxes: two_face::syntax::extra_newlines(),
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

struct MarkdownWriter<'a> {
    width: u16,
    lines: Vec<Line<'static>>,
    current: Vec<TaggedSpan>,
    styles: Vec<Style>,
    lists: Vec<ListState>,
    pending_item_prefix: Option<String>,
    quote_depth: usize,
    code_block: Option<CodeBlock>,
    table: Option<TableState>,
    image: Option<MarkdownImage>,
    links: Vec<Arc<str>>,
    rendered_links: Vec<(usize, LinkSpan)>,
    formulas: &'a [MarkdownMath],
    renderer: Option<&'a Ratatex>,
    previous_formulas: &'a [StreamingFormulaFrame],
    math_fallback: MathFallback,
    rendered_formulas: Vec<MarkdownFormula>,
}

struct TaggedSpan {
    span: Span<'static>,
    link: Option<Arc<str>>,
}

struct ListState {
    next: Option<u64>,
}

struct CodeBlock {
    language: Option<String>,
    source: String,
}

struct MarkdownImage {
    title: String,
    alt: String,
}

#[derive(Default)]
struct TableState {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    in_header: bool,
}

impl<'a> MarkdownWriter<'a> {
    fn new(
        width: u16,
        formulas: &'a [MarkdownMath],
        renderer: Option<&'a Ratatex>,
        previous_formulas: &'a [StreamingFormulaFrame],
        math_fallback: MathFallback,
    ) -> Self {
        Self {
            width,
            lines: vec![Line::styled(
                "● Nanocodex",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )],
            current: Vec::new(),
            styles: vec![Style::default()],
            lists: Vec::new(),
            pending_item_prefix: None,
            quote_depth: 0,
            code_block: None,
            table: None,
            image: None,
            links: Vec::new(),
            rendered_links: Vec::new(),
            formulas,
            renderer,
            previous_formulas,
            math_fallback,
            rendered_formulas: Vec::new(),
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
        if let Some(image) = &mut self.image {
            match event {
                Event::Text(text) | Event::Code(text) => image.alt.push_str(&text),
                Event::SoftBreak | Event::HardBreak => image.alt.push(' '),
                Event::End(TagEnd::Image) => self.end_image(),
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
                if self.lists.is_empty() {
                    self.blank_line();
                }
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
            Event::End(TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough) => {
                self.pop_style();
            }
            Event::End(TagEnd::Link) => {
                self.pop_style();
                let _ = self.links.pop();
            }
            Event::Start(Tag::Strong) => {
                self.push_style(Style::default().add_modifier(Modifier::BOLD));
            }
            Event::Start(Tag::Strikethrough) => {
                self.push_style(Style::default().add_modifier(Modifier::CROSSED_OUT));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                self.links.push(Arc::from(dest_url.into_string()));
                self.push_style(
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
            Event::Start(Tag::Image { title, .. }) => {
                self.flush_current();
                self.image = Some(MarkdownImage {
                    title: title.into_string(),
                    alt: String::new(),
                });
            }
            Event::Text(text) => self.append_text(&text),
            Event::Code(code) => {
                let style = self.current_style().patch(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM),
                );
                self.ensure_prefix();
                self.push_current(Span::styled(code.into_string(), style));
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
            Event::Html(html) | Event::InlineHtml(html) => match math_marker(&html) {
                Some(MathMarker::Display(index)) => self.append_display_math(index),
                Some(MathMarker::Inline(index)) => self.append_inline_math(index),
                None => {
                    let plain = strip_html(&html);
                    if !plain.is_empty() {
                        self.append_text(&plain);
                    }
                }
            },
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
                self.push_current(Span::styled(part.to_owned(), self.current_style()));
            }
            if parts.peek().is_some() {
                self.flush_current();
            }
        }
    }

    fn append_display_math(&mut self, index: usize) {
        let Some(formula) = self.formulas.get(index) else {
            return;
        };
        debug_assert!(formula.display);
        let source = formula.source.clone();
        let full_source = formula.full_source.clone();
        self.flush_current();
        self.ensure_prefix();
        let column = self
            .current
            .iter()
            .map(|span| UnicodeWidthStr::width(span.span.content.as_ref()))
            .sum::<usize>()
            .min(usize::from(u16::MAX)) as u16;
        let max_columns = self.width.saturating_sub(column).max(1);
        let state = self.renderer.map_or(FormulaState::Unsupported, |renderer| {
            renderer.request(&source, max_columns)
        });
        match state {
            FormulaState::Ready(formula) => {
                self.append_ready_display_formula(index, formula, full_source, column);
            }
            FormulaState::Pending => {
                if let Some(previous) = self.previous_formula(index, &source, max_columns) {
                    self.append_ready_display_formula(index, previous, full_source, column);
                } else if self.math_fallback.hides_pending() {
                    self.append_hidden_formula();
                } else {
                    self.append_formula_source(&full_source);
                }
            }
            FormulaState::Failed(_) if self.math_fallback.hides_failed() => {
                if let Some(previous) = self.previous_formula(index, &source, max_columns) {
                    self.append_ready_display_formula(index, previous, full_source, column);
                } else {
                    self.append_hidden_formula();
                }
            }
            FormulaState::Failed(_) | FormulaState::Unsupported => {
                self.append_formula_source(&full_source);
            }
        }
    }

    fn append_inline_math(&mut self, index: usize) {
        let Some(formula) = self.formulas.get(index) else {
            return;
        };
        debug_assert!(!formula.display);
        let source = formula.source.clone();
        let full_source = formula.full_source.clone();
        self.ensure_prefix();
        let column = self.current_width();
        let max_columns = self.width.saturating_sub(column).max(1);
        let state = self.renderer.map_or(FormulaState::Unsupported, |renderer| {
            renderer.request(&source, max_columns)
        });
        let ready = match state {
            FormulaState::Ready(formula) => Some(formula),
            FormulaState::Pending => self.previous_formula(index, &source, max_columns),
            FormulaState::Failed(_) | FormulaState::Unsupported => None,
        };
        if let Some(formula) = ready
            && formula.rows() == 1
        {
            let source_width =
                UnicodeWidthStr::width(full_source.as_str()).min(usize::from(u16::MAX)) as u16;
            let reserved = formula.columns().max(source_width);
            if reserved <= self.width.saturating_sub(column) {
                let formula_column =
                    column.saturating_add(reserved.saturating_sub(formula.columns()) / 2);
                self.push_current(Span::raw(" ".repeat(usize::from(reserved))));
                self.rendered_formulas.push(MarkdownFormula {
                    index,
                    formula,
                    full_source: full_source.into(),
                    line: self.lines.len(),
                    column: formula_column,
                    source_column: column,
                    block: false,
                });
                return;
            }
        }
        self.append_text(&full_source);
    }

    fn previous_formula(
        &self,
        index: usize,
        current_source: &str,
        max_columns: u16,
    ) -> Option<Arc<Formula>> {
        let previous = self.previous_formulas.get(index)?;
        if let Some(renderer) = self.renderer {
            for source in previous.sources.iter().rev() {
                if source.as_ref() == current_source {
                    continue;
                }
                if let FormulaState::Ready(formula) = renderer.request(source, max_columns) {
                    return Some(formula);
                }
            }
        }
        previous
            .formula
            .as_ref()
            .filter(|formula| formula.columns() <= max_columns)
            .cloned()
    }

    fn append_ready_display_formula(
        &mut self,
        index: usize,
        formula: Arc<Formula>,
        full_source: String,
        content_column: u16,
    ) {
        self.current.clear();
        let available = self.width.saturating_sub(content_column);
        let column = content_column.saturating_add(available.saturating_sub(formula.columns()) / 2);
        let line = self.lines.len();
        self.lines
            .extend((0..formula.rows()).map(|_| Line::raw(" ")));
        self.rendered_formulas.push(MarkdownFormula {
            index,
            formula,
            full_source: full_source.into(),
            line,
            column,
            source_column: column,
            block: true,
        });
    }

    fn append_hidden_formula(&mut self) {
        self.current.clear();
        self.lines.push(Line::raw(" "));
    }

    fn append_formula_source(&mut self, full_source: &str) {
        self.append_text(full_source);
        self.flush_current();
        self.blank_line();
    }

    fn ensure_prefix(&mut self) {
        if !self.current.is_empty() {
            return;
        }
        self.push_unlinked(Span::raw("  "));
        if self.quote_depth > 0 {
            self.push_unlinked(Span::styled(
                "│ ".repeat(self.quote_depth),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if let Some(prefix) = self.pending_item_prefix.take() {
            let indent = "  ".repeat(self.lists.len().saturating_sub(1));
            self.push_unlinked(Span::raw(indent));
            self.push_unlinked(Span::styled(prefix, Style::default().fg(Color::Green)));
        }
    }

    fn current_width(&self) -> u16 {
        self.current
            .iter()
            .map(|span| UnicodeWidthStr::width(span.span.content.as_ref()))
            .sum::<usize>()
            .min(usize::from(u16::MAX)) as u16
    }

    fn flush_current(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let line = self.lines.len();
        let mut column = 0_u16;
        for tagged in &self.current {
            let width = u16::try_from(UnicodeWidthStr::width(tagged.span.content.as_ref()))
                .unwrap_or(u16::MAX);
            if let Some(destination) = &tagged.link
                && width > 0
            {
                let end = column.saturating_add(width);
                if let Some(last) = self.rendered_links.last_mut()
                    && last.0 == line
                    && last.1.destination == *destination
                    && last.1.end == column
                {
                    last.1.end = end;
                } else {
                    self.rendered_links.push((
                        line,
                        LinkSpan {
                            destination: Arc::clone(destination),
                            start: column,
                            end,
                        },
                    ));
                }
            }
            column = column.saturating_add(width);
        }
        self.lines.push(Line::from(
            std::mem::take(&mut self.current)
                .into_iter()
                .map(|tagged| tagged.span)
                .collect::<Vec<_>>(),
        ));
    }

    fn push_current(&mut self, span: Span<'static>) {
        self.current.push(TaggedSpan {
            span,
            link: self.links.last().cloned(),
        });
    }

    fn push_unlinked(&mut self, span: Span<'static>) {
        self.current.push(TaggedSpan { span, link: None });
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
        for line in highlighted_code_lines(code.language.as_deref(), &code.source) {
            let mut spans = vec![Span::raw("    ")];
            spans.extend(line.spans);
            self.lines.push(Line::from(spans));
        }
        self.blank_line();
    }

    fn end_image(&mut self) {
        let Some(image) = self.image.take() else {
            return;
        };
        let label = if image.alt.trim().is_empty() {
            image.title.trim()
        } else {
            image.alt.trim()
        };
        let label = if label.is_empty() { "image" } else { label };
        self.lines.push(Line::from(vec![
            Span::styled("  🖼 ", Style::default().fg(Color::DarkGray)),
            Span::styled(label.to_owned(), Style::default().fg(Color::White)),
        ]));
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

    fn finish(mut self) -> RenderedAgentMarkdown {
        self.flush_current();
        while self
            .lines
            .last()
            .is_some_and(|line| line.spans.iter().all(|span| span.content.is_empty()))
        {
            let _ = self.lines.pop();
        }
        self.lines.push(Line::raw(""));
        let mut links = vec![Vec::new(); self.lines.len()];
        for (line, link) in self.rendered_links {
            if let Some(line_links) = links.get_mut(line) {
                line_links.push(link);
            }
        }
        RenderedAgentMarkdown {
            text: Text::from(self.lines),
            links,
            formulas: self.rendered_formulas,
            formula_sources: self
                .formulas
                .iter()
                .map(|formula| Arc::from(formula.source.as_str()))
                .collect(),
            math_generation: (!self.formulas.is_empty())
                .then(|| self.renderer.map(Ratatex::generation))
                .flatten(),
        }
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
    use std::{collections::HashSet, sync::mpsc, time::Duration};

    use ratatex::{PixelSize, Ratatex, TerminalProfile};
    use ratatui::{Terminal, backend::TestBackend, widgets::Paragraph};

    use super::{
        code_line_count, heal_streaming_markdown, highlighted_code_lines, render_agent_markdown,
        render_agent_markdown_with_math, restore_markdown_links,
    };

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
        assert!(rendered.contains("fn main"));
        assert!(!rendered.contains("LOC"));
        assert!(!rendered.contains("┌─"));
        assert!(rendered.contains("fn main() {}"));
    }

    #[test]
    fn unsupported_graphics_preserves_display_math_as_text() {
        let renderer = Ratatex::builder(TerminalProfile::unsupported(PixelSize::default()))
            .build()
            .unwrap();
        let rendered = render_agent_markdown_with_math(
            "Before\n\n$$\\nabla\\cdot\\mathbf{u}=0$$\n\nAfter",
            60,
            &renderer,
        );
        let text = rendered
            .text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(text.contains(r"$$\nabla\cdot\mathbf{u}=0$$"));
        assert!(rendered.formulas.is_empty());
        assert!(rendered.math_generation.is_some());
        renderer.shutdown();
    }

    #[test]
    fn unsupported_graphics_preserves_inline_math_as_text() {
        let renderer = Ratatex::builder(TerminalProfile::unsupported(PixelSize::default()))
            .build()
            .unwrap();
        let rendered = render_agent_markdown_with_math(
            r"The bound \(d\le P(n)^2\) is sufficient.",
            60,
            &renderer,
        );
        let text = rendered
            .text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(text.contains(r"The bound \(d\le P(n)^2\) is sufficient."));
        assert!(rendered.formulas.is_empty());
        renderer.shutdown();
    }

    #[test]
    fn inline_math_renders_inside_the_surrounding_markdown_line() {
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let cache = tempfile::tempdir().unwrap();
        let renderer = Ratatex::builder(TerminalProfile::kitty(PixelSize::new(10, 40), false))
            .cache_dir(cache.path())
            .on_update(move || {
                let _ = wake_tx.try_send(());
            })
            .build()
            .unwrap();
        let source = r"- The bound \(d\le P(n)^2\) for boundary obstructions.";

        let pending = render_agent_markdown_with_math(source, 80, &renderer);
        let pending_text = pending
            .text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(pending_text.contains(r"\(d\le P(n)^2\)"));
        wake_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let rendered = render_agent_markdown_with_math(source, 80, &renderer);
        assert_eq!(rendered.formulas.len(), 1);
        let formula = &rendered.formulas[0];
        assert!(!formula.block);
        assert_eq!(formula.formula.rows(), 1);
        let line = rendered.text.lines[formula.line]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(line.contains("• The bound "));
        assert!(line.contains(" for boundary obstructions."));
        assert!(!line.contains(r"\le"));
        renderer.shutdown();
    }

    #[test]
    fn display_math_becomes_placeholder_rows() {
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let cache = tempfile::tempdir().unwrap();
        let renderer = Ratatex::builder(TerminalProfile::kitty(PixelSize::new(10, 20), false))
            .cache_dir(cache.path())
            .on_update(move || {
                let _ = wake_tx.try_send(());
            })
            .build()
            .unwrap();
        let source = r"Before

$$
\rho\left(\frac{\partial \mathbf{u}}{\partial t}
+(\mathbf{u}\cdot\nabla)\mathbf{u}\right)
=-\nabla p+\mu\nabla^2\mathbf{u}+\rho\mathbf{f}
$$

After";

        let pending = render_agent_markdown_with_math(source, 100, &renderer);
        assert!(pending.formulas.is_empty());
        assert!(
            pending
                .text
                .lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .all(|span| !span.content.contains(r"\rho"))
        );
        wake_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let rendered = render_agent_markdown_with_math(source, 100, &renderer);
        assert_eq!(rendered.formulas.len(), 1);
        assert!(rendered.formulas[0].formula.columns() > 10);
        assert!(rendered.formulas[0].formula.rows() > 1);
        assert_eq!(renderer.drain_terminal_commands().len(), 1);
        renderer.shutdown();
    }

    #[test]
    fn display_math_uses_only_one_surrounding_markdown_gap() {
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let cache = tempfile::tempdir().unwrap();
        let renderer = Ratatex::builder(TerminalProfile::kitty(PixelSize::new(10, 20), false))
            .cache_dir(cache.path())
            .on_update(move || {
                let _ = wake_tx.try_send(());
            })
            .build()
            .unwrap();
        let source = "### Maxwell\n\n\\[\\nabla\\cdot\\mathbf{E}=0\\]\n\n### Next";

        let _ = render_agent_markdown_with_math(source, 80, &renderer);
        wake_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let rendered = render_agent_markdown_with_math(source, 80, &renderer);
        let formula = &rendered.formulas[0];
        let lines = rendered
            .text
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let maxwell = lines
            .iter()
            .position(|line| line.contains("Maxwell"))
            .unwrap();
        let next = lines.iter().position(|line| line.contains("Next")).unwrap();

        assert_eq!(formula.line, maxwell + 2);
        assert_eq!(next, formula.line + usize::from(formula.formula.rows()));
        assert_eq!(
            formula.column,
            2 + (78_u16.saturating_sub(formula.formula.columns())) / 2
        );
        renderer.shutdown();
    }

    #[test]
    fn nested_lists_do_not_insert_blank_rows_between_tiers() {
        let rendered =
            render_agent_markdown("- parent\n  - child\n    - grandchild\n- sibling", 60);
        let lines = rendered
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let parent = lines
            .iter()
            .position(|line| line.contains("• parent"))
            .unwrap();
        let child = lines
            .iter()
            .position(|line| line.contains("• child"))
            .unwrap();
        let grandchild = lines
            .iter()
            .position(|line| line.contains("• grandchild"))
            .unwrap();
        let sibling = lines
            .iter()
            .position(|line| line.contains("• sibling"))
            .unwrap();

        assert_eq!(child, parent + 1);
        assert_eq!(grandchild, child + 1);
        assert_eq!(sibling, grandchild + 1);
    }

    #[test]
    fn semantic_copy_restores_markdown_link_destinations() {
        assert_eq!(
            restore_markdown_links(
                "Read the docs and API reference".to_owned(),
                "Read the **[docs](https://example.com/docs)** and [API `reference`](https://example.com/api)",
            ),
            "Read the [docs](https://example.com/docs) and [API reference](https://example.com/api)"
        );
        assert_eq!(
            restore_markdown_links(
                "docsify docs docs".to_owned(),
                "docsify docs [docs](https://example.com/docs)",
            ),
            "docsify docs [docs](https://example.com/docs)"
        );
        assert_eq!(
            restore_markdown_links(
                "docu".to_owned(),
                "Read [documentation](https://example.com/docs) now",
            ),
            "[docu](https://example.com/docs)"
        );
        assert_eq!(
            restore_markdown_links(
                "Read the\ndocs".to_owned(),
                "Read the [docs](https://example.com/docs)",
            ),
            "Read the [docs](https://example.com/docs)"
        );
    }

    #[test]
    fn semantic_copy_replaces_fenced_code_chrome_with_raw_code() {
        let source = "Run this:\n\n```sh\necho '{\"garbageCollector\":{\"strategy\":\"disabled\"}}' |\n  sudo tee /etc/determinate/config.json >/dev/null\n\nsudo launchctl kickstart -k system/systems.determinate.nix-daemon\njust switch\n```";
        let rendered = "┌─ sh · 5 LOC\n  │ echo '{\"garbageCollector\":{\"strategy\":\"disabled\"}}' |\ncontinued after a visual wrap\n  │   sudo tee /etc/determinate/config.json >/dev/null\n  └─";

        assert_eq!(
            restore_markdown_links(rendered.to_owned(), source),
            "echo '{\"garbageCollector\":{\"strategy\":\"disabled\"}}' |\n  sudo tee /etc/determinate/config.json >/dev/null\n\nsudo launchctl kickstart -k system/systems.determinate.nix-daemon\njust switch"
        );
    }

    #[test]
    fn semantic_copy_strips_gutters_when_only_the_code_body_is_selected() {
        let source = "```javascript\nconst first = 1;\nconst second = 2;\n```";
        let selected = "const first = 1;\n  │ const second = 2;";

        assert_eq!(
            restore_markdown_links(selected.to_owned(), source),
            "const first = 1;\nconst second = 2;"
        );
    }

    #[test]
    fn semantic_copy_strips_rendered_block_quote_gutters() {
        let source = "Intro\n\n> got an awesome group speaking\n>\n> @rauch (Vercel)\n> @sqs (Amp)\n>\n> october 12-14";
        let selected = "Intro\n\n  │ got an awesome group speaking\n\n  │ @rauch (Vercel)\n  │ @sqs (Amp)\n\n  │ october 12-14";

        assert_eq!(
            restore_markdown_links(selected.to_owned(), source),
            "Intro\ngot an awesome group speaking\n@rauch (Vercel)\n@sqs (Amp)\noctober 12-14"
        );
    }

    #[test]
    fn semantic_copy_of_a_whole_rendered_quote_drops_the_header_and_gutters() {
        let source = "Intro\n\n> got an awesome group speaking\n>\n> @rauch (Vercel)\n> @sqs (Amp)\n>\n> october 12-14";
        let selected = render_agent_markdown(source, 120)
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            restore_markdown_links(selected, source),
            "Intro\ngot an awesome group speaking\n@rauch (Vercel)\n@sqs (Amp)\noctober 12-14"
        );
    }

    #[test]
    fn semantic_copy_prefers_a_literal_vertical_bar_over_gutter_stripping() {
        assert_eq!(
            restore_markdown_links(
                "│ literal text".to_owned(),
                "│ literal text\n\nliteral text"
            ),
            "│ literal text"
        );
    }

    #[test]
    fn semantic_copy_keeps_literal_urls_raw() {
        assert_eq!(
            restore_markdown_links(
                "https://example.com/docs and docs".to_owned(),
                "https://example.com/docs and [docs](https://example.com/documentation)",
            ),
            "https://example.com/docs and [docs](https://example.com/documentation)"
        );
        assert_eq!(
            restore_markdown_links(
                "https://example.com".to_owned(),
                "[https://example.com](https://redirect.example)",
            ),
            "[https://example.com](https://redirect.example)"
        );
    }

    #[test]
    fn markdown_images_render_a_bounded_placeholder() {
        let destination = format!("data:image/png;base64,{}", "A".repeat(100_000));
        let rendered = render_agent_markdown(
            &format!("before\n\n![deployment chart]({destination})\n\nafter"),
            80,
        );
        let content = rendered
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(content.contains("before"));
        assert!(content.contains("🖼 deployment chart"));
        assert!(content.contains("after"));
        assert!(!content.contains("base64"));
        assert!(content.len() < 200);
    }

    #[test]
    fn code_line_counts_ignore_only_terminal_newlines() {
        assert_eq!(code_line_count(""), 0);
        assert_eq!(code_line_count("one\n"), 1);
        assert_eq!(code_line_count("one\n\ntwo\n\n"), 3);
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

    #[test]
    fn skips_highlighting_for_oversized_source_lines() {
        let source = format!("let value = \"{}\";", "x".repeat(4 * 1024));
        let highlighted = highlighted_code_lines(Some("rust"), &source);

        assert_eq!(highlighted.len(), 1);
        assert_eq!(highlighted[0].spans.len(), 1);
        assert_eq!(highlighted[0].spans[0].content, source);
        assert_eq!(
            highlighted[0].spans[0].style.fg,
            Some(ratatui::style::Color::Yellow)
        );
    }

    #[test]
    fn highlights_typescript_and_tsx() {
        for (language, source) in [
            (
                "typescript",
                "interface User { name: string }\nconst user: User = { name: \"Ada\" };",
            ),
            (
                "tsx",
                "const Greeting = ({ name }: { name: string }) => <p>Hello {name}</p>;",
            ),
        ] {
            let highlighted = highlighted_code_lines(Some(language), source);
            let colors = highlighted
                .iter()
                .flat_map(|line| line.spans.iter())
                .filter_map(|span| span.style.fg)
                .collect::<HashSet<_>>();
            assert!(
                colors.len() > 1,
                "{language} source should use multiple syntax colors"
            );
        }
    }
}
