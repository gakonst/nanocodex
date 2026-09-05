// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Structured rendering for unified and `apply_patch` diffs.

use crate::tui::theme::Theme;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use syntect::{easy::HighlightLines, highlighting::Theme as SyntaxTheme, parsing::SyntaxSet};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) fn render(source: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }

    let files = parse(source);
    let syntax_theme = super::highlight::theme();
    let mut rendered = Vec::new();
    for file in files {
        if !rendered.is_empty() {
            rendered.push(Line::default());
        }
        render_file(&mut rendered, &file, width, theme, &syntax_theme);
    }
    rendered
}

#[derive(Default)]
struct FileDiff {
    path: String,
    hunks: Vec<Hunk>,
}

struct Hunk {
    heading: String,
    lines: Vec<DiffLine>,
    next_old_line: Option<u64>,
    next_new_line: Option<u64>,
    relative_lines: bool,
}

impl Default for Hunk {
    fn default() -> Self {
        Self {
            heading: String::new(),
            lines: Vec::new(),
            next_old_line: Some(1),
            next_new_line: Some(1),
            relative_lines: true,
        }
    }
}

struct DiffLine {
    kind: DiffLineKind,
    text: String,
    old_line: Option<u64>,
    new_line: Option<u64>,
}

#[derive(Clone, Copy)]
enum DiffLineKind {
    Context,
    Addition,
    Deletion,
}

fn parse(source: &str) -> Vec<FileDiff> {
    let mut files = Vec::<FileDiff>::new();
    let mut current_file = None;
    let mut current_hunk = None;
    let mut old_path = None;
    let mut apply_patch_format = false;

    for line in source.trim_end_matches('\n').lines() {
        if line == "*** Begin Patch" {
            apply_patch_format = true;
            continue;
        }
        if matches!(line, "*** End Patch" | "*** End of File") {
            continue;
        }
        if let Some(path) = git_path(line) {
            current_file = Some(push_file(&mut files, path));
            current_hunk = None;
            old_path = None;
            continue;
        }
        if let Some(path) = patch_path(line) {
            apply_patch_format = true;
            current_file = Some(push_file(&mut files, path));
            current_hunk = None;
            old_path = None;
            continue;
        }
        if !apply_patch_format && let Some(path) = line.strip_prefix("--- ") {
            if current_file.is_some_and(|file| !files[file].hunks.is_empty()) {
                current_file = None;
                current_hunk = None;
            }
            old_path = clean_path(path);
            continue;
        }
        if !apply_patch_format && let Some(path) = line.strip_prefix("+++ ") {
            let path = clean_path(path).or_else(|| old_path.take());
            if current_file.is_none()
                && let Some(path) = path
            {
                current_file = Some(push_file(&mut files, path));
            }
            continue;
        }
        if line.starts_with("@@") {
            let file = ensure_file(&mut files, &mut current_file);
            let (old_start, new_start) = hunk_starts(line);
            let relative_lines = old_start.is_none() && new_start.is_none();
            files[file].hunks.push(Hunk {
                heading: parse_hunk_heading(line),
                lines: Vec::new(),
                next_old_line: old_start.or(relative_lines.then_some(1)),
                next_new_line: new_start.or(relative_lines.then_some(1)),
                relative_lines,
            });
            current_hunk = Some(files[file].hunks.len() - 1);
            continue;
        }
        if is_diff_metadata(line) {
            continue;
        }

        let Some((kind, text)) = diff_line(line) else {
            continue;
        };
        let file = ensure_file(&mut files, &mut current_file);
        let hunk = current_hunk.unwrap_or_else(|| {
            files[file].hunks.push(Hunk::default());
            files[file].hunks.len() - 1
        });
        current_hunk = Some(hunk);
        let hunk = &mut files[file].hunks[hunk];
        let (old_line, new_line) = hunk.take_line_numbers(kind);
        hunk.lines.push(DiffLine {
            kind,
            text: text.to_owned(),
            old_line,
            new_line,
        });
    }

    if files.is_empty() {
        files.push(FileDiff {
            path: "diff".to_owned(),
            hunks: vec![Hunk::default()],
        });
    }
    files.retain(|file| !file.hunks.is_empty());
    files
}

fn push_file(files: &mut Vec<FileDiff>, path: String) -> usize {
    files.push(FileDiff {
        path,
        hunks: Vec::new(),
    });
    files.len() - 1
}

fn ensure_file(files: &mut Vec<FileDiff>, current: &mut Option<usize>) -> usize {
    if let Some(index) = *current {
        return index;
    }
    let index = push_file(files, "diff".to_owned());
    *current = Some(index);
    index
}

fn git_path(line: &str) -> Option<String> {
    line.strip_prefix("diff --git ")
        .and_then(|paths| paths.split_ascii_whitespace().nth(1))
        .and_then(clean_path)
}

fn patch_path(line: &str) -> Option<String> {
    [
        "*** Add File: ",
        "*** Update File: ",
        "*** Delete File: ",
        "*** Move to: ",
    ]
    .into_iter()
    .find_map(|prefix| line.strip_prefix(prefix))
    .map(str::to_owned)
}

fn clean_path(path: &str) -> Option<String> {
    let path = path.trim().split_ascii_whitespace().next()?;
    if path == "/dev/null" {
        return None;
    }
    Some(
        path.strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))
            .unwrap_or(path)
            .to_owned(),
    )
}

fn parse_hunk_heading(line: &str) -> String {
    let body = line.trim_start_matches('@').trim();
    let (ranges, context) = body
        .split_once("@@")
        .map_or((body, ""), |(ranges, context)| {
            (ranges.trim(), context.trim())
        });
    let mut tokens = ranges.split_ascii_whitespace();
    let old = tokens.next().filter(|range| range.starts_with('-'));
    let new = tokens.next().filter(|range| range.starts_with('+'));
    match (old, new, context.is_empty()) {
        (Some(old), Some(new), true) => format!("{old} → {new}"),
        (Some(old), Some(new), false) => format!("{old} → {new} · {context}"),
        _ if !body.is_empty() => body.to_owned(),
        _ => String::new(),
    }
}

fn hunk_starts(line: &str) -> (Option<u64>, Option<u64>) {
    let body = line.trim_start_matches('@').trim();
    let ranges = body.split_once("@@").map_or(body, |(ranges, _)| ranges);
    let mut tokens = ranges.split_ascii_whitespace();
    let old = tokens.next().and_then(|range| range_start(range, '-'));
    let new = tokens.next().and_then(|range| range_start(range, '+'));
    (old, new)
}

fn range_start(range: &str, prefix: char) -> Option<u64> {
    range.strip_prefix(prefix)?.split(',').next()?.parse().ok()
}

impl Hunk {
    fn take_line_numbers(&mut self, kind: DiffLineKind) -> (Option<u64>, Option<u64>) {
        let old_line = (!matches!(kind, DiffLineKind::Addition))
            .then_some(self.next_old_line)
            .flatten();
        let new_line = (!matches!(kind, DiffLineKind::Deletion))
            .then_some(self.next_new_line)
            .flatten();
        if !matches!(kind, DiffLineKind::Addition) {
            self.next_old_line = self.next_old_line.map(|line| line.saturating_add(1));
        }
        if !matches!(kind, DiffLineKind::Deletion) {
            self.next_new_line = self.next_new_line.map(|line| line.saturating_add(1));
        }
        (old_line, new_line)
    }
}

fn diff_line(line: &str) -> Option<(DiffLineKind, &str)> {
    if let Some(line) = line.strip_prefix('+') {
        return Some((DiffLineKind::Addition, line));
    }
    if let Some(line) = line.strip_prefix('-') {
        return Some((DiffLineKind::Deletion, line));
    }
    if let Some(line) = line.strip_prefix(' ') {
        return Some((DiffLineKind::Context, line));
    }
    if line.starts_with("\\ No newline at end of file") {
        return None;
    }
    Some((DiffLineKind::Context, line))
}

fn is_diff_metadata(line: &str) -> bool {
    [
        "index ",
        "new file mode ",
        "deleted file mode ",
        "old mode ",
        "new mode ",
        "similarity index ",
        "dissimilarity index ",
        "rename from ",
        "rename to ",
        "copy from ",
        "copy to ",
        "Binary files ",
    ]
    .into_iter()
    .any(|prefix| line.starts_with(prefix))
}

fn render_file(
    rendered: &mut Vec<Line<'static>>,
    file: &FileDiff,
    width: u16,
    theme: &Theme,
    syntax_theme: &SyntaxTheme,
) {
    let (additions, deletions) = change_counts(file.hunks.iter().flat_map(|hunk| &hunk.lines));
    let label = format!("{} · +{additions} −{deletions}", file.path);

    let line_number_width = line_number_width(file);
    let body_overhead = line_number_width.saturating_mul(2).saturating_add(9);
    if width <= body_overhead {
        render_narrow_file(rendered, &label, file, width, theme);
        return;
    }

    rendered.push(component_header(&label, width, theme));
    let assets = super::highlight::assets();
    let syntax = super::highlight::syntax_for_path(&assets.syntaxes, &file.path);
    let code_width = width.saturating_sub(body_overhead).max(1);
    for hunk in &file.hunks {
        rendered.push(hunk_divider(&hunk_label(hunk), width, theme));
        let mut old_highlighter = HighlightLines::new(syntax, syntax_theme);
        let mut new_highlighter = HighlightLines::new(syntax, syntax_theme);
        for line in &hunk.lines {
            let spans = highlighted_diff_line(
                line,
                &mut old_highlighter,
                &mut new_highlighter,
                &assets.syntaxes,
            );
            for (index, code) in super::highlight::wrap(spans, code_width)
                .into_iter()
                .enumerate()
            {
                rendered.push(component_body(
                    line,
                    code,
                    index,
                    line_number_width,
                    code_width,
                    theme,
                ));
            }
        }
    }
    rendered.push(Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(usize::from(width.saturating_sub(2)))),
        Style::default().fg(theme.border()),
    )));
}

fn line_number_width(file: &FileDiff) -> u16 {
    file.hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .flat_map(|line| [line.old_line, line.new_line])
        .flatten()
        .map(|line| line.to_string().len())
        .max()
        .and_then(|width| u16::try_from(width).ok())
        .unwrap_or(1)
}

fn render_narrow_file(
    rendered: &mut Vec<Line<'static>>,
    label: &str,
    file: &FileDiff,
    width: u16,
    theme: &Theme,
) {
    rendered.push(Line::from(Span::styled(
        truncate(label, width),
        Style::default()
            .fg(theme.accent())
            .add_modifier(Modifier::BOLD),
    )));
    let code_width = width.saturating_sub(1);
    for hunk in &file.hunks {
        rendered.push(Line::from(Span::styled(
            truncate(&hunk_label(hunk), width),
            Style::default().fg(theme.muted()),
        )));
        for line in &hunk.lines {
            if code_width == 0 {
                rendered.push(Line::from(Span::styled(
                    &marker(line.kind)[..1],
                    marker_style(line.kind, theme),
                )));
                continue;
            }
            for text in hard_wrap(&line.text, code_width) {
                rendered.push(Line::from(vec![
                    Span::styled(&marker(line.kind)[..1], marker_style(line.kind, theme)),
                    Span::styled(text, super::highlight::code_style()),
                ]));
            }
        }
    }
}

fn hunk_label(hunk: &Hunk) -> String {
    let (additions, deletions) = change_counts(hunk.lines.iter());
    let heading = if hunk.heading.is_empty() {
        "changes"
    } else {
        &hunk.heading
    };
    let positions = if hunk.relative_lines {
        " · relative lines"
    } else {
        ""
    };
    format!("{heading}{positions} · +{additions} −{deletions}")
}

fn change_counts<'a>(lines: impl Iterator<Item = &'a DiffLine>) -> (usize, usize) {
    lines.fold((0, 0), |(additions, deletions), line| match line.kind {
        DiffLineKind::Addition => (additions + 1, deletions),
        DiffLineKind::Deletion => (additions, deletions + 1),
        DiffLineKind::Context => (additions, deletions),
    })
}

fn component_header(label: &str, width: u16, theme: &Theme) -> Line<'static> {
    let label = truncate(label, width.saturating_sub(5));
    let label_width = u16::try_from(UnicodeWidthStr::width(label.as_str())).unwrap_or(u16::MAX);
    let fill = width.saturating_sub(label_width.saturating_add(5));
    Line::from(vec![
        Span::styled("╭─ ", Style::default().fg(theme.border())),
        Span::styled(
            label,
            Style::default()
                .fg(theme.accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {}╮", "─".repeat(usize::from(fill))),
            Style::default().fg(theme.border()),
        ),
    ])
}

fn hunk_divider(label: &str, width: u16, theme: &Theme) -> Line<'static> {
    let label = truncate(label, width.saturating_sub(5));
    let label_width = u16::try_from(UnicodeWidthStr::width(label.as_str())).unwrap_or(u16::MAX);
    let fill = width.saturating_sub(label_width.saturating_add(5));
    Line::from(vec![
        Span::styled("├─ ", Style::default().fg(theme.border())),
        Span::styled(label, Style::default().fg(theme.muted())),
        Span::styled(
            format!(" {}┤", "─".repeat(usize::from(fill))),
            Style::default().fg(theme.border()),
        ),
    ])
}

fn component_body(
    line: &DiffLine,
    code: Vec<Span<'static>>,
    wrap_index: usize,
    line_number_width: u16,
    code_width: u16,
    theme: &Theme,
) -> Line<'static> {
    let used = code.iter().map(|span| span.width()).sum::<usize>();
    let padding = usize::from(code_width).saturating_sub(used);
    let mut spans = Vec::with_capacity(code.len() + 7);
    spans.push(Span::styled("│", Style::default().fg(theme.border())));
    spans.push(Span::styled(
        line_number_gutter(line, wrap_index, line_number_width),
        Style::default().fg(theme.muted()),
    ));
    spans.push(Span::styled("│", Style::default().fg(theme.border())));
    if wrap_index == 0 {
        spans.push(Span::styled(
            marker(line.kind),
            marker_style(line.kind, theme),
        ));
    } else {
        spans.push(Span::styled("↪ ", Style::default().fg(theme.muted())));
    }
    spans.extend(code);
    spans.push(Span::styled(
        " ".repeat(padding),
        super::highlight::code_style(),
    ));
    spans.push(Span::styled(" │", Style::default().fg(theme.border())));
    Line::from(spans)
}

fn line_number_gutter(line: &DiffLine, wrap_index: usize, width: u16) -> String {
    let width = usize::from(width);
    if wrap_index > 0 {
        return format!(" {:width$} {:width$} ", "", "");
    }
    let old = line
        .old_line
        .map(|line| line.to_string())
        .unwrap_or_default();
    let new = line
        .new_line
        .map(|line| line.to_string())
        .unwrap_or_default();
    format!(" {old:>width$} {new:>width$} ")
}

fn marker(kind: DiffLineKind) -> &'static str {
    match kind {
        DiffLineKind::Context => "  ",
        DiffLineKind::Addition => "+ ",
        DiffLineKind::Deletion => "- ",
    }
}

fn marker_style(kind: DiffLineKind, theme: &Theme) -> Style {
    let color = match kind {
        DiffLineKind::Context => theme.muted(),
        DiffLineKind::Addition => Color::Green,
        DiffLineKind::Deletion => Color::Red,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn highlighted_diff_line(
    line: &DiffLine,
    old: &mut HighlightLines<'_>,
    new: &mut HighlightLines<'_>,
    syntaxes: &SyntaxSet,
) -> Vec<Span<'static>> {
    match line.kind {
        DiffLineKind::Context => {
            let highlighted = super::highlight::line(old, &line.text, syntaxes);
            let _ = super::highlight::line(new, &line.text, syntaxes);
            highlighted
        }
        DiffLineKind::Addition => super::highlight::line(new, &line.text, syntaxes),
        DiffLineKind::Deletion => super::highlight::line(old, &line.text, syntaxes),
    }
}

fn hard_wrap(text: &str, width: u16) -> Vec<String> {
    let spans = super::highlight::wrap(vec![Span::raw(text.to_owned())], width);
    spans
        .into_iter()
        .map(|line| line.into_iter().map(|span| span.content).collect())
        .collect()
}

fn truncate(text: &str, width: u16) -> String {
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

#[cfg(test)]
mod tests {
    use super::render;
    use crate::tui::theme::Theme;
    use ratatui::style::Color;

    const TWO_HUNKS: &str = "diff --git a/src/lib.rs b/src/lib.rs\n\
--- a/src/lib.rs\n\
+++ b/src/lib.rs\n\
@@ -1 +1 @@ first\n\
-old_one();\n\
+new_one();\n\
@@ -10 +10 @@ second\n\
-old_two();\n\
+new_two();\n";

    #[test]
    fn file_component_joins_hunks_with_labeled_dividers() {
        let lines = render(TWO_HUNKS, 50, &Theme::default());
        let rendered = lines.iter().map(ToString::to_string).collect::<Vec<_>>();

        assert!(rendered[0].starts_with("╭─ src/lib.rs · +2 −2"));
        assert_eq!(
            rendered.iter().filter(|line| line.starts_with('╭')).count(),
            1
        );
        assert_eq!(
            rendered.iter().filter(|line| line.starts_with('├')).count(),
            2
        );
        assert!(rendered.iter().any(|line| line.contains("-1 → +1 · first")));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("-10 → +10 · second"))
        );
        assert!(rendered.last().is_some_and(|line| line.starts_with('╰')));
        assert!(lines.iter().all(|line| line.width() == 50));
    }

    #[test]
    fn body_uses_old_and_new_line_number_gutters() {
        let source = "--- a/lib.rs\n+++ b/lib.rs\n@@ -9,2 +19,2 @@\n context\n-old\n+new\n";
        let rendered = render(source, 40, &Theme::default())
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        assert!(
            rendered
                .iter()
                .any(|line| line.contains("│  9 19 │  context"))
        );
        assert!(rendered.iter().any(|line| line.contains("│ 10    │- old")));
        assert!(rendered.iter().any(|line| line.contains("│    20 │+ new")));
    }

    #[test]
    fn positionless_patch_hunks_use_labeled_relative_line_numbers() {
        let source =
            "*** Begin Patch\n*** Update File: lib.rs\n@@\n context\n-old\n+new\n*** End Patch";
        let rendered = render(source, 48, &Theme::default())
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        assert!(
            rendered
                .iter()
                .any(|line| line.contains("changes · relative lines · +1 −1"))
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("│ 1 1 │  context"))
        );
        assert!(rendered.iter().any(|line| line.contains("│ 2   │- old")));
        assert!(rendered.iter().any(|line| line.contains("│   2 │+ new")));
    }

    #[test]
    fn separate_files_render_as_separate_components() {
        let source = "--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-old\n+new\n\
                      --- a/two.rs\n+++ b/two.rs\n@@ -2 +2 @@\n-old\n+new\n";
        let source = source.replace("                      ", "");
        let lines = render(&source, 40, &Theme::default());

        assert_eq!(
            lines
                .iter()
                .filter(|line| line.to_string().starts_with('╭'))
                .count(),
            2
        );
        assert!(lines.iter().any(|line| line.to_string().contains("one.rs")));
        assert!(lines.iter().any(|line| line.to_string().contains("two.rs")));
    }

    #[test]
    fn syntax_colors_ignore_configured_hex_colors() {
        let theme = toml::from_str::<Theme>(
            "mode = \"dark\"\naccent = \"#123456\"\ncode_background = \"#010203\"\n",
        )
        .unwrap();
        let lines = render(
            "--- a/lib.rs\n+++ b/lib.rs\n@@ -1 +1 @@\n-pub fn old() {}\n+pub fn new() {}\n",
            40,
            &theme,
        );
        let keyword = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == "pub")
            .expect("Rust keyword should be highlighted");

        assert_eq!(keyword.style.fg, Some(Color::Blue));
        assert_eq!(keyword.style.bg, None);
    }

    #[test]
    fn narrow_diff_rendering_never_overflows() {
        let wide = "--- a/lib.rs\n+++ b/lib.rs\n@@ -1 +1 @@\n-old\n+😀\n";
        for width in 1..=12 {
            let lines = render(TWO_HUNKS, width, &Theme::default());
            assert!(!lines.is_empty());
            assert!(lines.iter().all(|line| line.width() <= usize::from(width)));
            assert!(
                render(wide, width, &Theme::default())
                    .iter()
                    .all(|line| line.width() <= usize::from(width))
            );
        }
    }

    #[test]
    fn apply_patch_content_that_looks_like_headers_stays_in_its_file() {
        let lines = render(
            "*** Begin Patch\n*** Add File: src/lib.rs\n+++value\n---value\n*** End Patch",
            40,
            &Theme::default(),
        );
        let rendered = lines.iter().map(ToString::to_string).collect::<String>();

        assert!(rendered.contains("src/lib.rs"));
        assert!(rendered.contains("+ ++value"));
        assert!(rendered.contains("- --value"));
    }
}
