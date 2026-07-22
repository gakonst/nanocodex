use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

use super::markdown::highlighted_code_lines;

const TAB_REPLACEMENT: &str = "    ";
const TAB_WIDTH: usize = TAB_REPLACEMENT.len();
const ADD_BACKGROUND: Color = Color::Rgb(33, 58, 43);
const DELETE_BACKGROUND: Color = Color::Rgb(74, 34, 29);

#[derive(Clone)]
pub(super) struct PatchPresentation {
    pub(super) summary: String,
    files: Vec<FilePatch>,
}

#[derive(Clone, Copy)]
enum ChangeKind {
    Add,
    Update,
    Delete,
}

#[derive(Clone)]
struct FilePatch {
    kind: ChangeKind,
    path: String,
    move_to: Option<String>,
    body: Vec<String>,
    added: usize,
    removed: usize,
}

#[derive(Clone, Copy)]
enum DiffLineKind {
    Insert,
    Delete,
    Context,
}

pub(super) fn present_apply_patch(source: &str) -> Option<PatchPresentation> {
    let mut files = parse_apply_patch(source);
    if files.is_empty() {
        return None;
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));

    let total_added = files.iter().map(|file| file.added).sum::<usize>();
    let total_removed = files.iter().map(|file| file.removed).sum::<usize>();
    let summary = if let [file] = files.as_slice() {
        format!(
            "{} {} {}",
            change_verb(file.kind),
            display_path(file),
            line_counts(file.added, file.removed)
        )
    } else {
        format!(
            "Edited {} files {}",
            files.len(),
            line_counts(total_added, total_removed)
        )
    };

    Some(PatchPresentation { summary, files })
}

impl PatchPresentation {
    pub(super) fn lines(&self, width: u16) -> Vec<Line<'static>> {
        let total_added = self.files.iter().map(|file| file.added).sum::<usize>();
        let total_removed = self.files.iter().map(|file| file.removed).sum::<usize>();
        let file_count = self.files.len();
        let mut lines = Vec::new();

        let mut header = vec![Span::styled(
            "• ",
            Style::default().add_modifier(Modifier::DIM),
        )];
        if let [file] = self.files.as_slice() {
            header.push(Span::styled(
                change_verb(file.kind),
                Style::default().add_modifier(Modifier::BOLD),
            ));
            header.push(Span::raw(" "));
            header.push(Span::raw(display_path(file)));
            header.push(Span::raw(" "));
            header.extend(line_count_spans(file.added, file.removed));
        } else {
            header.push(Span::styled(
                "Edited",
                Style::default().add_modifier(Modifier::BOLD),
            ));
            header.push(Span::raw(format!(" {file_count} files ")));
            header.extend(line_count_spans(total_added, total_removed));
        }
        lines.push(Line::from(header));

        for (index, file) in self.files.iter().enumerate() {
            if index > 0 {
                lines.push(Line::raw(""));
            }
            if file_count > 1 {
                let mut file_header = vec![Span::styled(
                    "  └ ",
                    Style::default().add_modifier(Modifier::DIM),
                )];
                file_header.push(Span::raw(display_path(file)));
                file_header.push(Span::raw(" "));
                file_header.extend(line_count_spans(file.added, file.removed));
                lines.push(Line::from(file_header));
            }
            lines.extend(render_body(
                file,
                usize::from(width).saturating_sub(4).max(1),
            ));
        }
        lines
    }
}

fn parse_apply_patch(source: &str) -> Vec<FilePatch> {
    let mut files = Vec::new();
    let mut current: Option<FilePatch> = None;

    for line in source.lines() {
        let next = [
            ("*** Add File: ", ChangeKind::Add),
            ("*** Update File: ", ChangeKind::Update),
            ("*** Delete File: ", ChangeKind::Delete),
        ]
        .into_iter()
        .find_map(|(prefix, kind)| line.strip_prefix(prefix).map(|path| (kind, path)));
        if let Some((kind, path)) = next {
            if let Some(file) = current.take() {
                files.push(file);
            }
            current = Some(FilePatch {
                kind,
                path: path.to_owned(),
                move_to: None,
                body: Vec::new(),
                added: 0,
                removed: 0,
            });
            continue;
        }
        let Some(file) = current.as_mut() else {
            continue;
        };
        if let Some(path) = line.strip_prefix("*** Move to: ") {
            file.move_to = Some(path.to_owned());
        } else if line == "*** End Patch" {
            if let Some(file) = current.take() {
                files.push(file);
            }
        } else if line != "*** End of File" {
            if line.starts_with('+') {
                file.added += 1;
            } else if line.starts_with('-') {
                file.removed += 1;
            }
            file.body.push(line.to_owned());
        }
    }
    if let Some(file) = current {
        files.push(file);
    }
    files
}

fn render_body(file: &FilePatch, width: usize) -> Vec<Line<'static>> {
    let language = file
        .move_to
        .as_deref()
        .unwrap_or(&file.path)
        .rsplit_once('.')
        .map(|(_, extension)| extension);
    let content = file
        .body
        .iter()
        .filter_map(|line| diff_content(line).map(|(_, content)| content))
        .collect::<Vec<_>>()
        .join("\n");
    let highlighted = highlighted_code_lines(language, &content);
    let mut highlighted = highlighted.into_iter();
    let max_line_number = maximum_line_number(&file.body);
    let line_number_width = max_line_number.max(1).to_string().len();
    let mut old_line = 1_usize;
    let mut new_line = 1_usize;
    let mut first_hunk = true;
    let mut output = Vec::new();

    for raw in &file.body {
        if raw.starts_with("@@") {
            if !first_hunk {
                output.push(prefix_line(Line::from(vec![
                    Span::styled(
                        format!("{:line_number_width$} ", ""),
                        Style::default().add_modifier(Modifier::DIM),
                    ),
                    Span::styled("⋮", Style::default().add_modifier(Modifier::DIM)),
                ])));
            }
            first_hunk = false;
            if let Some((old_start, new_start)) = parse_hunk_starts(raw) {
                old_line = old_start;
                new_line = new_start;
            }
            continue;
        }
        let Some((kind, content)) = diff_content(raw) else {
            continue;
        };
        let syntax = highlighted
            .next()
            .map(|line| line.spans)
            .unwrap_or_default();
        let syntax = if syntax.is_empty() {
            vec![Span::raw(content.to_owned())]
        } else {
            syntax
        };
        let number = match kind {
            DiffLineKind::Insert => {
                let number = new_line;
                new_line = new_line.saturating_add(1);
                number
            }
            DiffLineKind::Delete => {
                let number = old_line;
                old_line = old_line.saturating_add(1);
                number
            }
            DiffLineKind::Context => {
                let number = new_line;
                old_line = old_line.saturating_add(1);
                new_line = new_line.saturating_add(1);
                number
            }
        };
        output.extend(
            wrap_diff_line(number, kind, &syntax, width, line_number_width)
                .into_iter()
                .map(prefix_line),
        );
    }
    output
}

fn maximum_line_number(lines: &[String]) -> usize {
    let mut old_line = 1_usize;
    let mut new_line = 1_usize;
    let mut maximum = 1_usize;
    for raw in lines {
        if raw.starts_with("@@") {
            if let Some((old_start, new_start)) = parse_hunk_starts(raw) {
                old_line = old_start;
                new_line = new_start;
            }
            continue;
        }
        match diff_content(raw).map(|(kind, _)| kind) {
            Some(DiffLineKind::Insert) => {
                maximum = maximum.max(new_line);
                new_line = new_line.saturating_add(1);
            }
            Some(DiffLineKind::Delete) => {
                maximum = maximum.max(old_line);
                old_line = old_line.saturating_add(1);
            }
            Some(DiffLineKind::Context) => {
                maximum = maximum.max(new_line);
                old_line = old_line.saturating_add(1);
                new_line = new_line.saturating_add(1);
            }
            None => {}
        }
    }
    maximum
}

fn diff_content(line: &str) -> Option<(DiffLineKind, &str)> {
    if let Some(content) = line.strip_prefix('+') {
        Some((DiffLineKind::Insert, content))
    } else if let Some(content) = line.strip_prefix('-') {
        Some((DiffLineKind::Delete, content))
    } else {
        line.strip_prefix(' ')
            .map(|content| (DiffLineKind::Context, content))
    }
}

fn parse_hunk_starts(header: &str) -> Option<(usize, usize)> {
    let mut ranges = header.strip_prefix("@@ -")?.split_whitespace();
    let old = ranges.next()?.split(',').next()?.parse().ok()?;
    let new = ranges
        .next()?
        .strip_prefix('+')?
        .split(',')
        .next()?
        .parse()
        .ok()?;
    Some((old, new))
}

fn wrap_diff_line(
    line_number: usize,
    kind: DiffLineKind,
    spans: &[Span<'static>],
    width: usize,
    line_number_width: usize,
) -> Vec<Line<'static>> {
    let prefix_columns = line_number_width.max(1) + 2;
    let available = width.saturating_sub(prefix_columns).max(1);
    let chunks = wrap_styled_spans(spans, available);
    let (sign, sign_style, line_style) = match kind {
        DiffLineKind::Insert => (
            '+',
            Style::default().fg(Color::Green),
            Style::default().bg(ADD_BACKGROUND),
        ),
        DiffLineKind::Delete => (
            '-',
            Style::default().fg(Color::Red),
            Style::default().bg(DELETE_BACKGROUND),
        ),
        DiffLineKind::Context => (' ', Style::default(), Style::default()),
    };

    chunks
        .into_iter()
        .enumerate()
        .map(|(index, mut chunk)| {
            let mut row = if index == 0 {
                vec![
                    Span::styled(
                        format!("{line_number:>line_number_width$} "),
                        Style::default().add_modifier(Modifier::DIM),
                    ),
                    Span::styled(sign.to_string(), sign_style),
                ]
            } else {
                vec![Span::styled(
                    format!("{:line_number_width$}  ", ""),
                    Style::default().add_modifier(Modifier::DIM),
                )]
            };
            if matches!(kind, DiffLineKind::Delete) {
                for span in &mut chunk {
                    span.style = span.style.add_modifier(Modifier::DIM);
                }
            }
            row.extend(chunk);
            Line::from(row).style(line_style)
        })
        .collect()
}

fn wrap_styled_spans(spans: &[Span<'static>], max_columns: usize) -> Vec<Vec<Span<'static>>> {
    let mut result = Vec::new();
    let mut current_line = Vec::new();
    let mut column = 0_usize;

    for span in spans {
        let style = span.style;
        let mut remaining = span.content.as_ref();
        while !remaining.is_empty() {
            let mut byte_end = 0_usize;
            let mut chunk_columns = 0_usize;
            for character in remaining.chars() {
                let width =
                    character
                        .width()
                        .unwrap_or(if character == '\t' { TAB_WIDTH } else { 0 });
                if column.saturating_add(chunk_columns).saturating_add(width) > max_columns {
                    break;
                }
                byte_end = byte_end.saturating_add(character.len_utf8());
                chunk_columns = chunk_columns.saturating_add(width);
            }
            if byte_end == 0 {
                if !current_line.is_empty() {
                    result.push(std::mem::take(&mut current_line));
                }
                let Some(character) = remaining.chars().next() else {
                    break;
                };
                let length = character.len_utf8();
                current_line.push(Span::styled(
                    remaining[..length].replace('\t', TAB_REPLACEMENT),
                    style,
                ));
                column = character
                    .width()
                    .unwrap_or(if character == '\t' { TAB_WIDTH } else { 1 });
                remaining = &remaining[length..];
                continue;
            }
            let (chunk, rest) = remaining.split_at(byte_end);
            current_line.push(Span::styled(chunk.replace('\t', TAB_REPLACEMENT), style));
            column = column.saturating_add(chunk_columns);
            remaining = rest;
            if column >= max_columns {
                result.push(std::mem::take(&mut current_line));
                column = 0;
            }
        }
    }
    if !current_line.is_empty() || result.is_empty() {
        result.push(current_line);
    }
    result
}

fn prefix_line(mut line: Line<'static>) -> Line<'static> {
    line.spans.insert(0, Span::raw("    "));
    line
}

fn display_path(file: &FilePatch) -> String {
    file.move_to.as_ref().map_or_else(
        || file.path.clone(),
        |path| format!("{} → {path}", file.path),
    )
}

fn change_verb(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Add => "Added",
        ChangeKind::Update => "Edited",
        ChangeKind::Delete => "Deleted",
    }
}

fn line_count_spans(added: usize, removed: usize) -> Vec<Span<'static>> {
    vec![
        Span::raw("("),
        Span::styled(format!("+{added}"), Style::default().fg(Color::Green)),
        Span::raw(" "),
        Span::styled(format!("-{removed}"), Style::default().fg(Color::Red)),
        Span::raw(")"),
    ]
}

fn line_counts(added: usize, removed: usize) -> String {
    format!("(+{added} -{removed})")
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::present_apply_patch;

    #[test]
    fn presents_paths_counts_moves_and_styled_hunks() {
        let patch = "*** Begin Patch\n*** Update File: src/old.rs\n*** Move to: src/new.rs\n@@\n-old()\n+new()\n*** Add File: README.md\n+# title\n*** End Patch";
        let presentation = present_apply_patch(patch).expect("valid patch presentation");

        assert_eq!(presentation.summary, "Edited 2 files (+2 -1)");
        let lines = presentation.lines(80);
        let rendered = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.starts_with("• Edited 2 files (+2 -1)"));
        assert!(rendered.contains("src/old.rs → src/new.rs"));
        assert!(rendered.contains("README.md"));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content == "-" && span.style.fg == Some(Color::Red))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content == "+" && span.style.fg == Some(Color::Green))
        }));
    }

    #[test]
    fn wraps_long_patch_lines_before_the_terminal_widget() {
        let patch = "*** Begin Patch\n*** Update File: sources.tsv\n@@\n+https://example.com/a/very/long/path/that/must/not/leak/across/the/terminal\n*** End Patch";
        let presentation = present_apply_patch(patch).unwrap();
        let lines = presentation.lines(32);

        assert!(lines.len() > 2);
        assert!(lines.iter().all(|line| line.width() <= 32));
        assert!(lines[2].to_string().starts_with("       "));
    }

    #[test]
    fn rejects_non_patch_text() {
        assert!(present_apply_patch("not a patch").is_none());
    }
}
