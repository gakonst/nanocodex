// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use crate::tui::{format::sanitize_terminal_text, theme::Theme};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use ratatui_image::sliced::SlicedProtocol;
use std::{ops::Range, path::Path, sync::Arc};
use syntect::easy::HighlightLines;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) struct Layout {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) images: Vec<ImagePlacement>,
    pub(super) links: Vec<Vec<LinkSpan>>,
    pub(super) selections: Vec<Vec<SourceSpan>>,
    pub(super) envelopes: Vec<SourceEnvelope>,
    pub(super) selection_source: Option<String>,
    pub(super) image_state: ImageState,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub(super) enum ImageState {
    #[default]
    None,
    Ready,
    Pending,
}

pub(super) struct ImagePlacement {
    pub(super) line: usize,
    pub(super) destination: Arc<str>,
    pub(super) protocol: Arc<SlicedProtocol>,
    pub(super) retransmit: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceSpan {
    pub(super) columns: Range<u16>,
    pub(super) source: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceEnvelope {
    pub(super) content: Range<usize>,
    pub(super) source: Range<usize>,
}

#[derive(Clone)]
pub(super) struct LinkSpan {
    pub(super) destination: Arc<str>,
    pub(super) start: u16,
    pub(super) end: u16,
}

pub(super) fn render(markdown: &str, width: u16, theme: &Theme) -> Layout {
    let workspace = std::env::current_dir().unwrap_or_default();
    render_in(markdown, width, theme, &workspace)
}

pub(super) fn render_in(markdown: &str, width: u16, theme: &Theme, workspace: &Path) -> Layout {
    render_cached(
        markdown,
        width,
        theme,
        workspace,
        &mut super::image::Cache::default(),
    )
}

pub(super) fn render_cached(
    markdown: &str,
    width: u16,
    theme: &Theme,
    workspace: &Path,
    images: &mut super::image::Cache,
) -> Layout {
    if width == 0 {
        return Layout {
            lines: Vec::new(),
            images: Vec::new(),
            links: Vec::new(),
            selections: Vec::new(),
            envelopes: Vec::new(),
            selection_source: None,
            image_state: ImageState::None,
        };
    }
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_SMART_PUNCTUATION
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM;
    let mut renderer = Renderer::new(width, theme, workspace, images);
    let mut events = Parser::new_ext(markdown, options).peekable();
    while let Some(event) = events.next() {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                renderer.flush();
                renderer.code_block(kind, &mut events);
            }
            Event::Start(Tag::Table(_)) => {
                renderer.flush();
                renderer.table(&mut events);
            }
            event => renderer.event(event),
        }
    }
    let (mut layout, selection_exclusions, image_selection_modes) = renderer.finish();
    let (selections, envelopes) = markdown_selection_spans(
        markdown,
        &layout.lines,
        options,
        &selection_exclusions,
        &image_selection_modes,
    );
    layout.selections = selections;
    layout.envelopes = envelopes;
    layout
}

pub(super) fn plain_selection_spans(source: &str, lines: &[Line<'static>]) -> Vec<Vec<SourceSpan>> {
    plain_selection_spans_excluding(source, lines, &[])
}

pub(super) fn plain_selection_spans_excluding(
    source: &str,
    lines: &[Line<'static>],
    exclusions: &[Vec<Range<u16>>],
) -> Vec<Vec<SourceSpan>> {
    let graphemes = source_graphemes(source, source, 0..source.len());
    align_source_graphemes(&graphemes, lines, exclusions)
}

pub(super) fn wrap_plain(text: &str, width: u16, style: Style) -> Vec<Line<'static>> {
    wrap_plain_with_whitespace(text, width, style, false)
}

pub(super) fn wrap_plain_preserving_whitespace(
    text: &str,
    width: u16,
    style: Style,
) -> Vec<Line<'static>> {
    wrap_plain_with_whitespace(text, width, style, true)
}

fn wrap_plain_with_whitespace(
    text: &str,
    width: u16,
    style: Style,
    preserve_whitespace: bool,
) -> Vec<Line<'static>> {
    let logical = sanitize(text);
    let mut lines = Vec::new();
    for line in logical.split('\n') {
        let spans = vec![Span::styled(line.to_owned(), style)];
        lines.extend(wrap_spans_with_whitespace(
            &spans,
            width,
            true,
            preserve_whitespace,
        ));
    }
    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
}

pub(super) fn sanitize(text: &str) -> String {
    sanitize_terminal_text(text).into_owned()
}

struct Renderer<'a> {
    width: u16,
    theme: &'a Theme,
    lines: Vec<Line<'static>>,
    current: Vec<TaggedSpan>,
    styles: Vec<Style>,
    lists: Vec<ListState>,
    quote_depth: usize,
    links: Vec<LinkState>,
    rendered_links: Vec<(usize, LinkSpan)>,
    selection_exclusions: Vec<Vec<Range<u16>>>,
    image_selection_modes: Vec<ImageSelectionMode>,
    images: Vec<ImagePlacement>,
    image_state: ImageState,
    image: Option<LinkState>,
    workspace: &'a Path,
    images_cache: &'a mut super::image::Cache,
}

struct ListState {
    next: Option<u64>,
}

struct LinkState {
    destination: Arc<str>,
    label: String,
}

#[derive(Clone, Copy)]
enum ImageSelectionMode {
    Hidden,
    AltText,
    Link,
}

struct TaggedSpan {
    span: Span<'static>,
    link: Option<Arc<str>>,
}

impl<'a> Renderer<'a> {
    fn new(
        width: u16,
        theme: &'a Theme,
        workspace: &'a Path,
        images_cache: &'a mut super::image::Cache,
    ) -> Self {
        Self {
            width,
            theme,
            lines: Vec::new(),
            current: Vec::new(),
            styles: vec![Style::default().fg(theme.text())],
            lists: Vec::new(),
            quote_depth: 0,
            links: Vec::new(),
            rendered_links: Vec::new(),
            selection_exclusions: Vec::new(),
            image_selection_modes: Vec::new(),
            images: Vec::new(),
            image_state: ImageState::None,
            image: None,
            workspace,
            images_cache,
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => self.span(
                &code,
                Style::default()
                    .fg(self.theme.code_text())
                    .bg(self.theme.code_background()),
            ),
            Event::InlineMath(math) => self.span(
                &format!("${}$", sanitize(&math)),
                Style::default().fg(self.theme.thinking_high()),
            ),
            Event::DisplayMath(math) => {
                self.flush();
                self.span(
                    &format!("  $${}$$", sanitize(&math)),
                    Style::default().fg(self.theme.thinking_high()),
                );
                self.flush();
                self.blank();
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                self.span(&html, Style::default().fg(self.theme.muted()));
            }
            Event::FootnoteReference(reference) => self.span(
                &format!("[{}]", sanitize(&reference)),
                Style::default().fg(self.theme.thinking_medium()),
            ),
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                self.lines.push(Line::from(Span::styled(
                    "─".repeat(usize::from(self.width)),
                    Style::default().fg(self.theme.muted()),
                )));
                self.blank();
            }
            Event::TaskListMarker(checked) => self.span(
                if checked { "✓ " } else { "□ " },
                Style::default().fg(if checked {
                    self.theme.thinking_medium()
                } else {
                    self.theme.muted()
                }),
            ),
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.ensure_prefix(),
            Tag::Heading { level, .. } => {
                self.flush();
                let style = heading_style(level, self.theme);
                self.push_unlinked(Span::styled("▍ ", style));
                self.push_style(style);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote_depth = self.quote_depth.saturating_add(1);
                self.push_style(Style::default().fg(self.theme.muted()));
            }
            Tag::List(start) => self.lists.push(ListState { next: start }),
            Tag::Item => {
                self.flush();
                self.ensure_quote_prefix();
                let depth = self.lists.len().saturating_sub(1);
                self.push_unlinked(Span::raw("  ".repeat(depth)));
                let marker = self.lists.last_mut().map_or_else(
                    || "• ".to_owned(),
                    |list| match &mut list.next {
                        Some(next) => {
                            let marker = format!("{next}. ");
                            *next = next.saturating_add(1);
                            marker
                        }
                        None => "• ".to_owned(),
                    },
                );
                self.push_unlinked(Span::styled(
                    marker,
                    Style::default().fg(self.theme.accent()),
                ));
            }
            Tag::Emphasis => self.push_style(Style::default().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(Style::default().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.push_style(
                Style::default()
                    .fg(self.theme.muted())
                    .add_modifier(Modifier::CROSSED_OUT),
            ),
            Tag::Superscript => self.text("^"),
            Tag::Subscript => self.text("~"),
            Tag::Link { dest_url, .. } => {
                self.links.push(LinkState {
                    destination: Arc::from(sanitize(&dest_url)),
                    label: String::new(),
                });
                self.push_style(
                    Style::default()
                        .fg(self.theme.accent())
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
            Tag::Image { dest_url, .. } => {
                self.image = Some(LinkState {
                    destination: Arc::from(sanitize(&dest_url)),
                    label: String::new(),
                });
            }
            Tag::FootnoteDefinition(label) => {
                self.flush();
                self.push_unlinked(Span::styled(
                    format!("[{}] ", sanitize(&label)),
                    Style::default().fg(self.theme.thinking_medium()),
                ));
            }
            Tag::DefinitionListTitle => {
                self.flush();
                self.push_style(Style::default().add_modifier(Modifier::BOLD));
            }
            Tag::DefinitionListDefinition => {
                self.flush();
                self.push_unlinked(Span::styled(
                    "  : ",
                    Style::default().fg(self.theme.muted()),
                ));
            }
            Tag::HtmlBlock | Tag::MetadataBlock(_) => {
                self.flush();
                self.push_style(Style::default().fg(self.theme.muted()));
            }
            Tag::CodeBlock(_)
            | Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::DefinitionList => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
                self.blank();
            }
            TagEnd::Heading(_) => {
                self.pop_style();
                self.flush();
                self.blank();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.pop_style();
                self.blank();
            }
            TagEnd::List(_) => {
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blank();
                }
            }
            TagEnd::Item => self.flush(),
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            TagEnd::Superscript => self.text("^"),
            TagEnd::Subscript => self.text("~"),
            TagEnd::Link => {
                self.pop_style();
                if let Some(link) = self.links.pop()
                    && link.label.trim() != link.destination.as_ref()
                {
                    self.push_linked(
                        Span::styled(
                            format!(" ↗ {}", link.destination),
                            Style::default()
                                .fg(self.theme.accent())
                                .add_modifier(Modifier::DIM),
                        ),
                        link.destination,
                    );
                }
            }
            TagEnd::Image => {
                if self.image_state == ImageState::None {
                    self.image_state = ImageState::Ready;
                }
                if let Some(image) = self.image.take() {
                    match self
                        .images_cache
                        .load(&image.destination, self.workspace, self.width)
                    {
                        super::image::LoadResult::Loaded(protocol) => {
                            self.image_selection_modes.push(ImageSelectionMode::Hidden);
                            self.flush();
                            let line = self.lines.len();
                            let size = protocol.size();
                            self.lines.extend((0..size.height).map(|_| {
                                Line::from(Span::raw(" ".repeat(usize::from(size.width))))
                            }));
                            self.images.push(ImagePlacement {
                                line,
                                destination: image.destination,
                                protocol,
                                retransmit: false,
                            });
                        }
                        super::image::LoadResult::Unsupported => {
                            self.image_selection_modes.push(ImageSelectionMode::Link);
                            self.render_image_link(image);
                        }
                        super::image::LoadResult::Deferred => {
                            self.image_state = ImageState::Pending;
                            self.image_selection_modes.push(ImageSelectionMode::Link);
                            self.render_image_link(image);
                        }
                        super::image::LoadResult::Failed => {
                            self.image_selection_modes.push(ImageSelectionMode::Hidden);
                            self.push_unlinked(Span::styled(
                                "image could not be rendered",
                                Style::default().fg(ratatui::style::Color::Red),
                            ));
                        }
                    }
                }
            }
            TagEnd::FootnoteDefinition
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::HtmlBlock
            | TagEnd::MetadataBlock(_) => {
                self.pop_style();
                self.flush();
                self.blank();
            }
            TagEnd::CodeBlock
            | TagEnd::DefinitionList
            | TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell => {}
        }
    }

    fn text(&mut self, text: &str) {
        let text = sanitize(text);
        if let Some(image) = &mut self.image {
            image.label.push_str(&text);
            return;
        }
        if let Some(link) = self.links.last_mut() {
            link.label.push_str(&text);
        }
        self.ensure_prefix();
        self.push_current(Span::styled(text, self.style()));
    }

    fn span(&mut self, text: &str, style: Style) {
        let text = sanitize(text);
        if let Some(image) = &mut self.image {
            image.label.push_str(&text);
            return;
        }
        self.ensure_prefix();
        self.push_current(Span::styled(text, self.style().patch(style)));
    }

    fn code_block(
        &mut self,
        kind: CodeBlockKind<'_>,
        events: &mut std::iter::Peekable<Parser<'_>>,
    ) {
        let language = match kind {
            CodeBlockKind::Indented => None,
            CodeBlockKind::Fenced(language) if language.is_empty() => None,
            CodeBlockKind::Fenced(language) => Some(sanitize(&language)),
        };
        let mut code = String::new();
        for event in events.by_ref() {
            match event {
                Event::End(TagEnd::CodeBlock) => break,
                Event::Text(text) | Event::Code(text) => code.push_str(&sanitize(&text)),
                Event::SoftBreak | Event::HardBreak => code.push('\n'),
                _ => {}
            }
        }
        let is_diff = language.as_deref().is_some_and(is_diff_language);
        if is_diff {
            self.lines
                .extend(super::diff::render(&code, self.width, self.theme));
            self.blank();
            return;
        }
        if self.width < 6 {
            self.narrow_code_block(&code, language.as_deref());
            self.blank();
            return;
        }

        let border = Style::default().fg(self.theme.border());
        let assets = super::highlight::assets();
        let syntax = language.as_deref().map_or_else(
            || assets.syntaxes.find_syntax_plain_text(),
            |language| super::highlight::syntax_for_token(&assets.syntaxes, language),
        );
        let syntax_theme = super::highlight::theme();
        let mut highlighter = HighlightLines::new(syntax, &syntax_theme);
        let header = self.lines.len();
        self.lines.push(code_block_header(
            language.as_deref(),
            self.width,
            self.theme,
        ));
        self.exclude_from_selection(header, 0..self.width);
        let content_width = self.width.saturating_sub(4).max(1);
        for source_line in code.trim_end_matches('\n').split('\n') {
            let highlighted =
                super::highlight::line(&mut highlighter, source_line, &assets.syntaxes);
            for (index, mut spans) in
                super::highlight::wrap(highlighted, content_width.saturating_sub(2).max(1))
                    .into_iter()
                    .enumerate()
            {
                if index > 0 {
                    spans.insert(
                        0,
                        Span::styled("↪ ", Style::default().fg(self.theme.muted())),
                    );
                }
                let used = spans.iter().map(Span::width).sum::<usize>();
                let padding = usize::from(content_width).saturating_sub(used);
                let mut body = Vec::with_capacity(spans.len() + 3);
                body.push(Span::styled("│ ", border));
                body.extend(spans);
                body.push(Span::raw(" ".repeat(padding)));
                body.push(Span::styled(" │", border));
                let line = self.lines.len();
                self.lines.push(Line::from(body));
                self.exclude_from_selection(line, 0..2);
                if index > 0 {
                    self.exclude_from_selection(line, 2..4);
                }
                let content_end = 2_u16
                    .saturating_add(u16::try_from(used).unwrap_or(u16::MAX))
                    .min(self.width);
                self.exclude_from_selection(line, content_end..self.width);
            }
        }
        let footer = self.lines.len();
        self.lines.push(Line::from(Span::styled(
            format!(
                "╰{}╯",
                "─".repeat(usize::from(self.width.saturating_sub(2)))
            ),
            border,
        )));
        self.exclude_from_selection(footer, 0..self.width);
        self.blank();
    }

    fn narrow_code_block(&mut self, code: &str, language: Option<&str>) {
        let gutter = Style::default().fg(self.theme.border());
        let assets = super::highlight::assets();
        let syntax = language.map_or_else(
            || assets.syntaxes.find_syntax_plain_text(),
            |language| super::highlight::syntax_for_token(&assets.syntaxes, language),
        );
        let syntax_theme = super::highlight::theme();
        let mut highlighter = HighlightLines::new(syntax, &syntax_theme);
        let content_width = self.width.saturating_sub(2).max(1);
        for source_line in code.trim_end_matches('\n').split('\n') {
            let highlighted =
                super::highlight::line(&mut highlighter, source_line, &assets.syntaxes);
            for spans in super::highlight::wrap(highlighted, content_width) {
                let mut line = vec![Span::styled("┃ ", gutter)];
                line.extend(spans);
                let line_index = self.lines.len();
                self.lines.push(Line::from(line));
                self.exclude_from_selection(line_index, 0..2);
            }
        }
    }

    fn table(&mut self, events: &mut std::iter::Peekable<Parser<'_>>) {
        let mut rows = Vec::<Vec<String>>::new();
        let mut row = Vec::<String>::new();
        let mut cell = String::new();
        let mut in_cell = false;
        let mut header_rows = 0_usize;
        let mut in_header = false;
        for event in events.by_ref() {
            match event {
                Event::Start(Tag::TableHead) => in_header = true,
                Event::End(TagEnd::TableHead) => {
                    if !row.is_empty() {
                        rows.push(std::mem::take(&mut row));
                        header_rows = header_rows.saturating_add(1);
                    }
                    in_header = false;
                }
                Event::Start(Tag::TableCell) => {
                    cell.clear();
                    in_cell = true;
                }
                Event::Start(Tag::Image { .. }) => {
                    self.image_selection_modes.push(ImageSelectionMode::AltText)
                }
                Event::End(TagEnd::TableCell) => {
                    row.push(cell.trim().to_owned());
                    in_cell = false;
                }
                Event::End(TagEnd::TableRow) => {
                    if in_header {
                        header_rows = header_rows.saturating_add(1);
                    }
                    rows.push(std::mem::take(&mut row));
                }
                Event::End(TagEnd::Table) => break,
                Event::Text(text) | Event::Code(text) if in_cell => {
                    cell.push_str(&sanitize(&text));
                }
                Event::SoftBreak | Event::HardBreak if in_cell => cell.push(' '),
                _ => {}
            }
        }
        self.lines
            .extend(render_table(&rows, header_rows, self.width, self.theme));
        self.blank();
    }

    fn ensure_prefix(&mut self) {
        if !self.current.is_empty() {
            return;
        }
        self.ensure_quote_prefix();
    }

    fn ensure_quote_prefix(&mut self) {
        for _ in 0..self.quote_depth {
            self.push_unlinked(Span::styled("▌ ", Style::default().fg(self.theme.accent())));
        }
    }

    fn push_style(&mut self, style: Style) {
        self.styles.push(self.style().patch(style));
    }

    fn pop_style(&mut self) {
        if self.styles.len() > 1 {
            self.styles.pop();
        }
    }

    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or_default()
    }

    fn push_current(&mut self, span: Span<'static>) {
        let link = self.links.last().map(|link| Arc::clone(&link.destination));
        self.current.push(TaggedSpan { span, link });
    }

    fn push_unlinked(&mut self, span: Span<'static>) {
        self.current.push(TaggedSpan { span, link: None });
    }

    fn push_linked(&mut self, span: Span<'static>, destination: Arc<str>) {
        self.current.push(TaggedSpan {
            span,
            link: Some(destination),
        });
    }

    fn render_image_link(&mut self, image: LinkState) {
        self.ensure_prefix();
        let label = if image.label.is_empty() {
            image.destination.to_string()
        } else {
            image.label
        };
        let include_destination = label.trim() != image.destination.as_ref();
        self.push_linked(
            Span::styled(
                label,
                Style::default()
                    .fg(self.theme.accent())
                    .add_modifier(Modifier::UNDERLINED),
            ),
            Arc::clone(&image.destination),
        );
        if include_destination {
            self.push_linked(
                Span::styled(
                    format!(" ↗ {}", image.destination),
                    Style::default()
                        .fg(self.theme.accent())
                        .add_modifier(Modifier::DIM),
                ),
                image.destination,
            );
        }
    }

    fn flush(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let first_line = self.lines.len();
        for (offset, (line, links)) in wrap_tagged_spans(&self.current, self.width, true)
            .into_iter()
            .enumerate()
        {
            self.lines.push(line);
            self.rendered_links.extend(
                links
                    .into_iter()
                    .map(|link| (first_line.saturating_add(offset), link)),
            );
        }
        self.current.clear();
    }

    fn blank(&mut self) {
        if self.lines.last().is_some_and(|line| line.width() == 0) {
            return;
        }
        self.lines.push(Line::default());
    }

    fn exclude_from_selection(&mut self, line: usize, columns: Range<u16>) {
        if self.selection_exclusions.len() <= line {
            self.selection_exclusions.resize_with(line + 1, Vec::new);
        }
        self.selection_exclusions[line].push(columns);
    }

    fn finish(mut self) -> (Layout, Vec<Vec<Range<u16>>>, Vec<ImageSelectionMode>) {
        self.flush();
        while self.lines.last().is_some_and(|line| line.width() == 0) {
            self.lines.pop();
        }
        let mut links = vec![Vec::new(); self.lines.len()];
        for (line, link) in self.rendered_links {
            if let Some(line_links) = links.get_mut(line) {
                line_links.push(link);
            }
        }
        let layout = Layout {
            lines: self.lines,
            images: self.images,
            links,
            selections: Vec::new(),
            envelopes: Vec::new(),
            selection_source: None,
            image_state: self.image_state,
        };
        (
            layout,
            self.selection_exclusions,
            self.image_selection_modes,
        )
    }
}

#[derive(Clone)]
struct SourceGrapheme {
    text: String,
    source: Range<usize>,
}

fn markdown_selection_spans(
    markdown: &str,
    lines: &[Line<'static>],
    options: Options,
    exclusions: &[Vec<Range<u16>>],
    image_selection_modes: &[ImageSelectionMode],
) -> (Vec<Vec<SourceSpan>>, Vec<SourceEnvelope>) {
    let mut graphemes = Vec::<SourceGrapheme>::new();
    let mut envelopes = Vec::<(TagEnd, Range<usize>, usize, bool, Option<String>, bool)>::new();
    let mut source_envelopes = Vec::new();
    let mut image_modes = image_selection_modes.iter().copied();
    let mut hidden_image_depth = 0_usize;
    for (event, range) in Parser::new_ext(markdown, options).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                let image_mode = matches!(tag, Tag::Image { .. })
                    .then(|| image_modes.next().unwrap_or(ImageSelectionMode::Hidden));
                let hidden_image = matches!(image_mode, Some(ImageSelectionMode::Hidden));
                if hidden_image {
                    hidden_image_depth = hidden_image_depth.saturating_add(1);
                }
                let destination = match &tag {
                    Tag::Link { dest_url, .. } => Some(sanitize(dest_url)),
                    Tag::Image { dest_url, .. }
                        if matches!(image_mode, Some(ImageSelectionMode::Link)) =>
                    {
                        Some(sanitize(dest_url))
                    }
                    _ => None,
                };
                let preserve_delimiters = !matches!(tag, Tag::CodeBlock(_));
                envelopes.push((
                    tag.to_end(),
                    range,
                    graphemes.len(),
                    preserve_delimiters,
                    destination,
                    hidden_image,
                ));
            }
            Event::End(end) => {
                let Some(index) = envelopes.iter().rposition(|(tag, ..)| *tag == end) else {
                    continue;
                };
                let (_, source, first, preserve_delimiters, destination, hidden_image) =
                    envelopes.remove(index);
                if preserve_delimiters && first != graphemes.len() {
                    source_envelopes.push(SourceEnvelope {
                        content: graphemes[first].source.start
                            ..graphemes
                                .last()
                                .expect("the envelope has content")
                                .source
                                .end,
                        source: source.clone(),
                    });
                }
                if let Some(destination) = destination {
                    let label = graphemes[first..]
                        .iter()
                        .map(|grapheme| grapheme.text.as_str())
                        .collect::<String>();
                    if label.trim() != destination {
                        let destination_source = markdown
                            .get(source.clone())
                            .and_then(|link| link.rfind(&destination))
                            .map_or_else(
                                || source.clone(),
                                |offset| {
                                    let start = source.start.saturating_add(offset);
                                    start..start.saturating_add(destination.len())
                                },
                            );
                        graphemes.extend(source_graphemes(
                            markdown,
                            &format!(" ↗ {destination}"),
                            destination_source,
                        ));
                    }
                }
                if hidden_image {
                    hidden_image_depth = hidden_image_depth.saturating_sub(1);
                }
            }
            Event::Text(text) | Event::Code(text) | Event::Html(text) | Event::InlineHtml(text)
                if hidden_image_depth == 0 =>
            {
                graphemes.extend(source_graphemes(markdown, &sanitize(&text), range));
            }
            Event::InlineMath(math) if hidden_image_depth == 0 => {
                graphemes.extend(source_graphemes(
                    markdown,
                    &format!("${}$", sanitize(&math)),
                    range,
                ));
            }
            Event::DisplayMath(math) if hidden_image_depth == 0 => {
                graphemes.extend(source_graphemes(
                    markdown,
                    &format!("$${}$$", sanitize(&math)),
                    range,
                ));
            }
            Event::FootnoteReference(reference) if hidden_image_depth == 0 => {
                graphemes.extend(source_graphemes(
                    markdown,
                    &format!("[{}]", sanitize(&reference)),
                    range,
                ));
            }
            Event::SoftBreak if hidden_image_depth == 0 => graphemes.push(SourceGrapheme {
                text: " ".to_owned(),
                source: range,
            }),
            Event::HardBreak if hidden_image_depth == 0 => graphemes.push(SourceGrapheme {
                text: "\n".to_owned(),
                source: range,
            }),
            Event::TaskListMarker(checked) if hidden_image_depth == 0 => {
                graphemes.extend(source_graphemes(
                    markdown,
                    if checked { "✓ " } else { "□ " },
                    range,
                ));
            }
            _ => {}
        }
    }
    (
        align_source_graphemes(&graphemes, lines, exclusions),
        source_envelopes,
    )
}

fn source_graphemes(raw: &str, rendered: &str, source: Range<usize>) -> Vec<SourceGrapheme> {
    let raw_fragment = raw.get(source.clone()).unwrap_or_default();
    if raw_fragment == rendered {
        return rendered
            .grapheme_indices(true)
            .map(|(offset, text)| SourceGrapheme {
                text: text.to_owned(),
                source: source.start + offset..source.start + offset + text.len(),
            })
            .collect();
    }
    rendered
        .graphemes(true)
        .map(|text| SourceGrapheme {
            text: text.to_owned(),
            source: source.clone(),
        })
        .collect()
}

struct RenderedGrapheme {
    line: usize,
    column: u16,
    text: String,
    width: u16,
}

fn align_source_graphemes(
    source: &[SourceGrapheme],
    lines: &[Line<'static>],
    exclusions: &[Vec<Range<u16>>],
) -> Vec<Vec<SourceSpan>> {
    let rendered = rendered_graphemes(lines, exclusions);
    let mut matches = vec![None; source.len()];
    let mut next_rendered = 0;
    for (source_index, grapheme) in source.iter().enumerate() {
        if grapheme.text.chars().all(char::is_whitespace) {
            continue;
        }
        let Some(offset) = rendered[next_rendered..]
            .iter()
            .position(|candidate| candidate.text == grapheme.text)
        else {
            continue;
        };
        let rendered_index = next_rendered + offset;
        matches[source_index] = Some(rendered_index);
        next_rendered = rendered_index + 1;
    }

    let mut line_start = 0;
    while line_start < source.len() {
        let line_end = source[line_start..]
            .iter()
            .position(|grapheme| grapheme.text == "\n")
            .map_or(source.len(), |offset| line_start + offset);
        let first_content = (line_start..line_end).find(|index| {
            !source[*index].text.chars().all(char::is_whitespace) && matches[*index].is_some()
        });
        if let Some(first_content) = first_content {
            let leading = first_content.saturating_sub(line_start);
            let first_rendered =
                matches[first_content].expect("matched content has a rendered cell");
            if leading <= first_rendered {
                let candidates = first_rendered - leading..first_rendered;
                let same_line = candidates
                    .clone()
                    .all(|index| rendered[index].line == rendered[first_rendered].line);
                let whitespace = candidates
                    .clone()
                    .all(|index| rendered[index].text.chars().all(char::is_whitespace));
                if same_line && whitespace {
                    for (source_index, rendered_index) in
                        (line_start..first_content).zip(candidates)
                    {
                        matches[source_index] = Some(rendered_index);
                    }
                }
            }
        }
        line_start = line_end.saturating_add(1);
    }

    for source_index in 1..source.len().saturating_sub(1) {
        if !source[source_index].text.chars().all(char::is_whitespace) {
            continue;
        }
        let Some(previous) = matches[..source_index].iter().rposition(Option::is_some) else {
            continue;
        };
        let Some(next_offset) = matches[source_index + 1..].iter().position(Option::is_some) else {
            continue;
        };
        let next = source_index + 1 + next_offset;
        let (Some(previous_rendered), Some(next_rendered)) = (matches[previous], matches[next])
        else {
            continue;
        };
        if next_rendered == previous_rendered + 2
            && rendered[previous_rendered + 1]
                .text
                .chars()
                .all(char::is_whitespace)
        {
            matches[source_index] = Some(previous_rendered + 1);
        }
    }

    let mut selections = vec![Vec::new(); lines.len()];
    for (grapheme, rendered_index) in source.iter().zip(matches) {
        let Some(rendered) = rendered_index.and_then(|index| rendered.get(index)) else {
            continue;
        };
        selections[rendered.line].push(SourceSpan {
            columns: rendered.column..rendered.column.saturating_add(rendered.width),
            source: grapheme.source.clone(),
        });
    }
    selections
}

fn rendered_graphemes(
    lines: &[Line<'static>],
    exclusions: &[Vec<Range<u16>>],
) -> Vec<RenderedGrapheme> {
    let mut rendered = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        let mut column = 0_u16;
        for span in &line.spans {
            for text in span.content.graphemes(true) {
                let width = u16::try_from(UnicodeWidthStr::width(text)).unwrap_or(u16::MAX);
                let excluded = exclusions
                    .get(line_index)
                    .is_some_and(|ranges| ranges.iter().any(|range| range.contains(&column)));
                if width > 0 && !excluded {
                    rendered.push(RenderedGrapheme {
                        line: line_index,
                        column,
                        text: text.to_owned(),
                        width,
                    });
                }
                column = column.saturating_add(width);
            }
        }
    }
    rendered
}

fn is_diff_language(language: &str) -> bool {
    let language = language.split_ascii_whitespace().next().unwrap_or_default();
    language.eq_ignore_ascii_case("diff") || language.eq_ignore_ascii_case("patch")
}

fn heading_style(level: HeadingLevel, theme: &Theme) -> Style {
    let color = match level {
        HeadingLevel::H1 => theme.thinking_max(),
        HeadingLevel::H2 => theme.accent(),
        HeadingLevel::H3 => theme.thinking_medium(),
        HeadingLevel::H4 => theme.thinking_high(),
        HeadingLevel::H5 => theme.thinking_xhigh(),
        HeadingLevel::H6 => theme.muted(),
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

pub(super) fn wrap_spans(
    spans: &[Span<'static>],
    width: u16,
    prefer_words: bool,
) -> Vec<Line<'static>> {
    wrap_spans_with_whitespace(spans, width, prefer_words, false)
}

fn wrap_spans_with_whitespace(
    spans: &[Span<'static>],
    width: u16,
    prefer_words: bool,
    preserve_whitespace: bool,
) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let graphemes = spans
        .iter()
        .flat_map(|span| {
            span.content
                .graphemes(true)
                .map(|text| StyledGrapheme {
                    text: text.to_owned(),
                    style: span.style,
                    link: None,
                    width: u16::try_from(UnicodeWidthStr::width(text)).unwrap_or(u16::MAX),
                    whitespace: text.chars().all(char::is_whitespace),
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    wrap_graphemes(&graphemes, width, prefer_words, preserve_whitespace)
        .into_iter()
        .map(|(line, _)| line)
        .collect()
}

fn wrap_tagged_spans(
    spans: &[TaggedSpan],
    width: u16,
    prefer_words: bool,
) -> Vec<(Line<'static>, Vec<LinkSpan>)> {
    if width == 0 {
        return Vec::new();
    }
    let graphemes = spans
        .iter()
        .flat_map(|tagged| {
            tagged
                .span
                .content
                .graphemes(true)
                .map(|text| StyledGrapheme {
                    text: text.to_owned(),
                    style: tagged.span.style,
                    link: tagged.link.as_ref().map(Arc::clone),
                    width: u16::try_from(UnicodeWidthStr::width(text)).unwrap_or(u16::MAX),
                    whitespace: text.chars().all(char::is_whitespace),
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    wrap_graphemes(&graphemes, width, prefer_words, false)
}

fn wrap_graphemes(
    graphemes: &[StyledGrapheme],
    width: u16,
    prefer_words: bool,
    preserve_whitespace: bool,
) -> Vec<(Line<'static>, Vec<LinkSpan>)> {
    if graphemes.is_empty() {
        return vec![(Line::default(), Vec::new())];
    }
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, grapheme) in graphemes.iter().enumerate() {
        if grapheme.text == "\n" {
            lines.extend(wrap_visual_line(
                &graphemes[start..index],
                width,
                prefer_words,
                preserve_whitespace,
            ));
            start = index + 1;
        }
    }
    if start < graphemes.len()
        || graphemes
            .last()
            .is_some_and(|grapheme| grapheme.text == "\n")
    {
        lines.extend(wrap_visual_line(
            &graphemes[start..],
            width,
            prefer_words,
            preserve_whitespace,
        ));
    }
    lines
}

fn wrap_visual_line(
    graphemes: &[StyledGrapheme],
    width: u16,
    prefer_words: bool,
    preserve_whitespace: bool,
) -> Vec<(Line<'static>, Vec<LinkSpan>)> {
    if graphemes.is_empty() {
        return vec![(Line::default(), Vec::new())];
    }

    let mut lines = Vec::new();
    let mut start = 0_usize;
    while start < graphemes.len() {
        if preserve_whitespace && start > 0 && graphemes[start].text == " " {
            start += 1;
        }
        if !preserve_whitespace {
            while start < graphemes.len() && graphemes[start].whitespace {
                start += 1;
            }
        }
        if start == graphemes.len() {
            break;
        }
        let mut end = start;
        let mut used = 0_u16;
        let mut word_break = None;
        let first_content = graphemes[start..]
            .iter()
            .position(|grapheme| !grapheme.whitespace)
            .map_or(start, |offset| start.saturating_add(offset));
        while end < graphemes.len() {
            let next = used.saturating_add(graphemes[end].width);
            if next > width && end > start {
                break;
            }
            if graphemes[end].whitespace && (!preserve_whitespace || end > first_content) {
                word_break = Some(end);
            }
            used = next;
            end += 1;
            if used >= width {
                break;
            }
        }
        let split = if prefer_words && end < graphemes.len() {
            word_break.filter(|&index| index > start).unwrap_or(end)
        } else {
            end
        };
        lines.push(graphemes_to_line(
            &graphemes[start..split],
            preserve_whitespace,
        ));
        start = split.max(start + 1);
    }
    lines
}

struct StyledGrapheme {
    text: String,
    style: Style,
    link: Option<Arc<str>>,
    width: u16,
    whitespace: bool,
}

fn graphemes_to_line(
    graphemes: &[StyledGrapheme],
    preserve_whitespace: bool,
) -> (Line<'static>, Vec<LinkSpan>) {
    let mut spans = Vec::<Span<'static>>::new();
    let mut links = Vec::<LinkSpan>::new();
    let mut column = 0_u16;
    let rendered_len = if preserve_whitespace {
        graphemes.len()
    } else {
        graphemes
            .iter()
            .rposition(|grapheme| !grapheme.whitespace)
            .map_or(0, |index| index.saturating_add(1))
    };
    for grapheme in &graphemes[..rendered_len] {
        if let Some(last) = spans.last_mut()
            && last.style == grapheme.style
        {
            last.content.to_mut().push_str(&grapheme.text);
        } else {
            spans.push(Span::styled(grapheme.text.clone(), grapheme.style));
        }
        if let Some(destination) = &grapheme.link
            && grapheme.width > 0
        {
            let end = column.saturating_add(grapheme.width);
            if let Some(last) = links.last_mut()
                && last.destination == *destination
                && last.end == column
            {
                last.end = end;
            } else {
                links.push(LinkSpan {
                    destination: Arc::clone(destination),
                    start: column,
                    end,
                });
            }
        }
        column = column.saturating_add(grapheme.width);
    }
    (Line::from(spans), links)
}

fn hard_wrap(text: &str, width: u16) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0_u16;
    for grapheme in text.graphemes(true) {
        let grapheme_width = u16::try_from(UnicodeWidthStr::width(grapheme)).unwrap_or(u16::MAX);
        if used.saturating_add(grapheme_width) > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        line.push_str(grapheme);
        used = used.saturating_add(grapheme_width);
    }
    lines.push(line);
    lines
}

fn code_block_header(language: Option<&str>, width: u16, theme: &Theme) -> Line<'static> {
    let border = Style::default().fg(theme.border());
    let Some(language) = language else {
        return Line::from(Span::styled(
            format!("╭{}╮", "─".repeat(usize::from(width.saturating_sub(2)))),
            border,
        ));
    };
    let available = width.saturating_sub(5);
    let language = truncate_graphemes(language, available);
    let language_width =
        u16::try_from(UnicodeWidthStr::width(language.as_str())).unwrap_or(u16::MAX);
    let fill = width.saturating_sub(language_width.saturating_add(5));
    Line::from(vec![
        Span::styled("╭─ ", border),
        Span::styled(
            language,
            Style::default()
                .fg(theme.accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {}╮", "─".repeat(usize::from(fill))), border),
    ])
}

fn truncate_graphemes(text: &str, width: u16) -> String {
    let mut rendered = String::new();
    let mut used = 0_u16;
    for grapheme in text.graphemes(true) {
        let grapheme_width = u16::try_from(UnicodeWidthStr::width(grapheme)).unwrap_or(u16::MAX);
        if used.saturating_add(grapheme_width) > width {
            break;
        }
        rendered.push_str(grapheme);
        used = used.saturating_add(grapheme_width);
    }
    rendered
}

fn render_table(
    rows: &[Vec<String>],
    header_rows: usize,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 || width < 4 {
        return Vec::new();
    }
    let border_cells = u16::try_from(columns.saturating_add(1)).unwrap_or(u16::MAX);
    let available = width.saturating_sub(border_cells);
    if available < u16::try_from(columns.saturating_mul(3)).unwrap_or(u16::MAX) {
        return render_stacked_table(rows, header_rows, width, theme);
    }
    let mut widths = vec![3_u16; columns];
    for row in rows {
        for (column, cell) in row.iter().enumerate() {
            widths[column] = widths[column]
                .max(u16::try_from(UnicodeWidthStr::width(cell.as_str())).unwrap_or(u16::MAX));
        }
    }
    while widths.iter().copied().sum::<u16>() > available {
        let Some((index, _)) = widths.iter().enumerate().max_by_key(|(_, width)| **width) else {
            break;
        };
        if widths[index] <= 3 {
            break;
        }
        widths[index] -= 1;
    }

    let mut lines = Vec::new();
    lines.push(table_rule('╭', '┬', '╮', &widths, theme));
    for (row_index, row) in rows.iter().enumerate() {
        let wrapped = widths
            .iter()
            .enumerate()
            .map(|(column, &cell_width)| {
                hard_wrap(row.get(column).map_or("", String::as_str), cell_width)
            })
            .collect::<Vec<_>>();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for line_index in 0..height {
            let mut spans = vec![Span::styled("│", Style::default().fg(theme.border()))];
            for (column, &cell_width) in widths.iter().enumerate() {
                let text = wrapped[column].get(line_index).map_or("", String::as_str);
                let padding = usize::from(cell_width).saturating_sub(UnicodeWidthStr::width(text));
                let style = if row_index < header_rows {
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text())
                };
                spans.push(Span::styled(
                    format!("{text}{}", " ".repeat(padding)),
                    style,
                ));
                spans.push(Span::styled("│", Style::default().fg(theme.border())));
            }
            lines.push(Line::from(spans));
        }
        if row_index + 1 < rows.len() {
            lines.push(table_rule('├', '┼', '┤', &widths, theme));
        }
    }
    lines.push(table_rule('╰', '┴', '╯', &widths, theme));
    lines
}

fn table_rule(
    left: char,
    middle: char,
    right: char,
    widths: &[u16],
    theme: &Theme,
) -> Line<'static> {
    let mut rule = left.to_string();
    for (index, width) in widths.iter().enumerate() {
        rule.push_str(&"─".repeat(usize::from(*width)));
        rule.push(if index + 1 == widths.len() {
            right
        } else {
            middle
        });
    }
    Line::from(Span::styled(rule, Style::default().fg(theme.border())))
}

fn render_stacked_table(
    rows: &[Vec<String>],
    header_rows: usize,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(headers) = rows.first() else {
        return Vec::new();
    };
    let mut lines = vec![Line::from(Span::styled(
        format!("╭{}╮", "─".repeat(usize::from(width.saturating_sub(2)))),
        Style::default().fg(theme.border()),
    ))];
    for row in rows.iter().skip(header_rows.max(1)) {
        for (column, value) in row.iter().enumerate() {
            let header = headers.get(column).map_or("Value", String::as_str);
            let content = format!("{header}: {value}");
            for line in hard_wrap(&content, width.saturating_sub(4).max(1)) {
                let padding = usize::from(width.saturating_sub(4))
                    .saturating_sub(UnicodeWidthStr::width(line.as_str()));
                lines.push(Line::from(vec![
                    Span::styled("│ ", Style::default().fg(theme.border())),
                    Span::styled(line, Style::default().fg(theme.text())),
                    Span::raw(" ".repeat(padding)),
                    Span::styled(" │", Style::default().fg(theme.border())),
                ]));
            }
        }
        if row.as_ptr() != rows.last().map_or(row.as_ptr(), Vec::as_ptr) {
            lines.push(Line::from(Span::styled(
                format!("├{}┤", "─".repeat(usize::from(width.saturating_sub(2)))),
                Style::default().fg(theme.border()),
            )));
        }
    }
    lines.push(Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(usize::from(width.saturating_sub(2)))),
        Style::default().fg(theme.border()),
    )));
    lines
}

#[cfg(test)]
mod tests {
    use super::{
        super::image::{Cache, MAX_IMAGE_HEIGHT},
        ImageState, Layout, render, render_cached,
    };
    use crate::tui::theme::Theme;
    use ratatui::style::{Color, Modifier};
    use std::{fs::File, path::Path, sync::Arc, time::Instant};

    fn write_png(path: &Path) {
        write_solid_png(path, 1, 1);
    }

    fn write_solid_png(path: &Path, width: u32, height: u32) {
        let file = File::create(path).unwrap();
        let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&[0xff, 0, 0].repeat((width * height) as usize))
            .unwrap();
    }

    fn render_inline_image_in(markdown: &str, width: u16, workspace: &Path) -> Layout {
        let mut images = Cache::with_inline_images(true);
        render_cached_until_ready(markdown, width, workspace, &mut images)
    }

    fn render_cached_until_ready(
        markdown: &str,
        width: u16,
        workspace: &Path,
        images: &mut Cache,
    ) -> Layout {
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let layout = render_cached(markdown, width, &Theme::default(), workspace, images);
            if layout.image_state != ImageState::Pending {
                return layout;
            }
            images.poll(Instant::now());
            assert!(Instant::now() < deadline, "image preparation timed out");
            std::thread::yield_now();
        }
    }

    #[test]
    fn requested_markdown_styles_are_applied() {
        let theme = Theme::default();
        let lines = render(
            "# Header\n\n`code` and [link](https://example.com)",
            80,
            &theme,
        )
        .lines;

        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Magenta));
        let code = lines[2]
            .spans
            .iter()
            .find(|span| span.content.contains("code"))
            .unwrap();
        assert_eq!(code.style.fg, Some(Color::Rgb(0xD7, 0xD7, 0xD7)));
        assert_eq!(code.style.bg, Some(Color::Rgb(0x26, 0x26, 0x26)));
        let link = lines[2]
            .spans
            .iter()
            .find(|span| span.content == "link")
            .unwrap();
        assert_eq!(link.style.fg, Some(Color::Blue));
        assert!(link.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn wrapped_links_retain_clickable_ranges() {
        let layout = render("[abcdefghij](https://example.com)", 5, &Theme::default());

        assert_eq!(layout.lines[0].to_string(), "abcde");
        assert_eq!(layout.lines[1].to_string(), "fghij");
        assert_eq!(layout.links[0].len(), 1);
        assert_eq!(
            layout.links[0][0].destination.as_ref(),
            "https://example.com"
        );
        assert_eq!((layout.links[0][0].start, layout.links[0][0].end), (0, 5));
        assert_eq!(layout.links[1].len(), 1);
        assert_eq!((layout.links[1][0].start, layout.links[1][0].end), (0, 5));
    }

    #[test]
    fn code_blocks_use_high_contrast_rounded_chrome() {
        let lines = render("```rust\npub fn main() {}\n```", 32, &Theme::default()).lines;

        assert_eq!(lines[0].to_string(), "╭─ rust ───────────────────────╮");
        assert_eq!(lines[1].to_string(), "│ pub fn main() {}             │");
        assert_eq!(lines[2].to_string(), "╰──────────────────────────────╯");
        let keyword = lines[1]
            .spans
            .iter()
            .find(|span| span.content == "pub")
            .expect("Rust keywords should be syntax-highlighted separately");
        assert_eq!(keyword.style.fg, Some(Color::Blue));
        assert!(lines[1].spans.iter().all(|span| span.style.bg.is_none()));
    }

    #[test]
    fn rust_keywords_types_and_parameters_use_distinct_terminal_colors() {
        let lines = render(
            "```rust\npub struct Widget;\npub fn choose(input: &str) { let value = if input.is_empty() { 1 } else { 2 }; }\n```",
            100,
            &Theme::default(),
        )
        .lines;
        let spans = lines
            .iter()
            .flat_map(|line| &line.spans)
            .collect::<Vec<_>>();
        let style = |token| {
            spans
                .iter()
                .find(|span| span.content == token)
                .unwrap_or_else(|| panic!("{token} should have its own syntax span"))
                .style
        };

        for keyword in ["pub", "struct", "fn", "let", "if"] {
            assert_eq!(style(keyword).fg, Some(Color::Blue));
        }
        assert_eq!(style("Widget").fg, Some(Color::Yellow));
        assert_eq!(style("input").fg, Some(Color::Reset));
        assert!(style("input").add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn code_selection_excludes_language_labels_and_borders() {
        let markdown = "```rust\nrust\n│ value\n```";
        let layout = render(markdown, 32, &Theme::default());
        let code_start = markdown.find("\nrust\n").unwrap() + 1;
        let pipe = markdown.find('│').unwrap();

        assert!(layout.selections[0].is_empty());
        assert!(
            layout.selections[1].iter().any(|span| {
                span.columns == (2..3) && span.source == (code_start..code_start + 1)
            })
        );
        assert!(
            layout.selections[2]
                .iter()
                .any(|span| span.columns == (2..3) && span.source == (pipe..pipe + '│'.len_utf8()))
        );
        assert!(
            layout.selections[2]
                .iter()
                .all(|span| !span.columns.contains(&0))
        );
    }

    #[test]
    fn fenced_languages_use_syntects_built_in_syntaxes() {
        let lines = render(
            "```javascript\nconst greeting = \"hello\";\n```",
            40,
            &Theme::default(),
        )
        .lines;
        let keyword = lines[1]
            .spans
            .iter()
            .find(|span| span.content == "const")
            .expect("JavaScript keywords should be syntax-highlighted separately");

        assert_eq!(keyword.style.fg, Some(Color::Blue));
    }

    #[test]
    fn diff_code_blocks_color_additions_and_deletions() {
        let lines = render(
            "```diff\n--- a/file.rs\n+++ b/file.rs\n-old value\n+new value\n context\n```",
            32,
            &Theme::default(),
        )
        .lines;
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
        assert_eq!(addition.style.bg, None);
        assert_eq!(deletion.style.bg, None);
    }

    #[test]
    fn diff_code_blocks_render_hunk_ranges_and_highlight_source() {
        let lines = render(
            "```diff\ndiff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -10,2 +10,3 @@ impl App\n-pub fn old() {}\n+pub fn new() {}\n+let value = 1;\n```",
            60,
            &Theme::default(),
        )
        .lines;
        let rendered = lines.iter().map(ToString::to_string).collect::<String>();
        let keyword = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == "fn")
            .expect("Rust keyword should be syntax-highlighted separately");

        assert!(rendered.contains("src/lib.rs"));
        assert!(rendered.contains("-10,2 → +10,3"));
        assert_eq!(keyword.style.fg, Some(Color::Blue));
    }

    #[test]
    fn tables_use_rounded_unicode_chrome() {
        let lines = render("| A | B |\n|---|---|\n| 1 | 2 |", 30, &Theme::default()).lines;
        let rendered = lines.iter().map(ToString::to_string).collect::<Vec<_>>();

        assert!(rendered.first().unwrap().starts_with('╭'));
        assert!(rendered.last().unwrap().starts_with('╰'));
        assert!(rendered.iter().any(|line| line.contains('┼')));
    }

    #[test]
    fn narrow_tables_fall_back_without_overflowing() {
        let lines = render(
            "| Header | Other |\n|---|---|\n| value | data |",
            8,
            &Theme::default(),
        )
        .lines;

        assert!(lines.iter().all(|line| line.width() <= 8));
        assert!(lines.first().unwrap().to_string().starts_with('╭'));
    }

    #[test]
    fn terminal_controls_are_sanitized() {
        let rendered = render("hello \u{1b}[31mred", 80, &Theme::default())
            .lines
            .into_iter()
            .map(|line| line.to_string())
            .collect::<String>();

        assert!(!rendered.contains('\u{1b}'));
        assert!(rendered.contains('�'));
    }

    #[test]
    fn renderable_inline_images_are_isolated_from_surrounding_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.png");
        write_png(&path);
        let layout =
            render_inline_image_in("before ![sample](sample.png) after", 80, directory.path());
        let lines = layout
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        assert_eq!(lines.first().map(|line| line.trim_end()), Some("before"));
        assert_eq!(lines.last().map(|line| line.trim_start()), Some("after"));
        assert!(lines.len() >= 3);
        assert_eq!(layout.images.len(), 1);
    }

    #[test]
    fn absolute_images_outside_the_workspace_are_rendered() {
        let workspace = tempfile::tempdir().unwrap();
        let scratchpad = tempfile::tempdir().unwrap();
        let path = scratchpad.path().join("sample.png");
        write_png(&path);

        let layout = render_inline_image_in(
            &format!("![sample]({})", path.display()),
            80,
            workspace.path(),
        );

        assert_eq!(layout.images.len(), 1);
    }

    #[test]
    fn image_failures_are_red_and_remain_inline() {
        let layout = render_inline_image_in(
            "before ![sample](/definitely/missing/image.png) after",
            80,
            Path::new("/workspace"),
        );

        assert_eq!(layout.lines.len(), 1);
        assert_eq!(
            layout.lines[0].to_string(),
            "before image could not be rendered after"
        );
        let error = layout.lines[0]
            .spans
            .iter()
            .find(|span| span.content == "image could not be rendered")
            .unwrap();
        assert_eq!(error.style.fg, Some(Color::Red));
    }

    #[test]
    fn unsupported_image_backends_render_images_as_markdown_links() {
        let mut images = Cache::with_inline_images(false);
        let layout = render_cached(
            "before ![sample](outside.png) after",
            80,
            &Theme::default(),
            Path::new("/workspace"),
            &mut images,
        );

        assert!(layout.images.is_empty());
        assert_eq!(
            layout.lines[0].to_string(),
            "before sample ↗ outside.png after"
        );
        assert_eq!(layout.links[0].len(), 1);
        assert_eq!(layout.links[0][0].destination.as_ref(), "outside.png");
        let label = layout.lines[0]
            .spans
            .iter()
            .find(|span| span.content == "sample")
            .unwrap();
        assert_eq!(label.style.fg, Some(Color::Blue));
        assert!(label.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn native_images_do_not_map_invisible_link_text_into_selection() {
        let directory = tempfile::tempdir().unwrap();
        write_png(&directory.path().join("after"));
        let markdown = "![sample](after) after";
        let destination = markdown.find("after").unwrap();
        let layout = render_inline_image_in(markdown, 80, directory.path());

        assert!(
            layout
                .selections
                .iter()
                .flatten()
                .all(|span| !span.source.contains(&destination))
        );
    }

    #[test]
    fn table_images_do_not_consume_later_fallback_selection_metadata() {
        let markdown = concat!(
            "| image |\n",
            "|---|\n",
            "| ![table](table.png) |\n\n",
            "![sample](outside.png)",
        );
        let destination = markdown.rfind("outside.png").unwrap();
        let mut images = Cache::with_inline_images(false);
        let layout = render_cached(
            markdown,
            80,
            &Theme::default(),
            Path::new("/workspace"),
            &mut images,
        );

        assert!(layout.selections.iter().flatten().any(|span| {
            span.source.start >= destination
                && span.source.end <= destination.saturating_add("outside.png".len())
        }));
    }

    #[test]
    fn streaming_markdown_reuses_unchanged_image_protocols() {
        let directory = tempfile::tempdir().unwrap();
        write_png(&directory.path().join("sample.png"));
        let mut images = Cache::with_inline_images(true);
        let first = render_cached_until_ready(
            "before ![sample](sample.png)",
            80,
            directory.path(),
            &mut images,
        );
        let second = render_cached_until_ready(
            "before ![sample](sample.png) after",
            80,
            directory.path(),
            &mut images,
        );

        assert!(Arc::ptr_eq(
            &first.images[0].protocol,
            &second.images[0].protocol
        ));
        let protocol = Arc::downgrade(&second.images[0].protocol);
        drop((first, second));
        assert!(protocol.upgrade().is_some());
    }

    #[test]
    fn tall_images_are_fitted_to_a_bounded_transcript_height() {
        let directory = tempfile::tempdir().unwrap();
        write_solid_png(&directory.path().join("tall.png"), 1, 1_000);

        let layout = render_inline_image_in("![tall](tall.png)", 80, directory.path());

        assert!(layout.images[0].protocol.size().height <= MAX_IMAGE_HEIGHT);
    }
}
