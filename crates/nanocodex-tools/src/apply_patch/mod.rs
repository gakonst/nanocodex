use std::{
    fs,
    path::{Path, PathBuf},
};

use nanocodex_core::ToolDefinition;
use serde_json::json;

use super::{StandardTool, Tool, ToolContext, ToolExecution, ToolInput, ToolResult};

mod parser;
mod seek_sequence;
mod streaming_parser;

use parser::{Hunk, UpdateFileChunk, parse_patch};

pub(super) struct ApplyPatchHandler {
    workspace: PathBuf,
}

impl ApplyPatchHandler {
    pub(super) fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[async_trait::async_trait]
impl Tool for ApplyPatchHandler {
    fn name(&self) -> &'static str {
        "apply_patch"
    }

    fn definition(&self) -> ToolDefinition {
        StandardTool::ApplyPatch.definition()
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let input = input.into_freeform()?;
        let workspace = self.workspace.clone();
        Ok(
            match tokio::task::spawn_blocking(move || apply(&input, &workspace)).await {
                Ok(Ok(output)) => ToolExecution::text(output).with_code_mode_value(json!({})),
                Ok(Err(error)) => ToolExecution::error(error),
                Err(error) => ToolExecution::error(format!("apply_patch task failed: {error}")),
            },
        )
    }
}

#[derive(Debug, PartialEq)]
struct ApplyPatchArgs {
    patch: String,
    hunks: Vec<Hunk>,
    workdir: Option<String>,
    environment_id: Option<String>,
}

pub(super) fn apply(patch: &str, workspace: &Path) -> Result<String, String> {
    let ApplyPatchArgs {
        hunks,
        patch: _,
        workdir: _,
        environment_id: _,
    } = parse_patch(patch).map_err(|error| error.to_string())?;
    if hunks.is_empty() {
        return Err("No files were modified.".to_owned());
    }

    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();

    for hunk in hunks {
        let affected_path = hunk.path().to_path_buf();
        match hunk {
            Hunk::AddFile { path, contents } => {
                let target = resolve(workspace, &path);
                write_file(&target, contents.as_bytes())?;
                added.push(affected_path);
            }
            Hunk::DeleteFile { path } => {
                let target = resolve(workspace, &path);
                fs::remove_file(&target).map_err(|error| {
                    format!("Failed to delete file {}: {error}", target.display())
                })?;
                deleted.push(affected_path);
            }
            Hunk::UpdateFile {
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
                    modified.push(affected_path);
                } else {
                    write_file(&source, updated.as_bytes())?;
                    modified.push(affected_path);
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

fn apply_chunks(original: &str, chunks: &[UpdateFileChunk], path: &Path) -> Result<String, String> {
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
        if let Some(context) = &chunk.change_context {
            let context = [context.clone()];
            let found = seek_sequence::seek_sequence(&original_lines, &context, line_index, false)
                .ok_or_else(|| {
                    format!(
                        "Failed to find context '{}' in {}",
                        context[0],
                        path.display()
                    )
                })?;
            line_index = found + 1;
        }

        if chunk.old_lines.is_empty() {
            let insertion_index = if original_lines.last().is_some_and(String::is_empty) {
                original_lines.len() - 1
            } else {
                original_lines.len()
            };
            replacements.push((insertion_index, 0, chunk.new_lines.clone()));
            continue;
        }

        let mut old_lines = chunk.old_lines.as_slice();
        let mut new_lines = chunk.new_lines.as_slice();
        let mut found = seek_sequence::seek_sequence(
            &original_lines,
            old_lines,
            line_index,
            chunk.is_end_of_file,
        );
        if found.is_none() && old_lines.last().is_some_and(String::is_empty) {
            old_lines = &old_lines[..old_lines.len() - 1];
            if new_lines.last().is_some_and(String::is_empty) {
                new_lines = &new_lines[..new_lines.len() - 1];
            }
            found = seek_sequence::seek_sequence(
                &original_lines,
                old_lines,
                line_index,
                chunk.is_end_of_file,
            );
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
    if !original_lines.last().is_some_and(String::is_empty) {
        original_lines.push(String::new());
    }
    Ok(original_lines.join("\n"))
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

#[cfg(test)]
mod tests {
    use super::apply;

    fn test_root(name: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "nanocodex-apply-patch-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&root)?;
        Ok(root)
    }

    #[test]
    fn applies_add_update_move_and_delete() -> Result<(), Box<dyn std::error::Error>> {
        let root = test_root("basic")?;
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

    #[test]
    fn accepts_codex_lenient_heredoc_form() -> Result<(), Box<dyn std::error::Error>> {
        let root = test_root("heredoc")?;
        let output = apply(
            "<<'EOF'\n*** Begin Patch\n*** Add File: added.txt\n+added\n*** End Patch\nEOF",
            &root,
        )?;

        assert_eq!(std::fs::read_to_string(root.join("added.txt"))?, "added\n");
        assert!(output.contains("A added.txt"));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn matches_codex_unicode_context_normalization() -> Result<(), Box<dyn std::error::Error>> {
        let root = test_root("unicode")?;
        std::fs::write(root.join("unicode.txt"), "Before — “quoted”\n")?;

        apply(
            "*** Begin Patch\n*** Update File: unicode.txt\n@@\n-Before - \"quoted\"\n+After\n*** End Patch",
            &root,
        )?;

        assert_eq!(
            std::fs::read_to_string(root.join("unicode.txt"))?,
            "After\n"
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn rejects_empty_update_with_codex_error() -> Result<(), Box<dyn std::error::Error>> {
        let root = test_root("empty-update")?;
        let error = apply(
            "*** Begin Patch\n*** Update File: file.txt\n*** End Patch",
            &root,
        )
        .expect_err("empty update should fail");

        assert_eq!(
            error,
            "invalid hunk at line 2, Update file hunk for path 'file.txt' is empty"
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
