use super::FunctionCallError;

use super::format::split_lines_preserve;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LineEnding {
    CrLf,
    Lf,
    Cr,
    None,
}

impl LineEnding {
    fn as_str(self) -> &'static str {
        match self {
            Self::CrLf => "\r\n",
            Self::Lf => "\n",
            Self::Cr => "\r",
            Self::None => "",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct SourceLine {
    text: String,
    ending: LineEnding,
}
#[derive(Clone, Debug)]
pub(super) struct SourceDocument {
    pub(super) bom: bool,
    pub(super) lines: Vec<SourceLine>,
    pub(super) fallback_line_ending: LineEnding,
}

impl AsRef<str> for SourceLine {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

pub(super) fn source_document(contents: &str, normalized_contents: &str) -> SourceDocument {
    let line_endings = original_line_endings(contents);
    let lines = split_lines_preserve(normalized_contents)
        .into_iter()
        .enumerate()
        .map(|(index, line)| SourceLine {
            text: line.to_string(),
            ending: line_endings.get(index).copied().unwrap_or(LineEnding::None),
        })
        .collect();
    SourceDocument {
        bom: contents.starts_with('\u{feff}'),
        lines,
        fallback_line_ending: line_endings.first().copied().unwrap_or(LineEnding::Lf),
    }
}

pub(super) fn reassemble_file_contents(document: &SourceDocument) -> String {
    let mut output = String::new();
    if document.bom {
        output.push('\u{feff}');
    }
    for line in &document.lines {
        output.push_str(&line.text);
        output.push_str(line.ending.as_str());
    }
    output
}

fn original_line_endings(contents: &str) -> Vec<LineEnding> {
    let mut line_endings = Vec::new();
    let mut chars = contents.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        let ending = match ch {
            '\r' => match chars.peek().copied() {
                Some((_, '\n')) => {
                    chars.next();
                    LineEnding::CrLf
                }
                _ => LineEnding::Cr,
            },
            '\n' => LineEnding::Lf,
            _ => continue,
        };
        line_endings.push(ending);
    }
    line_endings
}

fn preferred_insertion_ending(
    lines: &[SourceLine],
    index: usize,
    fallback_line_ending: LineEnding,
) -> LineEnding {
    lines
        .get(index)
        .map(|line| line.ending)
        .filter(|ending| *ending != LineEnding::None)
        .or_else(|| {
            index
                .checked_sub(1)
                .and_then(|previous| lines.get(previous))
                .map(|line| line.ending)
                .filter(|ending| *ending != LineEnding::None)
        })
        .unwrap_or(fallback_line_ending)
}

pub(super) fn insert_current_lines(
    lines: &mut Vec<SourceLine>,
    index: usize,
    inserted: &[String],
    original_has_final_newline: bool,
    fallback_line_ending: LineEnding,
) {
    if inserted.is_empty() {
        return;
    }
    let ending = preferred_insertion_ending(lines, index, fallback_line_ending);
    let preserve_no_final_newline = index == lines.len()
        && (lines
            .last()
            .is_some_and(|line| line.ending == LineEnding::None)
            || (lines.is_empty() && !original_has_final_newline));
    if preserve_no_final_newline && let Some(last) = lines.last_mut() {
        last.ending = ending;
    }
    let inserted_lines = inserted
        .iter()
        .enumerate()
        .map(|(index, text)| SourceLine {
            text: text.clone(),
            ending: if preserve_no_final_newline && index + 1 == inserted.len() {
                LineEnding::None
            } else {
                ending
            },
        })
        .collect::<Vec<_>>();
    lines.splice(index..index, inserted_lines);
}

pub(super) fn replace_current_range(
    lines: &mut Vec<SourceLine>,
    start_index: usize,
    removed_count: usize,
    inserted: &[String],
    fallback_line_ending: LineEnding,
) -> Result<(), FunctionCallError> {
    if start_index.saturating_add(removed_count) > lines.len() {
        return Err(FunctionCallError::RespondToModel(
            "Hashline operation no longer maps to the current file contents".to_string(),
        ));
    }
    let end_index = start_index + removed_count;
    let replacement_ending = lines[start_index..end_index]
        .iter()
        .map(|line| line.ending)
        .find(|ending| *ending != LineEnding::None)
        .or_else(|| {
            lines
                .get(end_index)
                .map(|line| line.ending)
                .filter(|ending| *ending != LineEnding::None)
        })
        .or_else(|| {
            start_index
                .checked_sub(1)
                .and_then(|previous| lines.get(previous))
                .map(|line| line.ending)
                .filter(|ending| *ending != LineEnding::None)
        })
        .unwrap_or(fallback_line_ending);
    let final_ending = lines
        .get(end_index.saturating_sub(1))
        .map_or(replacement_ending, |line| line.ending);
    let removed_final_no_newline = end_index == lines.len() && final_ending == LineEnding::None;
    let replacement = inserted
        .iter()
        .enumerate()
        .map(|(index, text)| SourceLine {
            text: text.clone(),
            ending: if index + 1 == inserted.len() {
                final_ending
            } else {
                replacement_ending
            },
        })
        .collect::<Vec<_>>();
    lines.splice(start_index..end_index, replacement);
    if inserted.is_empty()
        && removed_final_no_newline
        && let Some(last) = lines.last_mut()
    {
        last.ending = LineEnding::None;
    }
    Ok(())
}
