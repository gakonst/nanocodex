use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
};

#[cfg(not(target_family = "wasm"))]
use nanocodex_oai_api::tools::ToolDefinition;
#[cfg(not(target_family = "wasm"))]
use serde_json::json;

#[cfg(not(target_family = "wasm"))]
use super::{StandardTool, Tool, ToolContext, ToolInput, ToolOutput, ToolResult};

mod parser;
mod seek_sequence;
mod streaming_parser;

use parser::{Hunk, UpdateFileChunk, parse_patch};

#[cfg(not(target_family = "wasm"))]
pub(crate) struct ApplyPatchHandler {
    workspace: PathBuf,
}

#[cfg(not(target_family = "wasm"))]
impl ApplyPatchHandler {
    pub(crate) const fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[cfg(not(target_family = "wasm"))]
#[async_trait::async_trait]
impl Tool for ApplyPatchHandler {
    fn definition(&self) -> ToolDefinition {
        StandardTool::ApplyPatch.definition()
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let input = input.into_freeform()?;
        let workspace = self.workspace.clone();
        Ok(
            match tokio::task::spawn_blocking(move || apply(&input, &workspace)).await {
                Ok(Ok(output)) => ToolOutput::text(output).with_structured_result(json!({})),
                Ok(Err(error)) => {
                    ToolOutput::error(format!("apply_patch verification failed: {error}"))
                }
                Err(error) => ToolOutput::error(format!("apply_patch task failed: {error}")),
            },
        )
    }
}

/// One filesystem mutation produced by the canonical Rust patch engine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PatchOperation {
    /// Create or replace a UTF-8 file.
    Write {
        /// Workspace-relative or absolute patch path.
        path: PathBuf,
        /// Complete replacement contents.
        contents: String,
    },
    /// Delete a file.
    Delete {
        /// Workspace-relative or absolute patch path.
        path: PathBuf,
    },
}

/// Verified filesystem mutations and the normal `apply_patch` success output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatchPlan {
    operations: Vec<PatchOperation>,
    summary: String,
}

impl PatchPlan {
    /// Returns the mutations in execution order.
    #[must_use]
    pub fn operations(&self) -> &[PatchOperation] {
        &self.operations
    }

    /// Returns the canonical model-visible success summary.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// Returns existing UTF-8 files that must be read before planning a patch.
///
/// Files created earlier in the same patch are not returned.
pub fn required_files(patch: &str) -> Result<Vec<PathBuf>, String> {
    let hunks = parse(patch)?;
    Ok(required_files_from_hunks(&hunks))
}

fn required_files_from_hunks(hunks: &[Hunk]) -> Vec<PathBuf> {
    let mut produced = HashSet::new();
    let mut required = Vec::new();
    let mut retained = HashSet::new();
    for hunk in hunks {
        match hunk {
            Hunk::AddFile { path, .. } => {
                produced.insert(path.clone());
            }
            Hunk::DeleteFile { path } => {
                produced.remove(path);
            }
            Hunk::UpdateFile {
                path, move_path, ..
            } => {
                if !produced.contains(path) && retained.insert(path.clone()) {
                    required.push(path.clone());
                }
                produced.remove(path);
                produced.insert(move_path.as_ref().unwrap_or(path).clone());
            }
        }
    }
    required
}

/// Verifies a patch against its required input files and produces mutations.
pub fn plan(patch: &str, initial_files: &HashMap<PathBuf, String>) -> Result<PatchPlan, String> {
    let hunks = parse(patch)?;
    plan_hunks(hunks, initial_files)
}

fn plan_hunks(
    hunks: Vec<Hunk>,
    initial_files: &HashMap<PathBuf, String>,
) -> Result<PatchPlan, String> {
    let mut files = initial_files.clone();
    let mut operations = Vec::new();
    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();

    for hunk in hunks {
        match hunk {
            Hunk::AddFile { path, contents } => {
                files.insert(path.clone(), contents.clone());
                operations.push(PatchOperation::Write {
                    path: path.clone(),
                    contents,
                });
                added.push(path);
            }
            Hunk::DeleteFile { path } => {
                files.remove(&path);
                operations.push(PatchOperation::Delete { path: path.clone() });
                deleted.push(path);
            }
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let original = files
                    .get(&path)
                    .ok_or_else(|| format!("Failed to read file to update {}", path.display()))?;
                let updated = apply_chunks(original, &chunks, &path)?;
                if let Some(destination) = move_path {
                    files.remove(&path);
                    files.insert(destination.clone(), updated.clone());
                    operations.push(PatchOperation::Write {
                        path: destination.clone(),
                        contents: updated,
                    });
                    operations.push(PatchOperation::Delete { path });
                    modified.push(destination);
                } else {
                    files.insert(path.clone(), updated.clone());
                    operations.push(PatchOperation::Write {
                        path: path.clone(),
                        contents: updated,
                    });
                    modified.push(path);
                }
            }
        }
    }

    let mut summary = String::from("Success. Updated the following files:\n");
    for path in &added {
        push_summary_line(&mut summary, 'A', path);
    }
    for path in &modified {
        push_summary_line(&mut summary, 'M', path);
    }
    for path in &deleted {
        push_summary_line(&mut summary, 'D', path);
    }
    Ok(PatchPlan {
        operations,
        summary,
    })
}

#[derive(Debug, PartialEq)]
struct ApplyPatchArgs {
    patch: String,
    hunks: Vec<Hunk>,
    workdir: Option<String>,
    environment_id: Option<String>,
}

#[cfg(not(target_family = "wasm"))]
pub(super) fn apply(patch: &str, workspace: &Path) -> Result<String, String> {
    let hunks = parse(patch)?;
    validate_unique_targets(&hunks, |path| normalize_path(&resolve(workspace, path)))?;

    let mut initial_files = HashMap::new();
    for path in required_files_from_hunks(&hunks) {
        let source = resolve(workspace, &path);
        let contents = std::fs::read_to_string(&source).map_err(|error| {
            format!(
                "Failed to read file to update {}: {error}",
                source.display()
            )
        })?;
        initial_files.insert(path, contents);
    }
    let plan = plan_hunks(hunks, &initial_files)?;
    for operation in plan.operations() {
        match operation {
            PatchOperation::Write { path, contents } => {
                write_file(&resolve(workspace, path), contents.as_bytes())?;
            }
            PatchOperation::Delete { path } => {
                let target = resolve(workspace, path);
                std::fs::remove_file(&target).map_err(|error| {
                    format!("Failed to delete file {}: {error}", target.display())
                })?;
            }
        }
    }
    Ok(plan.summary().to_owned())
}

fn parse(patch: &str) -> Result<Vec<Hunk>, String> {
    let ApplyPatchArgs {
        hunks,
        patch: _,
        workdir: _,
        environment_id: _,
    } = parse_patch(patch).map_err(|error| error.to_string())?;
    if hunks.is_empty() {
        return Err("No files were modified.".to_owned());
    }
    Ok(hunks.into_iter().map(normalize_hunk_paths).collect())
}

#[cfg(not(target_family = "wasm"))]
fn validate_unique_targets(
    hunks: &[Hunk],
    mut resolve: impl FnMut(&Path) -> PathBuf,
) -> Result<(), String> {
    let mut targets = HashSet::new();
    for hunk in hunks {
        let (source, destination) = match hunk {
            Hunk::AddFile { path, .. } | Hunk::DeleteFile { path } => (path, None),
            Hunk::UpdateFile {
                path, move_path, ..
            } => (path, move_path.as_deref()),
        };

        for path in std::iter::once(source.as_path()).chain(destination) {
            let target = resolve(path);
            if !targets.insert(target.clone()) {
                return Err(format!("multiple operations target {}", target.display()));
            }
        }
    }
    Ok(())
}

fn normalize_hunk_paths(hunk: Hunk) -> Hunk {
    match hunk {
        Hunk::AddFile { path, contents } => Hunk::AddFile {
            path: normalize_path(&path),
            contents,
        },
        Hunk::DeleteFile { path } => Hunk::DeleteFile {
            path: normalize_path(&path),
        },
        Hunk::UpdateFile {
            path,
            move_path,
            chunks,
        } => Hunk::UpdateFile {
            path: normalize_path(&path),
            move_path: move_path.map(|path| normalize_path(&path)),
            chunks,
        },
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) =>
            {
                normalized.pop();
            }
            Component::ParentDir if normalized.has_root() => {}
            Component::ParentDir => normalized.push(component.as_os_str()),
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
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

#[cfg(not(target_family = "wasm"))]
fn resolve(workspace: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        workspace.join(path)
    }
}

#[cfg(not(target_family = "wasm"))]
fn write_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create parent directories for {}: {error}",
                path.display()
            )
        })?;
    }
    std::fs::write(path, contents)
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
    use std::{collections::HashMap, path::Path};

    use super::{PatchOperation, apply, plan};

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

    #[test]
    fn plan_preserves_sequential_operations_on_the_same_virtual_path() {
        let patch = "*** Begin Patch\n*** Add File: file.txt\n+first\n*** Update File: file.txt\n@@\n-first\n+second\n*** Delete File: file.txt\n*** End Patch";

        let plan = plan(patch, &HashMap::new()).expect("sequential virtual operations should plan");

        assert_eq!(
            plan.operations(),
            [
                PatchOperation::Write {
                    path: Path::new("file.txt").to_owned(),
                    contents: "first\n".to_owned(),
                },
                PatchOperation::Write {
                    path: Path::new("file.txt").to_owned(),
                    contents: "second\n".to_owned(),
                },
                PatchOperation::Delete {
                    path: Path::new("file.txt").to_owned(),
                },
            ]
        );
    }

    #[test]
    fn rejects_move_to_a_lexical_alias_without_mutating_the_file()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = test_root("aliased-move")?;
        std::fs::write(root.join("file.txt"), "before\n")?;

        let error = apply(
            "*** Begin Patch\n*** Update File: file.txt\n*** Move to: ./folder/../file.txt\n@@\n-before\n+after\n*** End Patch",
            &root,
        )
        .expect_err("an aliased move must fail before mutation");

        assert_eq!(
            error,
            format!(
                "multiple operations target {}",
                root.join("file.txt").display()
            )
        );
        assert_eq!(std::fs::read_to_string(root.join("file.txt"))?, "before\n");
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn preserves_distinct_updated_paths() -> Result<(), Box<dyn std::error::Error>> {
        let root = test_root("distinct-updates")?;
        std::fs::write(root.join("first.txt"), "first before\n")?;
        std::fs::write(root.join("second.txt"), "second before\n")?;

        apply(
            "*** Begin Patch\n*** Update File: first.txt\n@@\n-first before\n+first after\n*** Update File: second.txt\n@@\n-second before\n+second after\n*** End Patch",
            &root,
        )?;

        assert_eq!(
            std::fs::read_to_string(root.join("first.txt"))?,
            "first after\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("second.txt"))?,
            "second after\n"
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn rejects_duplicate_normalized_sources_before_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = test_root("duplicate-sources")?;
        std::fs::write(root.join("duplicate.txt"), "before\n")?;

        let error = apply(
            "*** Begin Patch\n*** Update File: duplicate.txt\n@@\n-before\n+first after\n*** Update File: ./duplicate.txt\n@@\n-before\n+second after\n*** End Patch",
            &root,
        )
        .expect_err("duplicate normalized sources must fail before mutation");

        assert_eq!(
            error,
            format!(
                "multiple operations target {}",
                root.join("duplicate.txt").display()
            )
        );
        assert_eq!(
            std::fs::read_to_string(root.join("duplicate.txt"))?,
            "before\n"
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn rejects_converging_move_destinations_before_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = test_root("converging-moves")?;
        std::fs::write(root.join("first.txt"), "first before\n")?;
        std::fs::write(root.join("second.txt"), "second before\n")?;

        let error = apply(
            "*** Begin Patch\n*** Update File: first.txt\n*** Move to: moved.txt\n@@\n-first before\n+first after\n*** Update File: second.txt\n*** Move to: ./moved.txt\n@@\n-second before\n+second after\n*** End Patch",
            &root,
        )
        .expect_err("converging move destinations must fail before mutation");

        assert_eq!(
            error,
            format!(
                "multiple operations target {}",
                root.join("moved.txt").display()
            )
        );
        assert_eq!(
            std::fs::read_to_string(root.join("first.txt"))?,
            "first before\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("second.txt"))?,
            "second before\n"
        );
        assert!(!root.join("moved.txt").exists());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn rejects_relative_and_absolute_aliases_before_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = test_root("resolved-aliases")?;
        let path = root.join("duplicate.txt");
        std::fs::write(&path, "before\n")?;
        let patch = format!(
            "*** Begin Patch\n*** Update File: duplicate.txt\n@@\n-before\n+first after\n*** Update File: {}\n@@\n-before\n+second after\n*** End Patch",
            path.display()
        );

        let error = apply(&patch, &root)
            .expect_err("relative and absolute aliases must fail before mutation");

        assert_eq!(
            error,
            format!("multiple operations target {}", path.display())
        );
        assert_eq!(std::fs::read_to_string(&path)?, "before\n");
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
