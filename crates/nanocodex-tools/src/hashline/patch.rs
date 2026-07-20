#![allow(
    clippy::cast_possible_wrap,
    clippy::similar_names,
    clippy::too_many_lines
)]

use super::FunctionCallError;
use super::take_bytes_at_char_boundary;
use serde::Serialize;

use super::block::find_normalized_block_span;
use super::format::json_escaped_content_len_bounded;
use super::format::split_lines_preserve;
use super::hash::FILE_HASH_WIDTH;
use super::hash::LINE_HASH_WIDTH;
use super::hash::hash_hex;
use super::hash::line_hash;
use super::hash::normalize_file_text;
use super::patch_lines::LineEnding;
use super::patch_lines::SourceLine;
use super::patch_lines::insert_current_lines;
use super::patch_lines::reassemble_file_contents;
use super::patch_lines::replace_current_range;
use super::patch_lines::source_document;
use super::patch_parser::HashlineOperation;
use super::patch_parser::LineAnchor;
use super::patch_parser::LineRange;
pub(super) use super::patch_parser::parse_anchor_hash;
pub(super) use super::patch_parser::parse_anchor_line;
use super::patch_parser::parse_hashline_patch;
use super::patch_sections::validate_patch_headers;

const PATCH_PREVIEW_MAX_LINES: usize = 40;
const PATCH_PREVIEW_MAX_SERIALIZED_BYTES: usize = 4 * 1024;
const PREVIEW_TRUNCATION_MARKER: &str = "... [content truncated]";

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(super) struct HashlinePatchPreview {
    pub(super) old_start_line: Option<usize>,
    pub(super) old_end_line: Option<usize>,
    pub(super) new_start_line: Option<usize>,
    pub(super) new_end_line: Option<usize>,
    pub(super) truncated: bool,
    pub(super) content: String,
}

#[derive(Clone, Copy)]
struct ChangeBounds {
    old_start: usize,
    old_end: usize,
    new_start: usize,
    new_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HashlinePatchFileOperation {
    Remove,
    Rename { new_path: String },
}

pub(super) fn apply_hashline_patch(
    path: &str,
    contents: &str,
    patch: &str,
) -> Result<String, FunctionCallError> {
    if hashline_patch_is_aborted(patch) {
        return Ok(contents.to_string());
    }
    validate_patch_headers(path, contents, patch)?;
    let operations = parse_hashline_patch(patch)?;
    if operations
        .iter()
        .any(|operation| matches!(operation, HashlineOperation::RemoveFile))
    {
        return Err(FunctionCallError::RespondToModel(
            "REM is a file operation; use a sectioned hashline.patch or the dedicated hashline file tools"
                .to_string(),
        ));
    }
    let operations = operations
        .into_iter()
        .filter(|operation| !operation.is_file_operation())
        .collect::<Vec<_>>();
    let normalized_contents = normalize_file_text(contents);
    let mut document = source_document(contents, &normalized_contents);
    let fallback_line_ending = document.fallback_line_ending;

    if operations.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "hashline.patch did not contain any operations".to_string(),
        ));
    }

    apply_operations(
        path,
        &mut document.lines,
        &operations,
        &normalized_contents,
        normalized_contents.ends_with('\n'),
        fallback_line_ending,
    )?;

    let output = reassemble_file_contents(&document);
    Ok(output)
}

pub(super) fn parse_hashline_patch_file_operation(
    patch: &str,
) -> Result<Option<HashlinePatchFileOperation>, FunctionCallError> {
    let operations = parse_hashline_patch(patch)?;
    let mut file_operation = None;
    let mut has_line_operation = false;
    for operation in operations {
        match operation {
            HashlineOperation::RemoveFile => {
                set_file_operation(&mut file_operation, HashlinePatchFileOperation::Remove)?;
            }
            HashlineOperation::RenameFile { new_path } => {
                set_file_operation(
                    &mut file_operation,
                    HashlinePatchFileOperation::Rename { new_path },
                )?;
            }
            HashlineOperation::Swap { .. }
            | HashlineOperation::Delete { .. }
            | HashlineOperation::InsertBefore { .. }
            | HashlineOperation::InsertAfter { .. }
            | HashlineOperation::InsertHead { .. }
            | HashlineOperation::InsertTail { .. }
            | HashlineOperation::SwapBlock { .. }
            | HashlineOperation::DeleteBlock { .. }
            | HashlineOperation::InsertBlockBefore { .. }
            | HashlineOperation::InsertBlockAfter { .. } => {
                has_line_operation = true;
            }
        }
    }
    if matches!(file_operation, Some(HashlinePatchFileOperation::Remove)) && has_line_operation {
        return Err(FunctionCallError::RespondToModel(
            "Hashline file operation REM cannot be combined with line operations in the same file section"
                .to_string(),
        ));
    }
    Ok(file_operation)
}

pub(super) fn hashline_patch_has_line_operations(patch: &str) -> Result<bool, FunctionCallError> {
    let operations = parse_hashline_patch(patch)?;
    Ok(operations
        .iter()
        .any(|operation| !operation.is_file_operation()))
}

fn set_file_operation(
    file_operation: &mut Option<HashlinePatchFileOperation>,
    next_operation: HashlinePatchFileOperation,
) -> Result<(), FunctionCallError> {
    if let Some(previous_operation) = file_operation {
        return Err(FunctionCallError::RespondToModel(format!(
            "only one Hashline file operation is allowed per file section; found {previous_operation:?} and {next_operation:?}"
        )));
    }
    *file_operation = Some(next_operation);
    Ok(())
}

pub(super) fn validate_file_hash(
    path: &str,
    contents: &str,
    expected_hash: &str,
) -> Result<(), FunctionCallError> {
    if expected_hash.len() != FILE_HASH_WIDTH
        || !expected_hash.chars().all(|ch| ch.is_ascii_hexdigit())
    {
        return Err(FunctionCallError::RespondToModel(format!(
            "invalid file hash for {path}: expected a {FILE_HASH_WIDTH}-hex Hashline file hash, got {expected_hash}"
        )));
    }
    let actual_hash = hash_hex(contents);
    if !expected_hash.eq_ignore_ascii_case(&actual_hash) {
        return Err(FunctionCallError::RespondToModel(format!(
            "file hash mismatch for {path}: expected {expected_hash}, found {actual_hash}; the file changed since it was read, so reread it with hashline.read and rebuild the patch from the refreshed anchors before retrying"
        )));
    }
    Ok(())
}

pub(super) fn hashline_patch_is_aborted(patch: &str) -> bool {
    patch
        .lines()
        .any(|line| line.trim_end_matches('\r').trim_end() == "*** Abort")
}

fn apply_operations(
    path: &str,
    lines: &mut Vec<SourceLine>,
    operations: &[HashlineOperation],
    original_contents: &str,
    original_has_final_newline: bool,
    fallback_line_ending: LineEnding,
) -> Result<(), FunctionCallError> {
    let original_lines = split_lines_preserve(original_contents)
        .into_iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let block_spans = operations
        .iter()
        .map(|operation| match operation {
            HashlineOperation::SwapBlock { anchor, .. }
            | HashlineOperation::DeleteBlock { anchor }
            | HashlineOperation::InsertBlockBefore { anchor, .. }
            | HashlineOperation::InsertBlockAfter { anchor, .. } => {
                block_span(path, &original_lines, anchor.line.line).map(Some)
            }
            _ => Ok(None),
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_block_operation_order(operations, &block_spans)?;
    let mut shifts = vec![0_isize; original_lines.len()];
    let mut deleted = vec![false; original_lines.len()];

    for (operation_index, operation) in operations.iter().enumerate() {
        match operation {
            HashlineOperation::Swap { range, replacement } => {
                validate_range(&original_lines, &deleted, range)?;
                let start_index = adjusted_index(range.start.line, &shifts)?;
                let replaced_count = range.end.line - range.start.line + 1;
                replace_current_range(
                    lines,
                    start_index,
                    replaced_count,
                    replacement,
                    fallback_line_ending,
                )?;
                mark_deleted(&mut deleted, range.start.line, range.end.line);
                apply_delta_after(
                    &mut shifts,
                    range.end.line,
                    replacement.len() as isize - replaced_count as isize,
                );
            }
            HashlineOperation::Delete { range } => {
                validate_range(&original_lines, &deleted, range)?;
                let start_index = adjusted_index(range.start.line, &shifts)?;
                let deleted_count = range.end.line - range.start.line + 1;
                replace_current_range(
                    lines,
                    start_index,
                    deleted_count,
                    &[],
                    fallback_line_ending,
                )?;
                mark_deleted(&mut deleted, range.start.line, range.end.line);
                apply_delta_after(&mut shifts, range.end.line, -(deleted_count as isize));
            }
            HashlineOperation::InsertBefore { anchor, inserted } => {
                validate_anchor(&original_lines, &deleted, anchor)?;
                let index = adjusted_index(anchor.line, &shifts)?;
                insert_current_lines(
                    lines,
                    index,
                    inserted,
                    original_has_final_newline,
                    fallback_line_ending,
                );
                apply_delta_from(&mut shifts, anchor.line, inserted.len() as isize);
            }
            HashlineOperation::InsertAfter { anchor, inserted } => {
                validate_anchor(&original_lines, &deleted, anchor)?;
                let index = adjusted_index(anchor.line, &shifts)?.saturating_add(1);
                insert_current_lines(
                    lines,
                    index,
                    inserted,
                    original_has_final_newline,
                    fallback_line_ending,
                );
                apply_delta_after(&mut shifts, anchor.line, inserted.len() as isize);
            }
            HashlineOperation::InsertHead { inserted } => {
                insert_current_lines(
                    lines,
                    0,
                    inserted,
                    original_has_final_newline,
                    fallback_line_ending,
                );
                apply_delta_from(&mut shifts, 1, inserted.len() as isize);
            }
            HashlineOperation::InsertTail { inserted } => {
                let index = lines.len();
                insert_current_lines(
                    lines,
                    index,
                    inserted,
                    original_has_final_newline,
                    fallback_line_ending,
                );
            }
            HashlineOperation::SwapBlock {
                anchor,
                replacement,
            } => {
                validate_anchor(&original_lines, &deleted, &anchor.line)?;
                let original_span = required_block_span(&block_spans, operation_index)?;
                validate_block_hash(&original_lines, original_span, &anchor.expected_hash)?;
                validate_not_deleted(&deleted, original_span.0, original_span.1)?;
                let current_span = adjusted_block_span(original_span, &shifts, lines.len())?;
                replace_current_range(
                    lines,
                    current_span.0,
                    current_span.1 - current_span.0 + 1,
                    replacement,
                    fallback_line_ending,
                )?;
                mark_deleted(&mut deleted, original_span.0, original_span.1);
                apply_delta_after(
                    &mut shifts,
                    original_span.1,
                    replacement.len() as isize - (original_span.1 - original_span.0 + 1) as isize,
                );
            }
            HashlineOperation::DeleteBlock { anchor } => {
                validate_anchor(&original_lines, &deleted, &anchor.line)?;
                let original_span = required_block_span(&block_spans, operation_index)?;
                validate_block_hash(&original_lines, original_span, &anchor.expected_hash)?;
                validate_not_deleted(&deleted, original_span.0, original_span.1)?;
                let current_span = adjusted_block_span(original_span, &shifts, lines.len())?;
                replace_current_range(
                    lines,
                    current_span.0,
                    current_span.1 - current_span.0 + 1,
                    &[],
                    fallback_line_ending,
                )?;
                mark_deleted(&mut deleted, original_span.0, original_span.1);
                apply_delta_after(
                    &mut shifts,
                    original_span.1,
                    -((original_span.1 - original_span.0 + 1) as isize),
                );
            }
            HashlineOperation::InsertBlockBefore { anchor, inserted } => {
                validate_anchor(&original_lines, &deleted, &anchor.line)?;
                let original_span = required_block_span(&block_spans, operation_index)?;
                validate_block_hash(&original_lines, original_span, &anchor.expected_hash)?;
                validate_not_deleted(&deleted, original_span.0, original_span.1)?;
                let current_span = adjusted_block_span(original_span, &shifts, lines.len())?;
                insert_current_lines(
                    lines,
                    current_span.0,
                    inserted,
                    original_has_final_newline,
                    fallback_line_ending,
                );
                apply_delta_from(&mut shifts, original_span.0, inserted.len() as isize);
            }
            HashlineOperation::InsertBlockAfter { anchor, inserted } => {
                validate_anchor(&original_lines, &deleted, &anchor.line)?;
                let original_span = required_block_span(&block_spans, operation_index)?;
                validate_block_hash(&original_lines, original_span, &anchor.expected_hash)?;
                validate_not_deleted(&deleted, original_span.0, original_span.1)?;
                let current_span = adjusted_block_span(original_span, &shifts, lines.len())?;
                insert_current_lines(
                    lines,
                    current_span.1.saturating_add(1),
                    inserted,
                    original_has_final_newline,
                    fallback_line_ending,
                );
                apply_delta_after(&mut shifts, original_span.1, inserted.len() as isize);
            }
            HashlineOperation::RemoveFile | HashlineOperation::RenameFile { .. } => {
                return Err(FunctionCallError::RespondToModel(
                    "REM and MV are file operations, not line operations".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_block_operation_order(
    operations: &[HashlineOperation],
    block_spans: &[Option<(usize, usize)>],
) -> Result<(), FunctionCallError> {
    for (operation_index, span) in block_spans.iter().enumerate() {
        let Some(span) = span else {
            continue;
        };
        for (previous_index, previous) in operations[..operation_index].iter().enumerate() {
            let changes_target = match previous {
                HashlineOperation::Swap { range, .. } | HashlineOperation::Delete { range } => {
                    range.start.line <= span.1 && span.0 <= range.end.line
                }
                HashlineOperation::InsertBefore { anchor, .. }
                | HashlineOperation::InsertAfter { anchor, .. } => {
                    (span.0..=span.1).contains(&anchor.line)
                }
                HashlineOperation::SwapBlock { .. }
                | HashlineOperation::DeleteBlock { .. }
                | HashlineOperation::InsertBlockBefore { .. }
                | HashlineOperation::InsertBlockAfter { .. } => block_spans[previous_index]
                    .is_some_and(|previous_span| {
                        previous_span.0 <= span.1 && span.0 <= previous_span.1
                    }),
                HashlineOperation::InsertHead { .. }
                | HashlineOperation::InsertTail { .. }
                | HashlineOperation::RemoveFile
                | HashlineOperation::RenameFile { .. } => false,
            };
            if changes_target {
                return Err(FunctionCallError::RespondToModel(format!(
                    "Hashline block operation {} cannot follow an earlier operation that changes its original block; apply the block operation in a separate patch",
                    operation_index + 1
                )));
            }
        }
    }
    Ok(())
}

fn required_block_span(
    block_spans: &[Option<(usize, usize)>],
    operation_index: usize,
) -> Result<(usize, usize), FunctionCallError> {
    block_spans
        .get(operation_index)
        .copied()
        .flatten()
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "Hashline block operation is missing its original span".to_string(),
            )
        })
}

fn adjusted_block_span(
    original_span: (usize, usize),
    shifts: &[isize],
    current_len: usize,
) -> Result<(usize, usize), FunctionCallError> {
    let start = adjusted_index(original_span.0, shifts)?;
    let end = adjusted_index(original_span.1, shifts)?;
    if start > end || end >= current_len {
        return Err(FunctionCallError::RespondToModel(
            "Hashline block operation no longer maps to the current file contents".to_string(),
        ));
    }
    Ok((start, end))
}

fn block_span(
    path: &str,
    lines: &[impl AsRef<str>],
    anchor_line: usize,
) -> Result<(usize, usize), FunctionCallError> {
    if anchor_line == 0 || anchor_line > lines.len().max(1) {
        return Err(FunctionCallError::RespondToModel(format!(
            "block anchor line {anchor_line} is outside file range 1..={}",
            lines.len()
        )));
    }
    let refs = lines.iter().map(AsRef::as_ref).collect::<Vec<_>>();
    Ok(find_normalized_block_span(path, &refs, anchor_line))
}

fn validate_block_hash(
    lines: &[String],
    span: (usize, usize),
    expected_hash: &str,
) -> Result<(), FunctionCallError> {
    let actual_hash = hash_hex(&lines[span.0 - 1..span.1].join("\n"));
    if actual_hash != expected_hash {
        return Err(FunctionCallError::RespondToModel(format!(
            "block hash mismatch: expected {expected_hash}, found {actual_hash}; reread the block with hashline.find_block and rebuild the patch from the refreshed anchor"
        )));
    }
    Ok(())
}

fn validate_range(
    original_lines: &[String],
    deleted: &[bool],
    range: &LineRange,
) -> Result<(), FunctionCallError> {
    validate_anchor(original_lines, deleted, &range.start)?;
    if range.end.line > original_lines.len() {
        return Err(FunctionCallError::RespondToModel(format!(
            "line {} is outside file range 1..={}",
            range.end.line,
            original_lines.len()
        )));
    }
    for line in range.start.line..=range.end.line {
        validate_not_deleted(deleted, line, line)?;
    }
    if range.end.line != range.start.line {
        validate_line_hash(original_lines, range.end.line, &range.end.expected_hash)?;
    }
    Ok(())
}

fn validate_not_deleted(
    deleted: &[bool],
    start_line: usize,
    end_line: usize,
) -> Result<(), FunctionCallError> {
    for line in start_line..=end_line {
        if deleted[line - 1] {
            return Err(FunctionCallError::RespondToModel(format!(
                "line {line} has already been deleted by an earlier Hashline operation"
            )));
        }
    }
    Ok(())
}

fn validate_anchor(
    original_lines: &[String],
    deleted: &[bool],
    anchor: &LineAnchor,
) -> Result<(), FunctionCallError> {
    validate_line_hash(original_lines, anchor.line, &anchor.expected_hash)?;
    if deleted[anchor.line - 1] {
        return Err(FunctionCallError::RespondToModel(format!(
            "line {} has already been deleted by an earlier Hashline operation",
            anchor.line
        )));
    }
    Ok(())
}

fn adjusted_index(line_number: usize, shifts: &[isize]) -> Result<usize, FunctionCallError> {
    let shift = shifts.get(line_number - 1).copied().ok_or_else(|| {
        FunctionCallError::RespondToModel(format!(
            "line {line_number} is outside file range 1..={}",
            shifts.len()
        ))
    })?;
    let index = (line_number as isize - 1) + shift;
    usize::try_from(index).map_err(|_| {
        FunctionCallError::RespondToModel(format!(
            "line {line_number} has shifted before the start of the file"
        ))
    })
}

fn mark_deleted(deleted: &mut [bool], start_line: usize, end_line: usize) {
    for line in start_line..=end_line {
        deleted[line - 1] = true;
    }
}

fn apply_delta_from(shifts: &mut [isize], start_line: usize, delta: isize) {
    for shift in shifts.iter_mut().skip(start_line.saturating_sub(1)) {
        *shift += delta;
    }
}

fn apply_delta_after(shifts: &mut [isize], line_number: usize, delta: isize) {
    for shift in shifts.iter_mut().skip(line_number) {
        *shift += delta;
    }
}

pub(super) fn build_hashline_patch_preview(
    old_contents: &str,
    new_contents: &str,
) -> Result<HashlinePatchPreview, FunctionCallError> {
    let normalized_old_contents = normalize_file_text(old_contents);
    let normalized_new_contents = normalize_file_text(new_contents);
    let old_lines = split_lines_preserve(&normalized_old_contents);
    let new_lines = split_lines_preserve(&normalized_new_contents);
    let bounds = change_bounds(&old_lines, &new_lines);
    if bounds.old_start == bounds.old_end && bounds.new_start == bounds.new_end {
        return Err(FunctionCallError::RespondToModel(
            "hashline.patch did not change file contents".to_string(),
        ));
    }

    let total_changed_lines = bounds.old_end.saturating_sub(bounds.old_start)
        + bounds.new_end.saturating_sub(bounds.new_start);
    let mut content = Vec::new();
    let mut serialized_bytes = 0;
    let mut byte_truncated = false;
    for (line_number, line) in old_lines[bounds.old_start..bounds.old_end]
        .iter()
        .enumerate()
    {
        if content.len() == PATCH_PREVIEW_MAX_LINES {
            break;
        }
        let line_number = bounds.old_start + line_number + 1;
        if !push_preview_line(&mut content, &mut serialized_bytes, '-', line_number, line) {
            byte_truncated = true;
            break;
        }
    }
    if !byte_truncated && content.len() < PATCH_PREVIEW_MAX_LINES {
        for (line_number, line) in new_lines[bounds.new_start..bounds.new_end]
            .iter()
            .enumerate()
        {
            if content.len() == PATCH_PREVIEW_MAX_LINES {
                break;
            }
            let line_number = bounds.new_start + line_number + 1;
            if !push_preview_line(&mut content, &mut serialized_bytes, '+', line_number, line) {
                byte_truncated = true;
                break;
            }
        }
    }

    Ok(HashlinePatchPreview {
        old_start_line: span_start(bounds.old_start, bounds.old_end),
        old_end_line: span_end(bounds.old_start, bounds.old_end),
        new_start_line: span_start(bounds.new_start, bounds.new_end),
        new_end_line: span_end(bounds.new_start, bounds.new_end),
        truncated: byte_truncated || total_changed_lines > content.len(),
        content: content.join("\n"),
    })
}

fn push_preview_line(
    content: &mut Vec<String>,
    serialized_bytes: &mut usize,
    prefix: char,
    line_number: usize,
    line: &str,
) -> bool {
    let hash = line_hash(line);
    let remaining = PATCH_PREVIEW_MAX_SERIALIZED_BYTES.saturating_sub(*serialized_bytes);
    if let Some(cost) = preview_line_serialized_cost(line_number, &hash, line, remaining) {
        let formatted = format!("{prefix}{line_number}:{hash}|{line}");
        *serialized_bytes += cost;
        content.push(formatted);
        return true;
    }

    let mut prefix_bytes = line.len().min(remaining / 2);
    loop {
        let line_prefix = take_bytes_at_char_boundary(line, prefix_bytes);
        let formatted =
            format!("{prefix}{line_number}:{hash}|{line_prefix}{PREVIEW_TRUNCATION_MARKER}");
        let cost = serialized_string_cost(&formatted);
        if cost <= remaining {
            content.push(formatted);
            return false;
        }
        if prefix_bytes == 0 {
            return false;
        }
        prefix_bytes /= 2;
    }
}

fn preview_line_serialized_cost(
    line_number: usize,
    hash: &str,
    line: &str,
    limit: usize,
) -> Option<usize> {
    let fixed = 7_usize
        .saturating_add(line_number.checked_ilog10().unwrap_or_default() as usize)
        .saturating_add(hash.len());
    let line_limit = limit.checked_sub(fixed)?;
    let line_len = json_escaped_content_len_bounded(line, line_limit)?;
    fixed.checked_add(line_len)
}

fn serialized_string_cost(value: &str) -> usize {
    serde_json::to_string(value)
        .map_or(usize::MAX, |serialized| serialized.len())
        .saturating_add(2)
}

fn validate_line_hash(
    lines: &[String],
    line_number: usize,
    expected_hash: &str,
) -> Result<(), FunctionCallError> {
    if line_number == 0 || line_number > lines.len() {
        return Err(FunctionCallError::RespondToModel(format!(
            "line {line_number} is outside file range 1..={}",
            lines.len()
        )));
    }
    if expected_hash.len() != LINE_HASH_WIDTH
        || !expected_hash.chars().all(|ch| ch.is_ascii_hexdigit())
    {
        return Err(FunctionCallError::RespondToModel(format!(
            "invalid line hash {expected_hash}; expected a {LINE_HASH_WIDTH}-hex hash"
        )));
    }
    let actual_hash = line_hash(&lines[line_number - 1]);
    if actual_hash != expected_hash {
        return Err(FunctionCallError::RespondToModel(format!(
            "line {line_number} hash mismatch: expected {expected_hash}, found {actual_hash}; the anchor is stale, so reread the file with hashline.read and rebuild the patch from the refreshed anchors before retrying"
        )));
    }
    Ok(())
}
fn change_bounds(old_lines: &[&str], new_lines: &[&str]) -> ChangeBounds {
    let common_prefix = old_lines
        .iter()
        .zip(new_lines)
        .take_while(|(old, new)| old == new)
        .count();
    let remaining_old = old_lines.len().saturating_sub(common_prefix);
    let remaining_new = new_lines.len().saturating_sub(common_prefix);
    let common_suffix = old_lines[common_prefix..]
        .iter()
        .rev()
        .zip(new_lines[common_prefix..].iter().rev())
        .take_while(|(old, new)| old == new)
        .count()
        .min(remaining_old)
        .min(remaining_new);

    ChangeBounds {
        old_start: common_prefix,
        old_end: old_lines.len() - common_suffix,
        new_start: common_prefix,
        new_end: new_lines.len() - common_suffix,
    }
}

fn span_start(start: usize, end: usize) -> Option<usize> {
    (start < end).then_some(start + 1)
}

fn span_end(start: usize, end: usize) -> Option<usize> {
    (start < end).then_some(end)
}
