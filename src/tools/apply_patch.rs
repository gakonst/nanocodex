use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolExecution, ToolFuture, ToolHandler};

pub(super) struct ApplyPatchHandler {
    workspace: PathBuf,
}

impl ApplyPatchHandler {
    pub(super) fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

impl ToolHandler for ApplyPatchHandler {
    fn name(&self) -> &'static str {
        "apply_patch"
    }

    fn spec(&self) -> Value {
        json!({
            "type": "function",
            "name": self.name(),
            "description": "Applies a patch to files in the task workspace.",
            "strict": false,
            "parameters": {
                "type": "object",
                "properties": {
                    "patch": {
                        "type": "string",
                        "description": "Patch text beginning with `*** Begin Patch` and ending with `*** End Patch`."
                    }
                },
                "required": ["patch"],
                "additionalProperties": false
            }
        })
    }

    fn execute(&self, input: String) -> ToolFuture<'_> {
        let workspace = self.workspace.clone();
        Box::pin(async move {
            let arguments = match serde_json::from_str::<ApplyPatchArguments>(&input) {
                Ok(arguments) => arguments,
                Err(error) => {
                    return ToolExecution::error(format!(
                        "failed to parse apply_patch arguments: {error}"
                    ));
                }
            };
            match tokio::task::spawn_blocking(move || apply(&arguments.patch, &workspace)).await {
                Ok(Ok(output)) => ToolExecution::text(output).with_code_mode_value(json!({})),
                Ok(Err(error)) => ToolExecution::error(error),
                Err(error) => ToolExecution::error(format!("apply_patch task failed: {error}")),
            }
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPatchArguments {
    patch: String,
}

const BEGIN_PATCH: &str = "*** Begin Patch";
const END_PATCH: &str = "*** End Patch";
const ADD_FILE: &str = "*** Add File: ";
const DELETE_FILE: &str = "*** Delete File: ";
const UPDATE_FILE: &str = "*** Update File: ";
const MOVE_TO: &str = "*** Move to: ";
const END_OF_FILE: &str = "*** End of File";

#[derive(Debug)]
enum Hunk {
    Add {
        path: PathBuf,
        contents: String,
    },
    Delete {
        path: PathBuf,
    },
    Update {
        path: PathBuf,
        move_path: Option<PathBuf>,
        chunks: Vec<Chunk>,
    },
}

#[derive(Debug)]
struct Chunk {
    context: Option<String>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    end_of_file: bool,
}

pub(super) fn apply(patch: &str, workspace: &Path) -> Result<String, String> {
    let hunks = parse(patch)?;
    if hunks.is_empty() {
        return Err("No files were modified.".to_owned());
    }

    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();

    for hunk in hunks {
        match hunk {
            Hunk::Add { path, contents } => {
                let target = resolve(workspace, &path);
                write_file(&target, contents.as_bytes())?;
                added.push(path);
            }
            Hunk::Delete { path } => {
                let target = resolve(workspace, &path);
                fs::remove_file(&target).map_err(|error| {
                    format!("Failed to delete file {}: {error}", target.display())
                })?;
                deleted.push(path);
            }
            Hunk::Update {
                path,
                move_path,
                chunks,
            } => {
                let source = resolve(workspace, &path);
                let original = fs::read_to_string(&source).map_err(|error| {
                    format!(
                        "Failed to read file to update {}: {error}",
                        source.display()
                    )
                })?;
                let updated = apply_chunks(&original, &chunks, &source)?;
                if let Some(move_path) = move_path {
                    let destination = resolve(workspace, &move_path);
                    write_file(&destination, updated.as_bytes())?;
                    fs::remove_file(&source).map_err(|error| {
                        format!("Failed to remove original {}: {error}", source.display())
                    })?;
                    modified.push(move_path);
                } else {
                    write_file(&source, updated.as_bytes())?;
                    modified.push(path);
                }
            }
        }
    }

    let mut summary = String::from("Success. Updated the following files:\n");
    for path in added {
        push_summary_line(&mut summary, 'A', &path);
    }
    for path in modified {
        push_summary_line(&mut summary, 'M', &path);
    }
    for path in deleted {
        push_summary_line(&mut summary, 'D', &path);
    }
    Ok(summary)
}

fn parse(patch_text: &str) -> Result<Vec<Hunk>, String> {
    let normalized = patch_text.replace("\r\n", "\n");
    let lines = normalized.trim().lines().collect::<Vec<_>>();
    if lines.first().map(|line| line.trim()) != Some(BEGIN_PATCH) {
        return Err(
            "Invalid patch: The first line of the patch must be '*** Begin Patch'".to_owned(),
        );
    }
    if lines.last().map(|line| line.trim()) != Some(END_PATCH) {
        return Err("Invalid patch: The last line of the patch must be '*** End Patch'".to_owned());
    }

    let mut hunks = Vec::new();
    let mut index = 1;
    while index + 1 < lines.len() {
        let line = lines[index].trim();
        if let Some(path_text) = line.strip_prefix(ADD_FILE) {
            hunks.push(parse_add_hunk(&lines, &mut index, path_text)?);
            continue;
        }
        if let Some(path_text) = line.strip_prefix(DELETE_FILE) {
            hunks.push(Hunk::Delete {
                path: required_path(path_text, index)?,
            });
            index += 1;
            continue;
        }
        if let Some(path_text) = line.strip_prefix(UPDATE_FILE) {
            hunks.push(parse_update_hunk(&lines, &mut index, path_text)?);
            continue;
        }
        return Err(invalid_hunk(
            index,
            "expected an add, delete, or update file header",
        ));
    }
    Ok(hunks)
}

fn parse_add_hunk(lines: &[&str], index: &mut usize, path_text: &str) -> Result<Hunk, String> {
    let path = required_path(path_text, *index)?;
    *index += 1;
    let mut contents = String::new();
    while *index + 1 < lines.len() && !is_hunk_header(lines[*index]) {
        let added = lines[*index]
            .strip_prefix('+')
            .ok_or_else(|| invalid_hunk(*index, "every added-file line must start with '+'"))?;
        contents.push_str(added);
        contents.push('\n');
        *index += 1;
    }
    if contents.is_empty() {
        return Err(invalid_hunk(
            *index,
            "add-file hunk must contain at least one line",
        ));
    }
    Ok(Hunk::Add { path, contents })
}

fn parse_update_hunk(lines: &[&str], index: &mut usize, path_text: &str) -> Result<Hunk, String> {
    let path = required_path(path_text, *index)?;
    *index += 1;
    let move_path = lines
        .get(*index)
        .and_then(|line| line.trim_end().strip_prefix(MOVE_TO).map(ToOwned::to_owned));
    let move_path = match move_path {
        Some(destination) => {
            let destination = required_path(&destination, *index)?;
            *index += 1;
            Some(destination)
        }
        None => None,
    };
    let mut chunks = Vec::new();
    while *index + 1 < lines.len() && !is_hunk_header(lines[*index]) {
        parse_update_line(lines[*index], *index, &mut chunks)?;
        *index += 1;
    }
    if chunks.is_empty()
        || chunks
            .iter()
            .any(|chunk| chunk.old_lines.is_empty() && chunk.new_lines.is_empty())
    {
        return Err(invalid_hunk(*index, "update-file hunk is empty"));
    }
    Ok(Hunk::Update {
        path,
        move_path,
        chunks,
    })
}

fn parse_update_line(line: &str, index: usize, chunks: &mut Vec<Chunk>) -> Result<(), String> {
    let trimmed = line.trim_end();
    if trimmed == "@@" || trimmed.starts_with("@@ ") {
        chunks.push(Chunk {
            context: trimmed.strip_prefix("@@ ").map(ToOwned::to_owned),
            old_lines: Vec::new(),
            new_lines: Vec::new(),
            end_of_file: false,
        });
        return Ok(());
    }
    if trimmed == END_OF_FILE {
        let chunk = chunks
            .last_mut()
            .ok_or_else(|| invalid_hunk(index, "end-of-file marker must follow changed lines"))?;
        chunk.end_of_file = true;
        return Ok(());
    }
    if chunks.is_empty() {
        chunks.push(Chunk {
            context: None,
            old_lines: Vec::new(),
            new_lines: Vec::new(),
            end_of_file: false,
        });
    }
    let Some(chunk) = chunks.last_mut() else {
        return Err(invalid_hunk(index, "update-file hunk is empty"));
    };
    if let Some(value) = line.strip_prefix('+') {
        chunk.new_lines.push(value.to_owned());
    } else if let Some(value) = line.strip_prefix('-') {
        chunk.old_lines.push(value.to_owned());
    } else if let Some(value) = line.strip_prefix(' ') {
        chunk.old_lines.push(value.to_owned());
        chunk.new_lines.push(value.to_owned());
    } else if line.is_empty() {
        chunk.old_lines.push(String::new());
        chunk.new_lines.push(String::new());
    } else {
        return Err(invalid_hunk(
            index,
            "every update line must start with ' ', '+', or '-'",
        ));
    }
    Ok(())
}

fn apply_chunks(original: &str, chunks: &[Chunk], path: &Path) -> Result<String, String> {
    let mut original_lines = original
        .split('\n')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if original_lines.last().is_some_and(String::is_empty) {
        original_lines.pop();
    }

    let mut replacements = Vec::new();
    let mut line_index = 0;
    for chunk in chunks {
        if let Some(context) = &chunk.context {
            let context = [context.clone()];
            let found =
                seek_sequence(&original_lines, &context, line_index, false).ok_or_else(|| {
                    format!(
                        "Failed to find context '{}' in {}",
                        context[0],
                        path.display()
                    )
                })?;
            line_index = found + 1;
        }

        if chunk.old_lines.is_empty() {
            replacements.push((original_lines.len(), 0, chunk.new_lines.clone()));
            continue;
        }

        let mut old_lines = chunk.old_lines.as_slice();
        let mut new_lines = chunk.new_lines.as_slice();
        let mut found = seek_sequence(&original_lines, old_lines, line_index, chunk.end_of_file);
        if found.is_none() && old_lines.last().is_some_and(String::is_empty) {
            old_lines = &old_lines[..old_lines.len() - 1];
            if new_lines.last().is_some_and(String::is_empty) {
                new_lines = &new_lines[..new_lines.len() - 1];
            }
            found = seek_sequence(&original_lines, old_lines, line_index, chunk.end_of_file);
        }
        let found = found.ok_or_else(|| {
            format!(
                "Failed to find expected lines in {}:\n{}",
                path.display(),
                chunk.old_lines.join("\n")
            )
        })?;
        replacements.push((found, old_lines.len(), new_lines.to_vec()));
        line_index = found + old_lines.len();
    }

    replacements.sort_by_key(|(start, _, _)| *start);
    for (start, old_len, new_lines) in replacements.into_iter().rev() {
        original_lines.splice(start..start + old_len, new_lines);
    }
    original_lines.push(String::new());
    Ok(original_lines.join("\n"))
}

fn seek_sequence(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start.min(lines.len()));
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let last_start = lines.len() - pattern.len();
    if !eof && start > last_start {
        return None;
    }
    let first = if eof { last_start } else { start };
    let range = first..=last_start;
    for index in range.clone() {
        if lines[index..index + pattern.len()] == *pattern {
            return Some(index);
        }
    }
    for index in range.clone() {
        if lines[index..index + pattern.len()]
            .iter()
            .zip(pattern)
            .all(|(line, expected)| line.trim_end() == expected.trim_end())
        {
            return Some(index);
        }
    }
    range.into_iter().find(|&index| {
        lines[index..index + pattern.len()]
            .iter()
            .zip(pattern)
            .all(|(line, expected)| line.trim() == expected.trim())
    })
}

fn resolve(workspace: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        workspace.join(path)
    }
}

fn write_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create parent directories for {}: {error}",
                path.display()
            )
        })?;
    }
    fs::write(path, contents)
        .map_err(|error| format!("Failed to write file {}: {error}", path.display()))
}

fn push_summary_line(summary: &mut String, operation: char, path: &Path) {
    summary.push(operation);
    summary.push(' ');
    summary.push_str(&path.to_string_lossy());
    summary.push('\n');
}

fn required_path(path: &str, line: usize) -> Result<PathBuf, String> {
    let path = path.trim();
    if path.is_empty() {
        Err(invalid_hunk(line, "file path cannot be empty"))
    } else {
        Ok(PathBuf::from(path))
    }
}

fn is_hunk_header(line: &str) -> bool {
    let line = line.trim();
    line == END_PATCH
        || line.starts_with(ADD_FILE)
        || line.starts_with(DELETE_FILE)
        || line.starts_with(UPDATE_FILE)
}

fn invalid_hunk(index: usize, message: &str) -> String {
    format!("Invalid patch hunk on line {}: {message}", index + 1)
}

#[cfg(test)]
mod tests {
    use super::apply;

    #[test]
    fn applies_add_update_move_and_delete() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "harness-apply-patch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&root)?;
        std::fs::write(root.join("old.txt"), "one\ntwo\n")?;
        std::fs::write(root.join("gone.txt"), "gone\n")?;

        let output = apply(
            "*** Begin Patch\n*** Add File: added.txt\n+added\n*** Update File: old.txt\n*** Move to: moved.txt\n@@\n-one\n+ONE\n two\n*** Delete File: gone.txt\n*** End Patch",
            &root,
        )?;

        assert_eq!(std::fs::read_to_string(root.join("added.txt"))?, "added\n");
        assert_eq!(
            std::fs::read_to_string(root.join("moved.txt"))?,
            "ONE\ntwo\n"
        );
        assert!(!root.join("old.txt").exists());
        assert!(!root.join("gone.txt").exists());
        assert!(output.contains("A added.txt"));
        assert!(output.contains("M moved.txt"));
        assert!(output.contains("D gone.txt"));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
