use super::hash::line_hash;
use super::take_bytes_at_char_boundary;

const CONTENT_TRUNCATION_MARKER: &str = "... [content truncated]";
const SERIALIZED_LINE_FIXED_OVERHEAD: usize = 80;

pub(super) struct HashlineExcerpt {
    pub content: String,
    pub end_line: Option<usize>,
    pub truncated: bool,
}

pub(super) fn build_hashline_excerpt(
    lines: &[&str],
    start_line: usize,
    end_line: usize,
    max_serialized_bytes: usize,
) -> HashlineExcerpt {
    if start_line == 0 || start_line > end_line || start_line > lines.len() {
        return HashlineExcerpt {
            content: String::new(),
            end_line: None,
            truncated: false,
        };
    }

    let end_line = end_line.min(lines.len());
    let mut content = Vec::new();
    let mut used_bytes = 0;
    let mut last_line = None;
    let mut truncated = false;

    for (index, line) in lines[start_line - 1..end_line].iter().enumerate() {
        let line_number = start_line + index;
        let hash = line_hash(line);
        let remaining = max_serialized_bytes.saturating_sub(used_bytes);
        let content_truncated = false;
        if let Some(cost) =
            serialized_line_cost(line_number, &hash, line, content_truncated, remaining)
        {
            let formatted = formatted_line(line_number, &hash, line);
            used_bytes += cost;
            content.push(formatted);
            last_line = Some(line_number);
            continue;
        }

        truncated = true;
        if let Some(formatted) = fit_truncated_line(line_number, &hash, line, remaining) {
            content.push(formatted);
            last_line = Some(line_number);
        }
        break;
    }

    HashlineExcerpt {
        content: content.join("\n"),
        end_line: last_line,
        truncated: truncated || last_line.is_some_and(|last_line| last_line < end_line),
    }
}

pub(super) fn split_lines_preserve(contents: &str) -> Vec<&str> {
    let trimmed = contents.strip_suffix('\n').unwrap_or(contents);
    if trimmed.is_empty() {
        Vec::new()
    } else {
        trimmed.split('\n').collect()
    }
}

fn fit_truncated_line(
    line_number: usize,
    hash: &str,
    line: &str,
    remaining: usize,
) -> Option<String> {
    let mut prefix_bytes = line.len().min(remaining / 2);
    loop {
        let prefix = take_bytes_at_char_boundary(line, prefix_bytes);
        let display = format!("{prefix}{CONTENT_TRUNCATION_MARKER}");
        if serialized_line_cost(line_number, hash, &display, true, remaining).is_some() {
            return Some(formatted_line(line_number, hash, &display));
        }
        if prefix_bytes == 0 {
            return None;
        }
        prefix_bytes /= 2;
    }
}

fn formatted_line(line_number: usize, hash: &str, content: &str) -> String {
    format!("{line_number}:{hash}|{content}")
}

fn serialized_line_cost(
    line_number: usize,
    hash: &str,
    content: &str,
    content_truncated: bool,
    limit: usize,
) -> Option<usize> {
    let fixed = SERIALIZED_LINE_FIXED_OVERHEAD
        .saturating_add(decimal_digits(line_number).saturating_mul(2))
        .saturating_add(hash.len().saturating_mul(2))
        .saturating_add(usize::from(content_truncated).saturating_mul(48));
    let content_limit = limit.checked_sub(fixed)?;
    let content_len = json_escaped_content_len_bounded(content, content_limit)?;
    fixed.checked_add(content_len)
}

pub(super) fn json_escaped_content_len_bounded(value: &str, limit: usize) -> Option<usize> {
    let mut len = 0_usize;
    for ch in value.chars() {
        let char_len = match ch {
            '"' | '\\' | '\u{0008}' | '\u{000c}' | '\n' | '\r' | '\t' => 2,
            '\u{0000}'..='\u{001f}' => 6,
            _ => ch.len_utf8(),
        };
        len = len.checked_add(char_len)?;
        if len > limit {
            return None;
        }
    }
    Some(len)
}

fn decimal_digits(value: usize) -> usize {
    value.checked_ilog10().unwrap_or_default() as usize + 1
}
