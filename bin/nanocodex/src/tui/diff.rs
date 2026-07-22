use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::markdown::highlighted_code_lines;

#[derive(Clone)]
pub(super) struct PatchPresentation {
    pub(super) summary: String,
    pub(super) lines: Vec<Line<'static>>,
}

#[derive(Clone, Copy)]
enum ChangeKind {
    Add,
    Update,
    Delete,
}

struct FilePatch {
    kind: ChangeKind,
    path: String,
    move_to: Option<String>,
    body: Vec<String>,
    added: usize,
    removed: usize,
}

pub(super) fn present_apply_patch(source: &str) -> Option<PatchPresentation> {
    let files = parse_apply_patch(source);
    if files.is_empty() {
        return None;
    }

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

    let mut lines = Vec::new();
    for (index, file) in files.iter().enumerate() {
        if index > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", change_verb(file.kind)),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(display_path(file), Style::default().fg(Color::Cyan)),
            Span::raw(" "),
            Span::styled(
                format!("+{}", file.added),
                Style::default().fg(Color::Green),
            ),
            Span::raw(" "),
            Span::styled(
                format!("-{}", file.removed),
                Style::default().fg(Color::Red),
            ),
        ]));
        lines.extend(render_body(file));
    }

    Some(PatchPresentation { summary, lines })
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
        } else {
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

fn render_body(file: &FilePatch) -> Vec<Line<'static>> {
    let language = file
        .move_to
        .as_deref()
        .unwrap_or(&file.path)
        .rsplit_once('.')
        .map(|(_, extension)| extension);
    let source = file
        .body
        .iter()
        .filter_map(|line| line.strip_prefix('+').or_else(|| line.strip_prefix(' ')))
        .collect::<Vec<_>>()
        .join("\n");
    let highlighted = highlighted_code_lines(language, &source);
    let mut highlighted = highlighted.into_iter();

    file.body
        .iter()
        .map(|line| {
            if line.starts_with("@@") {
                return Line::styled(line.clone(), Style::default().fg(Color::Cyan));
            }
            let (marker, content, color, syntax) = if let Some(content) = line.strip_prefix('+') {
                ('+', content, Color::Green, true)
            } else if let Some(content) = line.strip_prefix('-') {
                ('-', content, Color::Red, false)
            } else if let Some(content) = line.strip_prefix(' ') {
                (' ', content, Color::DarkGray, true)
            } else {
                (' ', line.as_str(), Color::DarkGray, false)
            };
            let mut spans = vec![Span::styled(
                format!("{marker} "),
                Style::default().fg(color),
            )];
            if syntax {
                if let Some(highlighted) = highlighted.next() {
                    spans.extend(highlighted.spans);
                } else {
                    spans.push(Span::styled(content.to_owned(), Style::default().fg(color)));
                }
            } else {
                spans.push(Span::styled(content.to_owned(), Style::default().fg(color)));
            }
            Line::from(spans)
        })
        .collect()
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
        let rendered = presentation
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("src/old.rs → src/new.rs"));
        assert!(rendered.contains("README.md"));
        assert!(presentation.lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains("old") && span.style.fg == Some(Color::Red))
        }));
        assert!(rendered.contains("new"));
        assert!(presentation.lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content == "+ " && span.style.fg == Some(Color::Green))
        }));
    }

    #[test]
    fn rejects_non_patch_text() {
        assert!(present_apply_patch("not a patch").is_none());
    }
}
