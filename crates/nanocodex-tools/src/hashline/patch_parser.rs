#![allow(clippy::too_many_lines)]

use super::FunctionCallError;

use super::hash::FILE_HASH_WIDTH;
use super::hash::LINE_HASH_WIDTH;
use super::patch_sections::parse_contextual_patch_file_header;

struct PayloadLine {
    text: String,
    kind: PayloadLineKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PayloadLineKind {
    Bare,
    Literal,
}

#[derive(Debug, Clone)]
pub(super) struct LineAnchor {
    pub(super) line: usize,
    pub(super) expected_hash: String,
}

#[derive(Debug, Clone)]
pub(super) struct BlockAnchor {
    pub(super) line: LineAnchor,
    pub(super) expected_hash: String,
}

#[derive(Debug, Clone)]
pub(super) struct LineRange {
    pub(super) start: LineAnchor,
    pub(super) end: LineAnchor,
}

#[derive(Debug, Clone)]
pub(super) enum HashlineOperation {
    Swap {
        range: LineRange,
        replacement: Vec<String>,
    },
    Delete {
        range: LineRange,
    },
    InsertBefore {
        anchor: LineAnchor,
        inserted: Vec<String>,
    },
    InsertAfter {
        anchor: LineAnchor,
        inserted: Vec<String>,
    },
    InsertHead {
        inserted: Vec<String>,
    },
    InsertTail {
        inserted: Vec<String>,
    },
    SwapBlock {
        anchor: BlockAnchor,
        replacement: Vec<String>,
    },
    DeleteBlock {
        anchor: BlockAnchor,
    },
    InsertBlockBefore {
        anchor: BlockAnchor,
        inserted: Vec<String>,
    },
    InsertBlockAfter {
        anchor: BlockAnchor,
        inserted: Vec<String>,
    },
    RemoveFile,
    RenameFile {
        new_path: String,
    },
}

impl HashlineOperation {
    pub(super) fn is_file_operation(&self) -> bool {
        matches!(
            self,
            HashlineOperation::RemoveFile | HashlineOperation::RenameFile { .. }
        )
    }
}

pub(super) fn parse_anchor_line(anchor: &str) -> Result<usize, FunctionCallError> {
    let line = anchor
        .split_once(':')
        .map_or(anchor, |(line, _)| line)
        .trim();
    line.parse::<usize>().map_err(|err| {
        FunctionCallError::RespondToModel(format!("invalid Hashline anchor {anchor}: {err}"))
    })
}

pub(super) fn parse_anchor_hash(anchor: &str) -> Option<&str> {
    anchor
        .split_once(':')
        .map(|(_, hash)| hash)
        .filter(|hash| !hash.is_empty())
}

pub(super) fn parse_hashline_patch(
    patch: &str,
) -> Result<Vec<HashlineOperation>, FunctionCallError> {
    let raw_lines = patch
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .collect::<Vec<_>>();
    let mut operations = Vec::new();
    let mut index = 0;
    while index < raw_lines.len() {
        let line = raw_lines[index].trim_end();
        if is_ignorable_patch_line(line) {
            index += 1;
            continue;
        }
        if line.starts_with('[') && parse_contextual_patch_file_header("", line, false)?.is_some() {
            index += 1;
            continue;
        }
        if let Some(message) = apply_patch_contamination_message(line) {
            return Err(FunctionCallError::RespondToModel(message));
        }
        let (op, rest) = split_hashline_operation(line)?;
        let op = op.to_ascii_uppercase();
        index += 1;

        let operation = match op.as_str() {
            "SWAP" => {
                if let Some((target, replacement)) = rest.split_once('|') {
                    HashlineOperation::Swap {
                        range: parse_line_range(target)?,
                        replacement: vec![replacement.to_string()],
                    }
                } else {
                    HashlineOperation::Swap {
                        range: parse_line_range(rest)?,
                        replacement: collect_payload_lines(&raw_lines, &mut index)?,
                    }
                }
            }
            "DEL" => HashlineOperation::Delete {
                range: parse_line_range(rest)?,
            },
            "INS.PRE" => {
                if let Some((target, inserted)) = rest.split_once('|') {
                    HashlineOperation::InsertBefore {
                        anchor: parse_line_anchor(target)?,
                        inserted: vec![inserted.to_string()],
                    }
                } else {
                    HashlineOperation::InsertBefore {
                        anchor: parse_line_anchor(rest)?,
                        inserted: collect_payload_lines(&raw_lines, &mut index)?,
                    }
                }
            }
            "INS.POST" => {
                if let Some((target, inserted)) = rest.split_once('|') {
                    HashlineOperation::InsertAfter {
                        anchor: parse_line_anchor(target)?,
                        inserted: vec![inserted.to_string()],
                    }
                } else {
                    HashlineOperation::InsertAfter {
                        anchor: parse_line_anchor(rest)?,
                        inserted: collect_payload_lines(&raw_lines, &mut index)?,
                    }
                }
            }
            "INS.HEAD" => {
                if let Some(inserted) = rest.trim_start().strip_prefix('|') {
                    HashlineOperation::InsertHead {
                        inserted: vec![inserted.to_string()],
                    }
                } else {
                    validate_empty_target(rest, "INS.HEAD")?;
                    HashlineOperation::InsertHead {
                        inserted: collect_payload_lines(&raw_lines, &mut index)?,
                    }
                }
            }
            "INS.TAIL" => {
                if let Some(inserted) = rest.trim_start().strip_prefix('|') {
                    HashlineOperation::InsertTail {
                        inserted: vec![inserted.to_string()],
                    }
                } else {
                    validate_empty_target(rest, "INS.TAIL")?;
                    HashlineOperation::InsertTail {
                        inserted: collect_payload_lines(&raw_lines, &mut index)?,
                    }
                }
            }
            "SWAP.BLK" => HashlineOperation::SwapBlock {
                anchor: parse_block_anchor(rest)?,
                replacement: collect_payload_lines(&raw_lines, &mut index)?,
            },
            "DEL.BLK" => HashlineOperation::DeleteBlock {
                anchor: parse_block_anchor(rest)?,
            },
            "INS.BLK.POST" | "INS.BLK" => HashlineOperation::InsertBlockAfter {
                anchor: parse_block_anchor(rest)?,
                inserted: collect_payload_lines(&raw_lines, &mut index)?,
            },
            "INS.BLK.PRE" => HashlineOperation::InsertBlockBefore {
                anchor: parse_block_anchor(rest)?,
                inserted: collect_payload_lines(&raw_lines, &mut index)?,
            },
            "REM" => {
                validate_empty_target(rest, "REM")?;
                HashlineOperation::RemoveFile
            }
            "MV" => HashlineOperation::RenameFile {
                new_path: parse_move_target(rest)?,
            },
            _ => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "unsupported Hashline operation {op}"
                )));
            }
        };
        operations.push(operation);
    }
    Ok(operations)
}

pub(super) fn is_ignorable_patch_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed == "*** Begin Patch" || trimmed == "*** End Patch"
}

fn collect_payload_lines(
    raw_lines: &[&str],
    index: &mut usize,
) -> Result<Vec<String>, FunctionCallError> {
    let mut payload = Vec::new();
    while *index < raw_lines.len() {
        let line = raw_lines[*index];
        if is_ignorable_patch_line(line) {
            *index += 1;
            if payload.is_empty() {
                continue;
            }
            break;
        }
        if parse_contextual_patch_file_header("", line, true)?.is_some() {
            break;
        }
        if let Some(message) = apply_patch_contamination_message(line) {
            return Err(FunctionCallError::RespondToModel(message));
        }
        if is_hashline_operation_line(line) {
            break;
        }
        if line.trim_start().starts_with('-') {
            return Err(FunctionCallError::RespondToModel(format!(
                "Hashline payload line {line:?} must start with + or be a bare replacement line; - rows are not accepted"
            )));
        }
        let (text, kind) = match line.strip_prefix('+') {
            Some(text) => (text, PayloadLineKind::Literal),
            None => (line, PayloadLineKind::Bare),
        };
        payload.push(PayloadLine {
            text: text.to_string(),
            kind,
        });
        *index += 1;
    }
    strip_uniform_read_output_payload_prefixes(&mut payload)?;
    Ok(payload.into_iter().map(|line| line.text).collect())
}

fn strip_uniform_read_output_payload_prefixes(
    payload: &mut [PayloadLine],
) -> Result<(), FunctionCallError> {
    let mut saw_prefixed = false;
    let mut saw_unprefixed = false;
    for line in payload
        .iter()
        .filter(|line| line.kind == PayloadLineKind::Bare)
    {
        if strip_read_output_payload_prefix(&line.text).is_some() {
            saw_prefixed = true;
        } else {
            saw_unprefixed = true;
        }
    }
    if !saw_prefixed {
        return Ok(());
    }
    if saw_unprefixed
        || payload
            .iter()
            .any(|line| line.kind == PayloadLineKind::Literal)
    {
        return Err(FunctionCallError::RespondToModel(
            "Hashline payload mixes pasted read-output rows with literal rows; prefix literal rows with + or provide uniformly formatted read-output rows"
                .to_string(),
        ));
    }
    for line in payload
        .iter_mut()
        .filter(|line| line.kind == PayloadLineKind::Bare)
    {
        if let Some(stripped) = strip_read_output_payload_prefix(&line.text) {
            line.text = stripped.to_string();
        }
    }
    Ok(())
}

fn strip_read_output_payload_prefix(line: &str) -> Option<&str> {
    let mut line = line.trim_start_matches([' ', '\t']);
    if let Some(prompt_stripped) = line.strip_prefix(">>>").or_else(|| line.strip_prefix(">>")) {
        line = prompt_stripped.trim_start_matches([' ', '\t']);
    }
    if let Some(marker_stripped) = line.strip_prefix('+').or_else(|| line.strip_prefix('*')) {
        line = marker_stripped.trim_start_matches([' ', '\t']);
    }

    let (line_number, rest) = line.split_once(':')?;
    if line_number.is_empty() || !line_number.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let (line_hash, content) = rest.split_once('|')?;
    (line_hash.len() == LINE_HASH_WIDTH && line_hash.chars().all(|ch| ch.is_ascii_hexdigit()))
        .then_some(content)
}

pub(super) fn apply_patch_contamination_message(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if trimmed == "*** Begin Patch"
        || trimmed == "*** End Patch"
        || trimmed.starts_with("*** Update File:")
        || trimmed.starts_with("*** Add File:")
        || trimmed.starts_with("*** Delete File:")
        || trimmed.starts_with("*** Move to:")
    {
        let preview = if trimmed.chars().count() > 48 {
            format!("{}...", trimmed.chars().take(48).collect::<String>())
        } else {
            trimmed.to_string()
        };
        return Some(format!(
            "apply_patch sentinel {preview:?} is not valid in Hashline. Reread the target with hashline__read, replace the sentinel with its [path]#HASH header, and use an operation such as SWAP 12:abcd:\n+replacement."
        ));
    }
    if trimmed.starts_with("@@ ") && trimmed.contains("@@") {
        return Some(
            "unified-diff hunk headers are not valid in Hashline. Reread the target with hashline__read, start with its [path]#HASH header, and replace the hunk with an operation such as SWAP 12:abcd..=14:ef01:\n+replacement."
                .to_string(),
        );
    }
    None
}

pub(super) fn is_hashline_operation_line(line: &str) -> bool {
    let Ok((op, _)) = split_hashline_operation(line) else {
        return false;
    };
    matches!(
        op.to_ascii_uppercase().as_str(),
        "SWAP"
            | "DEL"
            | "INS.PRE"
            | "INS.POST"
            | "INS.HEAD"
            | "INS.TAIL"
            | "SWAP.BLK"
            | "DEL.BLK"
            | "INS.BLK"
            | "INS.BLK.POST"
            | "INS.BLK.PRE"
            | "REM"
            | "MV"
    )
}

pub(super) fn hashline_operation_has_payload(line: &str) -> bool {
    let Ok((op, rest)) = split_hashline_operation(line) else {
        return false;
    };
    !rest.contains('|')
        && matches!(
            op.to_ascii_uppercase().as_str(),
            "SWAP"
                | "INS.PRE"
                | "INS.POST"
                | "INS.HEAD"
                | "INS.TAIL"
                | "SWAP.BLK"
                | "INS.BLK"
                | "INS.BLK.POST"
                | "INS.BLK.PRE"
        )
}

fn validate_empty_target(rest: &str, op: &str) -> Result<(), FunctionCallError> {
    let rest = rest.trim();
    if rest.is_empty() || rest == ":" {
        return Ok(());
    }
    Err(FunctionCallError::RespondToModel(format!(
        "{op} does not accept a line target"
    )))
}

fn parse_move_target(rest: &str) -> Result<String, FunctionCallError> {
    let rest = rest.trim().strip_prefix(':').map_or(rest.trim(), str::trim);
    if rest.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "MV requires a destination path".to_string(),
        ));
    }
    let mut chars = rest.char_indices();
    let Some((_, first)) = chars.next() else {
        return Err(FunctionCallError::RespondToModel(
            "MV requires a destination path".to_string(),
        ));
    };
    if first != '\'' && first != '"' {
        return Ok(rest.to_string());
    }

    let mut escaped = false;
    for (index, ch) in chars {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == first {
            let after = rest[index + ch.len_utf8()..].trim();
            if !after.is_empty() {
                return Err(FunctionCallError::RespondToModel(format!(
                    "invalid MV destination {rest:?}: unexpected trailing text {after:?}"
                )));
            }
            let inner = &rest[first.len_utf8()..index];
            if inner.is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "MV requires a destination path".to_string(),
                ));
            }
            return Ok(inner.to_string());
        }
    }
    Err(FunctionCallError::RespondToModel(format!(
        "invalid MV destination {rest:?}: missing closing quote"
    )))
}

fn parse_line_range(input: &str) -> Result<LineRange, FunctionCallError> {
    let normalized = normalize_anchor_target(input);
    let (start, end) = if let Some((start, end)) = split_range_anchor_text(&normalized) {
        (
            parse_line_anchor_text(start.trim(), input)?,
            parse_line_anchor_text(end.trim(), input)?,
        )
    } else {
        let anchor = parse_line_anchor_text(&normalized, input)?;
        (anchor.clone(), anchor)
    };
    if end.line < start.line {
        return Err(FunctionCallError::RespondToModel(format!(
            "Hashline range {input} ends before it starts"
        )));
    }
    Ok(LineRange { start, end })
}

fn parse_line_anchor(input: &str) -> Result<LineAnchor, FunctionCallError> {
    let range = parse_line_range(input)?;
    if range.start.line != range.end.line {
        return Err(FunctionCallError::RespondToModel(format!(
            "Hashline insert anchor {input} must be a single line"
        )));
    }
    Ok(range.start)
}

fn normalize_anchor_target(input: &str) -> String {
    let input = input.trim();
    if let Some(input) = input.strip_suffix(':') {
        return input.to_string();
    }
    input.to_string()
}

fn split_range_anchor_text(input: &str) -> Option<(&str, &str)> {
    if let Some(index) = input.find("..=") {
        return Some((&input[..index], &input[index + 3..]));
    }
    if let Some(index) = input.find("..") {
        return Some((&input[..index], &input[index + 2..]));
    }
    input
        .find('-')
        .map(|index| (&input[..index], &input[index + 1..]))
}

fn parse_line_anchor_text(input: &str, source: &str) -> Result<LineAnchor, FunctionCallError> {
    let (line_text, expected_hash) = split_anchor_hash(input.trim(), source)?;
    Ok(LineAnchor {
        line: parse_positive_line_number(line_text, source)?,
        expected_hash,
    })
}

fn split_anchor_hash<'a>(
    input: &'a str,
    source: &str,
) -> Result<(&'a str, String), FunctionCallError> {
    let Some((target, hash)) = input.rsplit_once(':') else {
        return Err(FunctionCallError::RespondToModel(format!(
            "invalid Hashline anchor {source}: expected line:4-hex-hash"
        )));
    };
    let hash = hash.trim().to_ascii_lowercase();
    if hash.len() != LINE_HASH_WIDTH || !hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(FunctionCallError::RespondToModel(format!(
            "invalid Hashline anchor {source}: expected a {LINE_HASH_WIDTH}-hex hash token after ':'; got {hash}"
        )));
    }
    let target = target.trim();
    if target.is_empty() || target.ends_with(':') || target.contains(':') {
        return Err(FunctionCallError::RespondToModel(format!(
            "invalid Hashline anchor {source}: expected line:4-hex-hash"
        )));
    }
    Ok((target, hash))
}

fn parse_block_anchor(input: &str) -> Result<BlockAnchor, FunctionCallError> {
    let normalized = normalize_anchor_target(input);
    let Some((line_anchor, block_hash)) = normalized.rsplit_once('@') else {
        return Err(FunctionCallError::RespondToModel(format!(
            "invalid Hashline block anchor {input}: expected line:LINE_HASH@BLOCK_HASH"
        )));
    };
    if block_hash.len() != FILE_HASH_WIDTH || !block_hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(FunctionCallError::RespondToModel(format!(
            "invalid Hashline block anchor {input}: expected a {FILE_HASH_WIDTH}-hex block hash"
        )));
    }
    Ok(BlockAnchor {
        line: parse_line_anchor(line_anchor)?,
        expected_hash: block_hash.to_ascii_lowercase(),
    })
}

fn parse_positive_line_number(input: &str, source: &str) -> Result<usize, FunctionCallError> {
    let line_number = input.trim().parse::<usize>().map_err(|err| {
        FunctionCallError::RespondToModel(format!("invalid Hashline anchor {source}: {err}"))
    })?;
    if line_number == 0 {
        return Err(FunctionCallError::RespondToModel(format!(
            "invalid Hashline anchor {source}: line numbers are 1-indexed"
        )));
    }
    Ok(line_number)
}

fn split_hashline_operation(input: &str) -> Result<(&str, &str), FunctionCallError> {
    let line = input.trim_start();
    let op_end = line
        .find(|ch: char| ch.is_whitespace() || ch == '|' || ch == ':')
        .unwrap_or(line.len());
    let (op, rest) = line.split_at(op_end);
    if op.is_empty() {
        return Err(invalid_operation_error(line));
    }
    let rest = if rest.starts_with('|') || rest.starts_with(':') {
        rest
    } else {
        rest.trim_start()
    };
    Ok((op, rest))
}

fn invalid_operation_error(line: &str) -> FunctionCallError {
    FunctionCallError::RespondToModel(format!(
        "invalid Hashline operation {line}; expected forms like SWAP 12:abcd:\n+text, SWAP 12:abcd|text, DEL 12:abcd, INS.POST 12:abcd:\n+text, or INS.TAIL:\n+text"
    ))
}
