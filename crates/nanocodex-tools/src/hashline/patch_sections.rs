#![allow(clippy::similar_names, clippy::single_match_else)]

use super::FunctionCallError;

use super::hash::FILE_HASH_WIDTH;
use super::patch::validate_file_hash;
use super::patch_parser::apply_patch_contamination_message;
use super::patch_parser::hashline_operation_has_payload;
use super::patch_parser::is_hashline_operation_line;
use super::patch_parser::is_ignorable_patch_line;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct HashlinePatchSection {
    pub(super) path: String,
    pub(super) expected_hash: Option<String>,
    pub(super) header_line: usize,
    pub(super) patch: String,
}

pub(super) fn split_hashline_patch_sections(
    patch: &str,
) -> Result<Vec<HashlinePatchSection>, FunctionCallError> {
    let raw_lines = patch.lines().collect::<Vec<_>>();
    let mut sections = Vec::<HashlinePatchSection>::new();
    let mut current_index = None;
    let mut payload_active = false;
    for (line_index, raw_line) in raw_lines.iter().enumerate() {
        let line_number = line_index + 1;
        let line = raw_line.trim_end_matches('\r');
        let next_is_operation = raw_lines[line_index + 1..]
            .iter()
            .map(|candidate| candidate.trim_end_matches('\r'))
            .find(|candidate| !candidate.trim().is_empty())
            .is_some_and(is_hashline_operation_line);
        let may_be_header = !payload_active || line.trim().contains("]#") || next_is_operation;
        let header = if may_be_header {
            parse_patch_file_header("", line)?
        } else {
            None
        };
        if let Some((path, expected_hash)) = header {
            let section_index =
                if let Some(index) = sections.iter().position(|section| section.path == path) {
                    merge_section_hash(&mut sections[index], expected_hash)?;
                    index
                } else {
                    sections.push(HashlinePatchSection {
                        path,
                        expected_hash,
                        header_line: line_number,
                        patch: String::new(),
                    });
                    sections.len() - 1
                };
            current_index = Some(section_index);
            payload_active = false;
            continue;
        }

        let Some(section_index) = current_index else {
            if let Some(message) = apply_patch_contamination_message(line) {
                return Err(FunctionCallError::Parser {
                    line: Some(line_number),
                    message,
                });
            }
            if is_ignorable_patch_line(line) {
                continue;
            }
            return Err(FunctionCallError::Parser {
                line: Some(line_number),
                message: "content appears before the first [path] or [path]#HASH section. This tool accepts the Hashline dialect; reread an existing target with hashline__read and copy its header".to_owned(),
            });
        };

        if !sections[section_index].patch.is_empty() {
            sections[section_index].patch.push('\n');
        }
        sections[section_index].patch.push_str(line);
        if is_hashline_operation_line(line) {
            payload_active = hashline_operation_has_payload(line);
        }
    }

    if sections.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "hashline.patch did not contain any file sections".to_string(),
        ));
    }
    Ok(sections)
}

fn parse_patch_file_header(
    target_path: &str,
    line: &str,
) -> Result<Option<(String, Option<String>)>, FunctionCallError> {
    let line = line.trim();
    if !line.starts_with('[') {
        return Ok(None);
    }
    let Some(inner) = line.strip_prefix('[') else {
        unreachable!("checked prefix above");
    };
    let (header_path, expected_hash) =
        if let Some((header_path, expected_hash)) = inner.rsplit_once("]#") {
            if expected_hash.is_empty() {
                return Err(FunctionCallError::RespondToModel(format!(
                    "invalid Hashline file header {line}; expected [{target_path}]#HASH"
                )));
            }
            validate_hash_token(target_path, expected_hash)?;
            (header_path, Some(expected_hash.to_ascii_lowercase()))
        } else if let Some(header_path) = inner.strip_suffix(']') {
            (header_path, None)
        } else {
            return Err(FunctionCallError::RespondToModel(format!(
                "invalid Hashline file header {line}; expected [{target_path}]#HASH"
            )));
        };
    let header_path = strip_apply_patch_path_noise(header_path);
    if header_path.trim().is_empty() {
        return Err(FunctionCallError::RespondToModel(format!(
            "invalid Hashline file header {line}; expected [{target_path}]#HASH"
        )));
    }
    Ok(Some((header_path, expected_hash)))
}
fn strip_apply_patch_path_noise(path_text: &str) -> String {
    let bytes = path_text.as_bytes();
    let mut stripped_stars = 0;
    while stripped_stars < bytes.len() && stripped_stars < 3 && bytes[stripped_stars] == b'*' {
        stripped_stars += 1;
    }

    let stripped = path_text[stripped_stars..].trim_start();
    let stripped_lower = stripped.to_ascii_lowercase();
    for keyword in ["update", "delete", "add", "move"] {
        if !stripped_lower.starts_with(keyword) {
            continue;
        }
        let after_keyword = &stripped[keyword.len()..];
        let after_separator =
            after_keyword.trim_start_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != ':');
        let after_separator_lower = after_separator.to_ascii_lowercase();
        let after_optional_word = if after_separator_lower.starts_with("file") {
            after_separator["file".len()..]
                .trim_start_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != ':')
        } else if after_separator_lower.starts_with("to") {
            after_separator["to".len()..]
                .trim_start_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != ':')
        } else {
            after_separator
        };
        if let Some(path) = after_optional_word.strip_prefix(':') {
            return path.trim_start().to_string();
        }
    }

    stripped.to_string()
}

fn merge_section_hash(
    section: &mut HashlinePatchSection,
    expected_hash: Option<String>,
) -> Result<(), FunctionCallError> {
    let Some(expected_hash) = expected_hash else {
        return Ok(());
    };
    match &section.expected_hash {
        Some(previous_hash) if previous_hash != &expected_hash => {
            Err(FunctionCallError::RespondToModel(format!(
                "conflicting hash tags for {}: {} vs {}",
                section.path, previous_hash, expected_hash
            )))
        }
        None => {
            section.expected_hash = Some(expected_hash);
            Ok(())
        }
        Some(_) => Ok(()),
    }
}

pub(super) fn validate_patch_headers(
    path: &str,
    contents: &str,
    patch: &str,
) -> Result<(), FunctionCallError> {
    let mut section_hash = None;
    let mut payload_active = false;
    for raw_line in patch.lines() {
        let line = raw_line.trim();
        if let Some((header_path, expected_hash)) =
            parse_contextual_patch_file_header(path, line, payload_active)?
        {
            payload_active = false;
            if header_path != path {
                return Err(FunctionCallError::RespondToModel(format!(
                    "Hashline file header path {header_path} does not match target path {path}; this single-file patch application only accepts headers for {path}"
                )));
            }
            let Some(expected_hash) = expected_hash else {
                return Err(FunctionCallError::RespondToModel(format!(
                    "existing-file Hashline patches require a [{path}]#HASH header"
                )));
            };
            match &section_hash {
                Some(previous_hash) if previous_hash != &expected_hash => {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "conflicting hash tags for {path}: {previous_hash} vs {expected_hash}"
                    )));
                }
                None => section_hash = Some(expected_hash),
                Some(_) => {}
            }
            continue;
        }
        if is_hashline_operation_line(line) {
            payload_active = hashline_operation_has_payload(line);
        }
    }
    if let Some(expected_hash) = section_hash {
        validate_file_hash(path, contents, &expected_hash)?;
    }
    Ok(())
}

fn validate_hash_token(path: &str, expected_hash: &str) -> Result<(), FunctionCallError> {
    if expected_hash.len() == FILE_HASH_WIDTH
        && expected_hash.chars().all(|ch| ch.is_ascii_hexdigit())
    {
        return Ok(());
    }
    Err(FunctionCallError::RespondToModel(format!(
        "invalid file hash for {path}: expected a {FILE_HASH_WIDTH}-hex Hashline file hash, got {expected_hash}"
    )))
}

pub(super) fn parse_contextual_patch_file_header(
    target_path: &str,
    line: &str,
    payload_active: bool,
) -> Result<Option<(String, Option<String>)>, FunctionCallError> {
    let trimmed = line.trim();
    let is_strong_header = trimmed.contains("]#")
        || trimmed
            .strip_prefix('[')
            .is_some_and(|inner| inner.starts_with('*'));
    if payload_active && !is_strong_header {
        return Ok(None);
    }
    parse_patch_file_header(target_path, line)
}
