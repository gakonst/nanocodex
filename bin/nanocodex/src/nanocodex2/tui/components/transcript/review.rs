//! Self-contained review snapshots. Never resolve a finding against the live workspace.
use super::diff::{self, DiffLine, DiffLineKind};
use super::markdown::{sanitize, wrap_plain};
use crate::nanocodex2::tui::theme::Theme;
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use serde::Deserialize;
use syntect::easy::HighlightLines;

const MAX_BLOCK: usize = 128 * 1024;
const MAX_DIFF: usize = 96 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Finding {
    file: String,
    side: Side,
    line_start: u64,
    line_end: u64,
    title: String,
    body: String,
    #[serde(default)]
    diff: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Side {
    Old,
    New,
}

struct Hunk {
    label: String,
    lines: Vec<DiffLine>,
}

impl Finding {
    fn position(&self, line: &DiffLine) -> Option<u64> {
        match self.side {
            Side::Old => line.old_line,
            Side::New => line.new_line,
        }
    }
    fn anchored(&self, line: &DiffLine) -> bool {
        self.position(line)
            .is_some_and(|n| (self.line_start..=self.line_end).contains(&n))
    }
    fn location(&self) -> String {
        format!(
            "{} · {} lines {}–{}",
            sanitize(&self.file),
            match self.side {
                Side::Old => "old",
                Side::New => "new",
            },
            self.line_start,
            self.line_end
        )
    }
}

/// None leaves malformed or oversized input in the ordinary, readable code renderer.
pub(super) fn render(source: &str, width: u16, theme: &Theme) -> Option<Vec<Line<'static>>> {
    if source.len() > MAX_BLOCK {
        return None;
    }
    let finding: Finding = serde_json::from_str(source).ok()?;
    if finding.file.trim().is_empty()
        || finding.file.len() > 4096
        || finding.title.trim().is_empty()
        || finding.title.len() > 4096
        || finding.body.len() > 32 * 1024
    {
        return None;
    }
    let mut out = Vec::new();
    if width == 0 {
        return Some(out);
    }
    // Below five cells, borders leave no useful content. Still retain every finding word.
    let bordered = width >= 5;
    let inner = if bordered { width - 4 } else { width };
    if bordered {
        out.push(diff::component_header("Review", width, theme));
    }
    prose(&mut out, &finding.location(), inner, bordered, theme, false);
    match parse_diff(&finding) {
        Err(reason) => {
            prose(
                &mut out,
                &format!("Context unavailable: {reason}"),
                inner,
                bordered,
                theme,
                false,
            );
            comment(&mut out, &finding, inner, bordered, theme);
        }
        Ok(hunks) => {
            let assets = super::highlight::assets();
            let syntax = super::highlight::syntax_for_path(&assets.syntaxes, &finding.file);
            let syntax_theme = super::highlight::theme();
            let digits = hunks
                .iter()
                .flat_map(|h| &h.lines)
                .flat_map(|l| [l.old_line, l.new_line])
                .flatten()
                .map(|n| n.to_string().len() as u16)
                .max()
                .unwrap_or(1);
            let overhead = digits * 2 + 9;
            for hunk in hunks {
                if bordered {
                    out.push(diff::hunk_divider(&hunk.label, width, theme));
                } else {
                    prose(&mut out, &hunk.label, inner, bordered, theme, false);
                }
                let mut old = HighlightLines::new(syntax, syntax_theme);
                let mut new = HighlightLines::new(syntax, syntax_theme);
                for line in hunk.lines {
                    let marked = finding.anchored(&line);
                    let spans =
                        diff::highlighted_diff_line(&line, &mut old, &mut new, &assets.syntaxes);
                    if width > overhead {
                        let code_width = width - overhead;
                        for (index, code) in super::highlight::wrap(spans, code_width)
                            .into_iter()
                            .enumerate()
                        {
                            let mut row =
                                diff::component_body(&line, code, index, digits, code_width, theme);
                            if marked {
                                row.spans[0] = Span::styled(
                                    "┃",
                                    Style::default()
                                        .fg(theme.accent())
                                        .add_modifier(Modifier::BOLD),
                                );
                                row.spans[1].style = Style::default()
                                    .fg(theme.accent())
                                    .add_modifier(Modifier::BOLD);
                            }
                            out.push(row);
                        }
                    } else {
                        // Stack the absolute gutters above code when side-by-side columns cannot fit.
                        let marker = match line.kind {
                            DiffLineKind::Addition => "+",
                            DiffLineKind::Deletion => "-",
                            DiffLineKind::Context => " ",
                        };
                        let label = format!(
                            "{} old:{} new:{} {marker}",
                            if marked { ">" } else { " " },
                            line.old_line
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "–".into()),
                            line.new_line
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "–".into())
                        );
                        prose(&mut out, &label, inner, bordered, theme, marked);
                        for code in super::highlight::wrap(spans, inner) {
                            out.push(frame(code, inner, bordered, theme));
                        }
                    }
                    if finding.position(&line) == Some(finding.line_end) {
                        comment(&mut out, &finding, inner, bordered, theme);
                    }
                }
            }
        }
    }
    if bordered {
        out.push(Line::from(Span::styled(
            format!("╰{}╯", "─".repeat(usize::from(width - 2))),
            Style::default().fg(theme.border()),
        )));
    }
    Some(out)
}

fn frame(spans: Vec<Span<'static>>, width: u16, bordered: bool, theme: &Theme) -> Line<'static> {
    if !bordered {
        return Line::from(spans);
    }
    let padding = usize::from(width).saturating_sub(spans.iter().map(Span::width).sum());
    let mut row = vec![Span::styled("│ ", Style::default().fg(theme.border()))];
    row.extend(spans);
    row.push(Span::raw(" ".repeat(padding)));
    row.push(Span::styled(" │", Style::default().fg(theme.border())));
    Line::from(row)
}

fn prose(
    out: &mut Vec<Line<'static>>,
    text: &str,
    width: u16,
    bordered: bool,
    theme: &Theme,
    bold: bool,
) {
    let style = if bold {
        Style::default()
            .fg(theme.accent())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text())
    };
    for line in wrap_plain(text, width, style) {
        out.push(frame(line.spans, width, bordered, theme));
    }
}

fn comment(
    out: &mut Vec<Line<'static>>,
    finding: &Finding,
    width: u16,
    bordered: bool,
    theme: &Theme,
) {
    prose(
        out,
        &format!("↳ {}", finding.title),
        width,
        bordered,
        theme,
        true,
    );
    prose(out, &finding.body, width, bordered, theme, false);
}

fn range(token: &str, prefix: char) -> Option<(u64, u64)> {
    let token = token.strip_prefix(prefix)?;
    let (start, count) = token.split_once(',').unwrap_or((token, "1"));
    if start.is_empty()
        || count.is_empty()
        || !start.bytes().all(|b| b.is_ascii_digit())
        || !count.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let start: u64 = start.parse().ok()?;
    let count: u64 = count.parse().ok()?;
    if count > 2000 || (count > 0 && start == 0) {
        return None;
    }
    start.checked_add(count)?;
    Some((start, count))
}

// Unified headers delimit timestamps with TAB, not spaces. Git's quoted paths are
// deliberately rejected rather than guessing their C-escape decoding or identity.
fn path(header: &str) -> Option<&str> {
    let path = header.split('\t').next()?;
    if path.starts_with('"') || path.chars().any(char::is_control) {
        return None;
    }
    Some(
        path.strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))
            .unwrap_or(path),
    )
}

fn parse_diff(finding: &Finding) -> Result<Vec<Hunk>, &'static str> {
    if finding.diff.is_empty() {
        return Err("no captured diff");
    }
    if finding.diff.len() > MAX_DIFF {
        return Err("captured diff exceeds size limit");
    }
    if finding.line_start == 0
        || finding.line_end < finding.line_start
        || finding.line_end - finding.line_start > 2000
    {
        return Err("invalid anchor range");
    }
    if finding.file.chars().any(char::is_control) {
        return Err("invalid file path");
    }
    let lines: Vec<_> = finding.diff.lines().collect();
    if lines.len() > 2000 {
        return Err("captured diff exceeds line limit");
    }
    let mut i = 0;
    while i < lines.len()
        && (lines[i].starts_with("diff --git ")
            || lines[i].starts_with("index ")
            || lines[i].starts_with("old mode ")
            || lines[i].starts_with("new mode ")
            || lines[i].starts_with("similarity index ")
            || lines[i].starts_with("rename from ")
            || lines[i].starts_with("rename to ")
            || lines[i].starts_with("copy from ")
            || lines[i].starts_with("copy to ")
            || lines[i].starts_with("new file mode ")
            || lines[i].starts_with("deleted file mode "))
    {
        i += 1;
    }
    let old = lines
        .get(i)
        .and_then(|s| s.strip_prefix("--- "))
        .and_then(path)
        .ok_or("missing old file header")?;
    let new = lines
        .get(i + 1)
        .and_then(|s| s.strip_prefix("+++ "))
        .and_then(path)
        .ok_or("missing new file header")?;
    let old_missing = old == "/dev/null";
    let new_missing = new == "/dev/null";
    let selected = match finding.side {
        Side::Old => old,
        Side::New => new,
    };
    if selected == "/dev/null" || selected != finding.file {
        return Err("file does not match selected diff side");
    }
    i += 2;
    let mut hunks = Vec::new();
    let mut previous = (0, 0);
    let mut anchors = 0;
    while i < lines.len() {
        let heading = lines[i];
        let ranges = heading
            .strip_prefix("@@ ")
            .and_then(|s| s.split_once(" @@"))
            .ok_or("invalid absolute hunk header")?
            .0;
        let mut tokens = ranges.split_ascii_whitespace();
        let (mut old, old_count) = tokens
            .next()
            .and_then(|s| range(s, '-'))
            .ok_or("invalid old hunk range")?;
        let (mut new, new_count) = tokens
            .next()
            .and_then(|s| range(s, '+'))
            .ok_or("invalid new hunk range")?;
        if (old_missing && old_count != 0) || (new_missing && new_count != 0) {
            return Err("missing file side has nonempty hunk");
        }
        if tokens.next().is_some() || old < previous.0 || new < previous.1 {
            return Err("overlapping or invalid hunks");
        }
        previous = (old + old_count, new + new_count);
        i += 1;
        let mut consumed = (0, 0);
        let mut body = Vec::new();
        while i < lines.len() && !lines[i].starts_with("@@ ") {
            let source = lines[i];
            i += 1;
            if source == "\\ No newline at end of file" && !body.is_empty() {
                continue;
            }
            let (kind, text) = if let Some(s) = source.strip_prefix(' ') {
                (DiffLineKind::Context, s)
            } else if let Some(s) = source.strip_prefix('+') {
                (DiffLineKind::Addition, s)
            } else if let Some(s) = source.strip_prefix('-') {
                (DiffLineKind::Deletion, s)
            } else {
                return Err("invalid unified diff line");
            };
            let old_line = if !matches!(kind, DiffLineKind::Addition) {
                consumed.0 += 1;
                let n = old;
                old = old.checked_add(1).ok_or("line overflow")?;
                Some(n)
            } else {
                None
            };
            let new_line = if !matches!(kind, DiffLineKind::Deletion) {
                consumed.1 += 1;
                let n = new;
                new = new.checked_add(1).ok_or("line overflow")?;
                Some(n)
            } else {
                None
            };
            if consumed.0 > old_count || consumed.1 > new_count {
                return Err("hunk line counts do not match");
            }
            body.push(DiffLine {
                kind,
                text: sanitize(text),
                old_line,
                new_line,
            });
        }
        if consumed != (old_count, new_count) {
            return Err("incomplete captured hunk");
        }
        let anchor_count = body.iter().filter(|l| finding.anchored(l)).count() as u64;
        if anchor_count > 0 {
            if anchor_count != finding.line_end - finding.line_start + 1 {
                return Err("anchor range crosses or exceeds captured hunk");
            }
            anchors += 1;
        }
        hunks.push(Hunk {
            label: sanitize(heading),
            lines: body,
        });
    }
    if anchors != 1 {
        return Err("anchor range absent or ambiguous in captured diff");
    }
    hunks.retain(|hunk| hunk.lines.iter().any(|line| finding.anchored(line)));
    Ok(hunks)
}
